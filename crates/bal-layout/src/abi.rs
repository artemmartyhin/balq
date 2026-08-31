//! `eth_call` on a compiler-generated getter is a storage read. For every
//! `public` state variable Solidity emits a getter with a fixed shape:
//!
//! - a value: `x()` returns it;
//! - a mapping: `x(key)`, nested mappings take one key per level;
//! - an array: `x(index)`; a mapping to an array: `x(key, index)`;
//! - a struct: the members in order, *except* mappings and dynamic arrays.
//!
//! So a call whose selector names a function `x(...)` with exactly that
//! shape, where `x` is a top-level variable of the layout, reads a known
//! path — and the answer can come from the archive, ABI-encoded, without an
//! EVM. Anything else (a view with logic, a mismatched shape) is *not*
//! resolved; the caller falls back to a node. A guess would break the
//! promise that balq never answers with something it does not know.

use crate::{Encoding, Layout, LayoutError, Location, Result, Value};
use alloy_primitives::{keccak256, Address, B256, I256, U256};

/// One `view`/`pure` function of an ABI, as far as a getter needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Getter {
    /// Function name.
    pub name: String,
    /// `keccak256("name(t1,t2)")[..4]`.
    pub selector: [u8; 4],
    /// Canonical input types (`address`, `uint256`, …).
    pub inputs: Vec<String>,
    /// Canonical output types.
    pub outputs: Vec<String>,
}

/// The `view` functions of a contract ABI, by selector.
#[derive(Debug, Clone, Default)]
pub struct Getters(Vec<Getter>);

impl Getters {
    /// From an artifact's `abi` array (forge / hardhat / solc output).
    /// Functions that are not `view`/`pure`, or whose inputs are not
    /// static single-word types, are skipped: they can never be getters.
    pub fn from_abi(abi: &serde_json::Value) -> Self {
        let mut out = Vec::new();
        let Some(items) = abi.as_array() else {
            return Self(out);
        };
        for f in items {
            if f["type"].as_str() != Some("function") {
                continue;
            }
            if !matches!(f["stateMutability"].as_str(), Some("view") | Some("pure")) {
                continue;
            }
            let Some(name) = f["name"].as_str() else {
                continue;
            };
            let types = |k: &str| -> Option<Vec<String>> {
                f[k].as_array()?
                    .iter()
                    .map(|p| p["type"].as_str().map(String::from))
                    .collect()
            };
            let (Some(inputs), Some(outputs)) = (types("inputs"), types("outputs")) else {
                continue;
            };
            if !inputs.iter().all(|t| is_static_word(t)) {
                continue;
            }
            let sig = format!("{name}({})", inputs.join(","));
            let h = keccak256(sig.as_bytes());
            out.push(Getter {
                name: name.to_string(),
                selector: [h[0], h[1], h[2], h[3]],
                inputs,
                outputs,
            });
        }
        Self(out)
    }

    /// From a whole artifact (`{ "abi": [...] , ... }`); empty if it has none.
    pub fn from_artifact_json(v: &serde_json::Value) -> Self {
        v.get("abi").map(Self::from_abi).unwrap_or_default()
    }

    /// The function with this selector, if any.
    pub fn find(&self, selector: &[u8; 4]) -> Option<&Getter> {
        self.0.iter().find(|g| &g.selector == selector)
    }

    /// No `view` functions known.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Number of `view` functions known.
    pub fn len(&self) -> usize {
        self.0.len()
    }
}

/// A call resolved to storage: the path it reads and how to encode the answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedCall {
    /// The getter that matched.
    pub name: String,
    /// Storage path of what it reads (`balances[0x…]`, `totals`).
    pub path: String,
    /// One location per output, in output order (a struct getter has several).
    pub reads: Vec<Location>,
    /// Canonical output types, parallel to `reads`.
    pub outputs: Vec<String>,
}

fn is_static_word(t: &str) -> bool {
    t == "address"
        || t == "bool"
        || (t.starts_with("uint") || t.starts_with("int")) && !t.contains('[')
        || (t.starts_with("bytes") && t.len() > 5 && t[5..].parse::<u8>().is_ok())
}

