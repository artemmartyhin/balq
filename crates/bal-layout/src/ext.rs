//! Beyond one solc layout:
//! - **namespaced storage** (ERC-7201, Diamond): a struct laid out by another
//!   compile mounted at a computed or explicit base slot;
//! - **dynamic `bytes`/`string`**: short values live in their slot, long ones
//!   at `keccak(slot) + i` — the extra slots to read and how to assemble them;
//! - **mapping keys**: a slot can be named when a candidate key is at hand
//!   (`keccak` is one-way, but `keccak(key ‖ base)` is one keccak per guess).

use crate::{Encoding, Layout, LayoutError, Location, Result, StorageEntry, TypeInfo, Value};
use alloy_primitives::{keccak256, Address, B256, U256};
use std::path::Path;

/// A layout manifest: a base layout plus namespaces mounted into it.
///
/// ```json
/// {
///   "base": "out/Vault.sol/Vault.json",
///   "namespaces": [
///     { "prefix": "erc20", "layout": "out/ERC20Storage.sol/ERC20Storage.json",
///       "erc7201": "openzeppelin.storage.ERC20" },
///     { "prefix": "diamond", "layout": "out/AppStorage.sol/AppStorage.json",
///       "slot": "0x…" }
///   ]
/// }
/// ```
///
/// Paths are relative to the manifest. The namespace layout is any solc
/// `storageLayout` whose top-level variables are the struct's members — the
/// usual trick is a one-line contract that declares the struct as its only
/// state variable, or the storage-layout output of the library itself.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    base: Option<String>,
    #[serde(default)]
    namespaces: Vec<Namespace>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Namespace {
    prefix: String,
    layout: String,
    erc7201: Option<String>,
    slot: Option<String>,
}

impl Layout {
    /// ERC-7201 base slot: `keccak256(abi.encode(uint256(keccak256(id)) - 1)) & ~0xff`.
    pub fn erc7201_slot(id: &str) -> B256 {
        let h = U256::from_be_bytes(keccak256(id.as_bytes()).0).wrapping_sub(U256::from(1));
        let mut out = keccak256(h.to_be_bytes::<32>()).0;
        out[31] = 0;
        B256::from(out)
    }

    /// Mount `other`'s top-level variables as members of a struct named
    /// `prefix` that starts at `base`: `prefix.var`, `prefix.map[key]`, …
    /// Types are merged by id (solc ids are canonical, so equal types agree).
    pub fn mount(&mut self, prefix: &str, other: &Layout, base: B256) -> Result<()> {
        if prefix.is_empty() || prefix.contains(['.', '[', ']']) {
            return Err(LayoutError::Syntax(prefix.into()));
        }
        if self.storage.iter().any(|e| e.label == prefix) {
            return Err(LayoutError::Syntax(format!("{prefix}: name already used")));
        }
        for (id, t) in &other.types {
            self.types.entry(id.clone()).or_insert_with(|| t.clone());
        }
        let words = other
            .storage
            .iter()
            .map(|e| {
                let size = other
                    .types
                    .get(&e.type_id)
                    .map(|t| t.number_of_bytes)
                    .unwrap_or(32);
                e.slot + U256::from(size.div_ceil(32))
            })
            .max()
            .unwrap_or(U256::ZERO);
        let type_id = format!("t_struct(namespace {prefix})_storage");
        self.types.insert(
            type_id.clone(),
            TypeInfo {
                encoding: Encoding::Inplace,
                label: format!("struct {prefix}"),
                number_of_bytes: words.saturating_to::<usize>().saturating_mul(32),
                key: None,
                value: None,
                base: None,
                members: Some(other.storage.clone()),
            },
        );
        self.storage.push(StorageEntry {
            label: prefix.to_string(),
            slot: U256::from_be_bytes(base.0),
            offset: 0,
            type_id,
        });
        Ok(())
    }

    /// Build from a manifest file (see [`Manifest`]).
    pub fn from_manifest(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)?;
        let m: Manifest =
            serde_json::from_str(&text).map_err(|e| LayoutError::Json(e.to_string()))?;
        let dir = path.parent().unwrap_or(Path::new("."));
        let mut layout = match &m.base {
            Some(b) => Layout::from_artifact(dir.join(b))?,
            None => Layout::from_json(r#"{"storage":[],"types":{}}"#)?,
        };
        for ns in &m.namespaces {
            let other = Layout::from_artifact(dir.join(&ns.layout))?;
            let base = match (&ns.erc7201, &ns.slot) {
                (Some(id), None) => Layout::erc7201_slot(id),
                (None, Some(s)) => s
                    .parse::<B256>()
                    .map_err(|_| LayoutError::Syntax(format!("{}: slot {s}", ns.prefix)))?,
                _ => {
                    return Err(LayoutError::Syntax(format!(
                        "{}: exactly one of erc7201 / slot",
                        ns.prefix
                    )))
                }
            };
            layout.mount(&ns.prefix, &other, base)?;
        }
        Ok(layout)
    }

    /// Is this location a dynamic `bytes` / `string`?
    pub fn is_dynamic_bytes(&self, loc: &Location) -> bool {
        self.types
            .get(&loc.type_id)
            .is_some_and(|t| t.encoding == Encoding::Bytes)
    }

    /// For a dynamic `bytes`/`string` whose slot holds `word`: the extra
    /// slots holding the data, in order. Empty for a short value (≤ 31
    /// bytes, stored in place) or for anything that is not dynamic bytes.
    pub fn bytes_data_slots(&self, loc: &Location, word: B256) -> Vec<B256> {
        if !self.is_dynamic_bytes(loc) || word.0[31] & 1 == 0 {
            return Vec::new();
        }
        let len = (U256::from_be_bytes(word.0) - U256::from(1)) / U256::from(2);
        // A crafted word could claim a gigantic length; cap what we ask for.
        let words = len
            .div_ceil(U256::from(32))
            .min(U256::from(4096))
            .to::<u64>();
        let data = U256::from_be_bytes(keccak256(loc.slot.as_slice()).0);
        (0..words)
            .map(|i| B256::from(data.wrapping_add(U256::from(i)).to_be_bytes::<32>()))
            .collect()
    }

    /// Assemble a dynamic `bytes`/`string` from its slot word and the data
    /// slots from [`Layout::bytes_data_slots`] (in order). Strings that are
    /// not valid UTF-8 come back as bytes.
    pub fn decode_bytes(&self, loc: &Location, word: B256, chunks: &[B256]) -> Value {
        if !self.is_dynamic_bytes(loc) {
            return self.decode(loc, word);
        }
        let raw: Vec<u8> = if word.0[31] & 1 == 0 {
            let len = (word.0[31] / 2) as usize;
            word.0[..len.min(31)].to_vec()
        } else {
            let len = ((U256::from_be_bytes(word.0) - U256::from(1)) / U256::from(2))
                .min(U256::from(chunks.len() * 32))
                .to::<usize>();
            let mut out = Vec::with_capacity(len);
            for c in chunks {
                out.extend_from_slice(c.as_slice());
            }
            out.truncate(len);
            out
        };
        let is_string = self
            .types
            .get(&loc.type_id)
            .is_some_and(|t| t.label == "string" || t.label == "string storage ref");
        if is_string {
            match String::from_utf8(raw) {
                Ok(s) => Value::Str(s),
                Err(e) => Value::Bytes(e.into_bytes()),
            }
        } else {
            Value::Bytes(raw)
        }
    }