/// The ABI type a storage type label corresponds to.
fn abi_type_of(label: &str) -> String {
    if label == "address payable" || label.starts_with("contract ") {
        return "address".into();
    }
    if label.starts_with("enum ") {
        return "uint8".into();
    }
    if label == "string storage ref" {
        return "string".into();
    }
    if label == "bytes storage ref" {
        return "bytes".into();
    }
    label.to_string()
}

/// A 32-byte calldata word as the text `Layout::locate` takes for a key or
/// index: the full word in hex, so `bytesN` keys keep their left alignment
/// and integers keep their value.
fn word_text(w: &[u8]) -> String {
    format!("0x{}", alloy_primitives::hex::encode(w))
}

impl Layout {
    /// Resolve `calldata` (selector + ABI-encoded arguments) against the
    /// contract's getters and this layout. `Ok(None)` means "not a getter of
    /// a variable in this layout" — fall back to a node. `Err` only for
    /// malformed calldata.
    pub fn resolve_call(&self, getters: &Getters, calldata: &[u8]) -> Result<Option<ResolvedCall>> {
        if calldata.len() < 4 {
            return Err(LayoutError::Syntax(
                "calldata shorter than a selector".into(),
            ));
        }
        let selector: [u8; 4] = [calldata[0], calldata[1], calldata[2], calldata[3]];
        let Some(g) = getters.find(&selector) else {
            return Ok(None);
        };
        let args = &calldata[4..];
        if args.len() != 32 * g.inputs.len() {
            return Err(LayoutError::Syntax(format!(
                "{}: expected {} argument word(s), got {} bytes",
                g.name,
                g.inputs.len(),
                args.len()
            )));
        }
        // The getter's name must be a top-level variable.
        let Some(top) = self.storage.iter().find(|e| e.label == g.name) else {
            return Ok(None);
        };
        // Walk the type chain: one input per mapping key / array index.
        let mut path = g.name.clone();
        let mut type_id = top.type_id.clone();
        for (i, in_ty) in g.inputs.iter().enumerate() {
            let Ok(t) = self.ty(&type_id) else {
                return Ok(None);
            };
            let word = &args[32 * i..32 * i + 32];
            match t.encoding {
                Encoding::Mapping => {
                    let (Some(kt), Some(vt)) = (t.key.as_deref(), t.value.as_deref()) else {
                        return Ok(None);
                    };
                    let key_abi = self
                        .ty(kt)
                        .map(|k| abi_type_of(&k.label))
                        .unwrap_or_default();
                    if &key_abi != in_ty {
                        return Ok(None);
                    }
                    path = format!("{path}[{}]", word_text(word));
                    type_id = vt.to_string();
                }
                Encoding::DynamicArray | Encoding::Inplace if t.base.is_some() => {
                    if in_ty != "uint256" {
                        return Ok(None);
                    }
                    let idx = U256::from_be_slice(word);
                    path = format!("{path}[{idx}]");
                    type_id = t.base.clone().unwrap_or_default();
                }
                _ => return Ok(None),
            }
        }
        // What is left must be what the outputs describe.
        let Ok(t) = self.ty(&type_id) else {
            return Ok(None);
        };
        let (reads, outputs): (Vec<Location>, Vec<String>) =
            match (t.encoding, t.members.as_deref(), t.base.is_some()) {
                (Encoding::Inplace, Some(members), _) => {
                    // Struct: members in order, minus mappings and dynamic arrays.
                    let mut reads = Vec::new();
                    let mut outs = Vec::new();
                    for m in members {
                        let Ok(mt) = self.ty(&m.type_id) else {
                            return Ok(None);
                        };
                        if mt.encoding == Encoding::Mapping || mt.encoding == Encoding::DynamicArray
                        {
                            continue;
                        }
                        if mt.members.is_some() || mt.base.is_some() {
                            return Ok(None); // nested struct / fixed array: not a plain tuple
                        }
                        let Ok(loc) = self.locate(&format!("{path}.{}", m.label)) else {
                            return Ok(None);
                        };
                        reads.push(loc);
                        outs.push(abi_type_of(&mt.label));
                    }
                    (reads, outs)
                }
                (Encoding::Inplace, None, false) | (Encoding::Bytes, _, _) => {
                    let Ok(loc) = self.locate(&path) else {
                        return Ok(None);
                    };
                    (vec![loc], vec![abi_type_of(&t.label)])
                }
                _ => return Ok(None), // a container needs more inputs than given
            };
        if outputs != g.outputs {
            return Ok(None);
        }
        Ok(Some(ResolvedCall {
            name: g.name.clone(),
            path,
            reads,
            outputs,
        }))
    }
}

/// ABI-encode decoded values as a function's return data. Static types take
/// one word each; `string`/`bytes` are dynamic (offset, length, data).
pub fn encode_return(values: &[Value], types: &[String]) -> Result<Vec<u8>> {
    if values.len() != types.len() {
        return Err(LayoutError::Syntax("values/types length mismatch".into()));
    }
    let n = values.len();
    let mut head: Vec<[u8; 32]> = Vec::with_capacity(n);
    let mut tail: Vec<u8> = Vec::new();
    for (v, t) in values.iter().zip(types) {
        let dynamic = t == "string" || t == "bytes";
        if dynamic {
            let bytes: Vec<u8> = match v {
                Value::Str(s) => s.as_bytes().to_vec(),
                Value::Bytes(b) | Value::FixedBytes(b) => b.clone(),
                _ => return Err(LayoutError::Syntax(format!("{t}: not a byte value"))),
            };
            let offset = U256::from(32 * n + tail.len());
            head.push(offset.to_be_bytes::<32>());
            tail.extend_from_slice(&U256::from(bytes.len()).to_be_bytes::<32>());
            tail.extend_from_slice(&bytes);
            let pad = (32 - bytes.len() % 32) % 32;
            tail.extend(std::iter::repeat_n(0u8, pad));
        } else {
            head.push(static_word(v, t)?);
        }
    }
    let mut out = Vec::with_capacity(32 * n + tail.len());
    for w in head {
        out.extend_from_slice(&w);
    }
    out.extend_from_slice(&tail);
    Ok(out)
}

fn static_word(v: &Value, t: &str) -> Result<[u8; 32]> {
    Ok(match v {
        Value::Uint(u) => u.to_be_bytes::<32>(),
        Value::Int(i) => i.to_be_bytes::<32>(),
        Value::Bool(b) => U256::from(*b as u8).to_be_bytes::<32>(),
        Value::Address(a) => B256::left_padding_from(a.as_slice()).0,
        Value::FixedBytes(b) => {
            let mut w = [0u8; 32];
            let n = b.len().min(32);
            w[..n].copy_from_slice(&b[..n]);
            w
        }
        Value::Raw(w) => {
            if t.starts_with("int") {
                I256::from_raw(U256::from_be_bytes(w.0)).to_be_bytes::<32>()
            } else {
                w.0
            }
        }
        Value::Str(_) | Value::Bytes(_) => {
            return Err(LayoutError::Syntax(format!(
                "{t}: dynamic value in a static slot"
            )))
        }
    })
}