    /// [`Layout::describe_slot`] plus mapping entries whose key is one of
    /// `keys` (each a 32-byte word: an address left-padded, a number, …).
    /// Two mapping levels are tried with the same candidates, and a struct
    /// or array under a mapping is walked as usual.
    pub fn describe_slot_with_keys(
        &self,
        slot: B256,
        array_probe: u64,
        keys: &[B256],
    ) -> Vec<(String, Location)> {
        let mut out = self.describe_slot(slot, array_probe);
        if keys.is_empty() {
            return out;
        }
        let target = U256::from_be_bytes(slot.0);
        for e in &self.storage {
            self.describe_mappings_in(
                &e.label,
                e.slot,
                &e.type_id,
                target,
                array_probe,
                keys,
                0,
                &mut out,
            );
        }
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn describe_mappings_in(
        &self,
        name: &str,
        base: U256,
        type_id: &str,
        target: U256,
        probe: u64,
        keys: &[B256],
        depth: usize,
        out: &mut Vec<(String, Location)>,
    ) {
        if depth > 8 {
            return;
        }
        let Ok(t) = self.ty(type_id) else { return };
        match (t.encoding, t.members.as_deref()) {
            (Encoding::Inplace, Some(members)) => {
                for m in members {
                    self.describe_mappings_in(
                        &format!("{name}.{}", m.label),
                        base + m.slot,
                        &m.type_id,
                        target,
                        probe,
                        keys,
                        depth + 1,
                        out,
                    );
                }
            }
            (Encoding::Mapping, _) => {
                let (Some(kt), Some(vt)) = (t.key.as_deref(), t.value.as_deref()) else {
                    return;
                };
                let key_label = self.ty(kt).map(|k| k.label.clone()).unwrap_or_default();
                for key in keys {
                    let mut buf = [0u8; 64];
                    buf[..32].copy_from_slice(key.as_slice());
                    buf[32..].copy_from_slice(&base.to_be_bytes::<32>());
                    let s1 = U256::from_be_bytes(keccak256(buf).0);
                    let entry = format!("{name}[{}]", show_key(key, &key_label));
                    let Ok(v) = self.ty(vt) else { continue };
                    match (v.encoding, v.members.is_some(), v.base.is_some()) {
                        (Encoding::Mapping, _, _) => {
                            self.describe_mappings_in(
                                &entry,
                                s1,
                                vt,
                                target,
                                probe,
                                keys,
                                depth + 1,
                                out,
                            );
                        }
                        (Encoding::Inplace, false, false) | (Encoding::Bytes, _, _) => {
                            if s1 == target {
                                out.push((
                                    entry,
                                    Location {
                                        slot: slot_b(target),
                                        offset: 0,
                                        size: v.number_of_bytes,
                                        type_id: vt.to_string(),
                                    },
                                ));
                            }
                        }
                        _ => {
                            // struct / array under the mapping: walk it
                            self.describe_in(&entry, s1, 0, vt, target, probe, out);
                            self.describe_mappings_in(
                                &entry,
                                s1,
                                vt,
                                target,
                                probe,
                                keys,
                                depth + 1,
                                out,
                            );
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

fn slot_b(u: U256) -> B256 {
    B256::from(u.to_be_bytes::<32>())
}

/// How a candidate key reads in a path: addresses checksummed, everything
/// else decimal.
fn show_key(key: &B256, key_label: &str) -> String {
    if key_label.starts_with("address") || key_label.starts_with("contract ") {
        Address::from_slice(&key.0[12..]).to_string()
    } else {
        U256::from_be_bytes(key.0).to_string()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    const FIXTURE: &str = include_str!("../tests/fixtures/Playground.layout.json");

    #[test]
    fn erc7201_matches_openzeppelin() {
        // OpenZeppelin ERC20Upgradeable: ERC20StorageLocation.
        let s = Layout::erc7201_slot("openzeppelin.storage.ERC20");
        assert_eq!(
            format!("{s}"),
            "0x52c63247e1f47db19d5ce0460030c497f067ca4cebf71ba98eeadabe20bace00"
        );
        assert_eq!(s.0[31], 0, "low byte cleared");
    }

    #[test]
    fn mount_namespace_and_read_through_it() {
        let mut base = Layout::from_json(r#"{"storage":[],"types":{}}"#).unwrap();
        let ns = Layout::from_json(FIXTURE).unwrap();
        let at = Layout::erc7201_slot("test.playground");
        base.mount("pg", &ns, at).unwrap();
        let counter = base.locate("pg.counter").unwrap();
        assert_eq!(counter.slot, at);
        let bal = base
            .locate("pg.balances[0x35825972e2ca90851b14576C531F13dA0B5d53ce]")
            .unwrap();
        assert_ne!(bal.slot, at);
        // The same slot names back through the namespace.
        let names = base.describe_slot(counter.slot, 16);
        assert!(names.iter().any(|(n, _)| n == "pg.counter"), "{names:?}");
        assert!(
            base.mount("pg", &ns, at).is_err(),
            "duplicate prefix refused"
        );
        assert!(base.mount("a.b", &ns, at).is_err(), "dots refused");
        assert!(base.typescript("V").contains("readonly pg: {"));
    }

    #[test]
    fn mapping_slots_are_named_from_candidate_keys() {
        let l = Layout::from_json(FIXTURE).unwrap();
        let user: Address = "0x35825972e2ca90851b14576C531F13dA0B5d53ce"
            .parse()
            .unwrap();
        let key = B256::left_padding_from(user.as_slice());
        let loc = l.locate(&format!("balances[{user}]")).unwrap();
        assert!(
            l.describe_slot(loc.slot, 16).is_empty(),
            "one-way without a key"
        );
        let named = l.describe_slot_with_keys(loc.slot, 16, &[key]);
        assert_eq!(named.len(), 1);
        assert_eq!(named[0].0, format!("balances[{user}]"));
        // A wrong candidate names nothing.
        let other = B256::left_padding_from(Address::repeat_byte(9).as_slice());
        assert!(l.describe_slot_with_keys(loc.slot, 16, &[other]).is_empty());
    }

    #[test]
    fn dynamic_bytes_short_and_long() {
        // A minimal layout with one `string`.
        let l = Layout::from_json(
            r#"{"storage":[{"label":"name","slot":"0","offset":0,"type":"t_string_storage"}],
                "types":{"t_string_storage":{"encoding":"bytes","label":"string","numberOfBytes":"32"}}}"#,
        )
        .unwrap();
        let loc = l.locate("name").unwrap();
        assert!(l.is_dynamic_bytes(&loc));
        // Short: "hi" → data left-aligned, last byte = len*2.
        let mut w = [0u8; 32];
        w[..2].copy_from_slice(b"hi");
        w[31] = 4;
        let word = B256::from(w);
        assert!(l.bytes_data_slots(&loc, word).is_empty());
        assert_eq!(l.decode_bytes(&loc, word, &[]), Value::Str("hi".into()));
        // Long: 40 bytes → word = len*2+1, data in keccak(slot)+0..1.
        let text = "0123456789012345678901234567890123456789";
        let lenw = B256::from(U256::from(text.len() * 2 + 1).to_be_bytes::<32>());
        let slots = l.bytes_data_slots(&loc, lenw);
        assert_eq!(slots.len(), 2);
        let data = U256::from_be_bytes(keccak256(loc.slot.as_slice()).0);
        assert_eq!(slots[0], B256::from(data.to_be_bytes::<32>()));
        let mut c0 = [0u8; 32];
        c0.copy_from_slice(&text.as_bytes()[..32]);
        let mut c1 = [0u8; 32];
        c1[..8].copy_from_slice(&text.as_bytes()[32..]);
        assert_eq!(
            l.decode_bytes(&loc, lenw, &[B256::from(c0), B256::from(c1)]),
            Value::Str(text.into())
        );
    }
}