/// `Address` from a 32-byte calldata word (the low 20 bytes).
pub fn word_address(w: &[u8; 32]) -> Address {
    Address::from_slice(&w[12..])
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    const ARTIFACT: &str = include_str!("../tests/fixtures/Playground.artifact.json");

    fn setup() -> (Layout, Getters) {
        let v: serde_json::Value = serde_json::from_str(ARTIFACT).unwrap();
        (
            Layout::from_json(ARTIFACT).unwrap(),
            Getters::from_artifact_json(&v),
        )
    }

    fn call(sig: &str, args: &[[u8; 32]]) -> Vec<u8> {
        let h = keccak256(sig.as_bytes());
        let mut d = h[..4].to_vec();
        for a in args {
            d.extend_from_slice(a);
        }
        d
    }

    #[test]
    fn value_mapping_nested_array_and_struct_getters_resolve() {
        let (l, g) = setup();
        assert_eq!(g.len(), 9);
        let user: Address = "0x35825972e2ca90851b14576C531F13dA0B5d53ce"
            .parse()
            .unwrap();
        let ukey = B256::left_padding_from(user.as_slice()).0;
        let seven = U256::from(7).to_be_bytes::<32>();

        let r = l
            .resolve_call(&g, &call("counter()", &[]))
            .unwrap()
            .unwrap();
        assert_eq!(
            (r.path.as_str(), r.outputs.as_slice()),
            ("counter", &["uint256".to_string()][..])
        );
        assert_eq!(r.reads[0], l.locate("counter").unwrap());

        let r = l
            .resolve_call(&g, &call("balances(address)", &[ukey]))
            .unwrap()
            .unwrap();
        assert_eq!(r.reads[0], l.locate(&format!("balances[{user}]")).unwrap());

        let r = l
            .resolve_call(&g, &call("nested(address,uint256)", &[ukey, seven]))
            .unwrap()
            .unwrap();
        assert_eq!(r.reads[0], l.locate(&format!("nested[{user}][7]")).unwrap());

        let r = l
            .resolve_call(&g, &call("items(uint256)", &[seven]))
            .unwrap()
            .unwrap();
        assert_eq!(r.reads[0], l.locate("items[7]").unwrap());

        // Struct getter: the members as a tuple, packed word shared.
        let r = l.resolve_call(&g, &call("totals()", &[])).unwrap().unwrap();
        assert_eq!(r.outputs, vec!["uint64".to_string(), "uint192".to_string()]);
        assert_eq!(r.reads.len(), 2);
        assert_eq!(r.reads[0], l.locate("totals.lastTime").unwrap());
        assert_eq!(r.reads[1], l.locate("totals.index").unwrap());
    }

    #[test]
    fn non_getters_are_not_resolved() {
        let (l, g) = setup();
        // Unknown selector: not ours.
        assert_eq!(
            l.resolve_call(&g, &call("getReserves()", &[])).unwrap(),
            None
        );
        // Wrong argument count for a known selector: malformed, not guessed.
        assert!(l.resolve_call(&g, &call("balances(address)", &[])).is_err());
        // A view function that exists in the ABI but is not a variable.
        let mut abi: serde_json::Value = serde_json::from_str(ARTIFACT).unwrap();
        abi["abi"].as_array_mut().unwrap().push(serde_json::json!({
            "type": "function", "name": "counterPlusOne", "stateMutability": "view",
            "inputs": [], "outputs": [{ "type": "uint256" }]
        }));
        let g2 = Getters::from_artifact_json(&abi);
        assert_eq!(
            l.resolve_call(&g2, &call("counterPlusOne()", &[])).unwrap(),
            None
        );
        // Same name as a variable but the wrong output type: not resolved.
        abi["abi"].as_array_mut().unwrap().push(serde_json::json!({
            "type": "function", "name": "lastPoker", "stateMutability": "view",
            "inputs": [{ "type": "uint256" }], "outputs": [{ "type": "address" }]
        }));
        let g3 = Getters::from_artifact_json(&abi);
        assert_eq!(
            l.resolve_call(
                &g3,
                &call("lastPoker(uint256)", &[U256::ZERO.to_be_bytes::<32>()])
            )
            .unwrap(),
            None
        );
    }

    #[test]
    fn return_encoding_static_and_dynamic() {
        let user: Address = "0x35825972e2ca90851b14576C531F13dA0B5d53ce"
            .parse()
            .unwrap();
        let out = encode_return(
            &[
                Value::Uint(U256::from(5)),
                Value::Bool(true),
                Value::Address(user),
            ],
            &["uint64".into(), "bool".into(), "address".into()],
        )
        .unwrap();
        assert_eq!(out.len(), 96);
        assert_eq!(out[31], 5);
        assert_eq!(out[63], 1);
        assert_eq!(&out[76..96], user.as_slice());
        // A string: offset 32, length, data padded to a word.
        let out = encode_return(&[Value::Str("hi".into())], &["string".into()]).unwrap();
        assert_eq!(out.len(), 96);
        assert_eq!(out[31], 32);
        assert_eq!(out[63], 2);
        assert_eq!(&out[64..66], b"hi");
        // Negative int keeps its two's complement.
        let out = encode_return(
            &[Value::Int(I256::try_from(-1i64).unwrap())],
            &["int128".into()],
        )
        .unwrap();
        assert!(out.iter().all(|b| *b == 0xff));
    }
}
