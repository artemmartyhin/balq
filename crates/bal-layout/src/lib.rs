//! `bal-layout`: solc `storageLayout` → slot arithmetic and typed decoding.
//!
//! Knows nothing about where words come from. Give it a path like
//! `balances[0xabc…]`, `totals.index`, `nested[0xabc…][7]`, `items[2]`,
//! `items.length`, and it answers with a [`Location`] (slot, byte offset,
//! size, type). Give it a 32-byte word and a location, and it decodes a
//! [`Value`]. The reverse direction — "which field is slot X?" — works for
//! everything except mapping entries (keccak is one-way; see
//! [`Layout::describe_slot`]).

use alloy_primitives::{keccak256, Address, B256, I256, U256};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

/// Layout parsing and path resolution failures.
#[derive(Debug, thiserror::Error)]
pub enum LayoutError {
    /// The layout JSON did not parse.
    #[error("json: {0}")]
    Json(String),
    /// The artifact file could not be read.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// Neither a bare layout nor an artifact with a `storageLayout` key.
    #[error("artifact has no storageLayout (compile with `extra_output = [\"storageLayout\"]` / outputSelection)")]
    NoStorageLayout,
    /// No such top-level variable or struct member.
    #[error("unknown field `{0}`")]
    UnknownField(String),
    /// The layout references a type id it does not define.
    #[error("unknown type id `{0}` in layout")]
    UnknownType(String),
    /// The path continues past something that cannot be indexed that way.
    #[error("`{path}` is a {what}; expected {expected}")]
    Shape {
        /// Path resolved so far.
        path: String,
        /// What was found there.
        what: &'static str,
        /// What the next segment would have to be.
        expected: &'static str,
    },
    /// A mapping key or array index did not parse.
    #[error("bad key `{0}` for {1}")]
    BadKey(String, String),
    /// The path string is malformed.
    #[error("bad path syntax near `{0}`")]
    Syntax(String),
    /// `string`/`bytes` mapping keys are hashed differently and not handled yet.
    #[error("mapping keys of type {0} (dynamic) are not supported yet")]
    DynamicKey(String),
}

/// Result of layout operations.
pub type Result<T> = std::result::Result<T, LayoutError>;

/// Deepest struct/array nesting followed. Real layouts are a few levels;
/// a self-referential type in a crafted file would otherwise recurse forever.
const MAX_NESTING: usize = 32;

/// One variable (or struct member) as solc reports it.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageEntry {
    /// Variable name.
    pub label: String,
    /// Slot number (relative to the struct start for members).
    #[serde(deserialize_with = "de_u256_str")]
    pub slot: U256,
    /// Byte offset from the low-order end of the word.
    pub offset: usize,
    /// Type id, resolved through [`TypeInfo`].
    #[serde(rename = "type")]
    pub type_id: String,
}

/// One entry of the layout's `types` table.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeInfo {
    /// How values of this type are laid out.
    pub encoding: Encoding,
    /// Solidity type name, e.g. `uint128`, `struct Foo.Bar`.
    pub label: String,
    /// Size in bytes (for arrays: of the whole array).
    #[serde(deserialize_with = "de_usize_str")]
    pub number_of_bytes: usize,
    /// Mapping key type id.
    pub key: Option<String>,
    /// Mapping value type id.
    pub value: Option<String>,
    /// Array element type id.
    pub base: Option<String>,
    /// Struct members.
    pub members: Option<Vec<StorageEntry>>,
}

/// solc storage encodings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Encoding {
    /// Value stored in place (also structs and fixed arrays).
    Inplace,
    /// `keccak(key || slot)`.
    Mapping,
    /// Length in place, data at `keccak(slot)`.
    DynamicArray,
    /// `bytes`/`string`: short in place, long at `keccak(slot)`.
    Bytes,
}

#[derive(Debug, Clone, Deserialize)]
struct RawLayout {
    storage: Vec<StorageEntry>,
    types: HashMap<String, TypeInfo>,
}

fn de_u256_str<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<U256, D::Error> {
    let s = String::deserialize(d)?;
    s.parse::<U256>().map_err(serde::de::Error::custom)
}

fn de_usize_str<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<usize, D::Error> {
    let s = String::deserialize(d)?;
    s.parse::<usize>().map_err(serde::de::Error::custom)
}

/// Where a value lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    /// Storage slot.
    pub slot: B256,
    /// Byte offset from the low-order end of the word (solc convention).
    pub offset: usize,
    /// Size in bytes.
    pub size: usize,
    /// Type id for decoding.
    pub type_id: String,
}

/// A decoded value. `Raw` means the layout knew the location but not how to
/// read it (dynamic bytes/strings, unknown encodings): the word is shown as
/// is rather than guessed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// `uintN`, enums.
    Uint(U256),
    /// `intN`, sign-extended.
    Int(I256),
    /// `bool`.
    Bool(bool),
    /// `address`, contract types.
    Address(Address),
    /// `bytesN`.
    FixedBytes(Vec<u8>),
    /// Not decodable from a single word with this type; the full word.
    Raw(B256),
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Uint(u) => write!(f, "{u}"),
            Value::Int(i) => write!(f, "{i}"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Address(a) => write!(f, "{a}"),
            Value::FixedBytes(b) => write!(f, "0x{}", alloy_primitives::hex::encode(b)),
            Value::Raw(w) => write!(f, "{w}"),
        }
    }
}

/// How a decoded value should be read by a caller that only sees text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueKind {
    /// Unsigned integer or enum; decimal text.
    Uint,
    /// Signed integer; decimal text, possibly negative.
    Int,
    /// `true` / `false`.
    Bool,
    /// Checksummed `0x` address.
    Address,
    /// `bytesN`; `0x` hex.
    Bytes,
    /// Not decodable from one word; the whole word as `0x` hex.
    Raw,
}

/// What a storage path names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathKind {
    /// A leaf that can be read.
    Value(ValueKind),
    /// A struct: continue with `.member`.
    Struct,
    /// A mapping: continue with `[key]`.
    Mapping,
    /// A dynamic array: continue with `[index]` or `.length`.
    Array,
    /// A fixed-size array: continue with `[index]`.
    FixedArray,
}

fn value_kind(label: &str) -> ValueKind {
    if label == "bool" {
        ValueKind::Bool
    } else if label == "address" || label == "address payable" || label.starts_with("contract ") {
        ValueKind::Address
    } else if label.starts_with("uint") || label.starts_with("enum ") {
        ValueKind::Uint
    } else if label.starts_with("int") {
        ValueKind::Int
    } else if label.starts_with("bytes") {
        ValueKind::Bytes
    } else {
        ValueKind::Raw
    }
}

/// A parsed storage layout.
pub struct Layout {
    storage: Vec<StorageEntry>,
    types: HashMap<String, TypeInfo>,
}

#[derive(Debug)]
enum Seg {
    Field(String),
    Index(String),
}

fn parse_path(path: &str) -> Result<Vec<Seg>> {
    let mut out = Vec::new();
    let mut rest = path.trim();
    if rest.is_empty() {
        return Err(LayoutError::Syntax(path.into()));
    }
    let mut first = true;
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix('[') {
            let end = r
                .find(']')
                .ok_or_else(|| LayoutError::Syntax(rest.into()))?;
            out.push(Seg::Index(r[..end].trim().to_string()));
            rest = &r[end + 1..];
        } else {
            let r = if first {
                rest
            } else {
                rest.strip_prefix('.')
                    .ok_or_else(|| LayoutError::Syntax(rest.into()))?
            };
            let end = r.find(['.', '[']).unwrap_or(r.len());
            if end == 0 {
                return Err(LayoutError::Syntax(rest.into()));
            }
            out.push(Seg::Field(r[..end].to_string()));
            rest = &r[end..];
        }
        first = false;
    }
    Ok(out)
}

/// Parse a mapping key or array index: decimal, `0x` hex, `true`/`false`,
/// or a negative decimal (two's complement).
fn parse_key(key: &str, key_type: &str) -> Result<B256> {
    let k = key.trim();
    let bad = || LayoutError::BadKey(key.into(), key_type.into());
    let u = if let Some(h) = k.strip_prefix("0x") {
        U256::from_str_radix(h, 16).map_err(|_| bad())?
    } else if k == "true" {
        U256::from(1)
    } else if k == "false" {
        U256::ZERO
    } else if let Some(n) = k.strip_prefix('-') {
        let n: U256 = n.parse().map_err(|_| bad())?;
        U256::ZERO.wrapping_sub(n)
    } else {
        k.parse::<U256>().map_err(|_| bad())?
    };
    Ok(B256::from(u.to_be_bytes::<32>()))
}

fn slot_b(u: U256) -> B256 {
    B256::from(u.to_be_bytes::<32>())
}

impl Layout {
    /// Accepts either a bare `storageLayout` object or a whole forge/hardhat
    /// artifact that contains one.
    pub fn from_json(s: &str) -> Result<Self> {
        let v: serde_json::Value =
            serde_json::from_str(s).map_err(|e| LayoutError::Json(e.to_string()))?;
        let raw = if v.get("storage").is_some() {
            v
        } else if let Some(l) = v.get("storageLayout") {
            l.clone()
        } else {
            return Err(LayoutError::NoStorageLayout);
        };
        let raw: RawLayout =
            serde_json::from_value(raw).map_err(|e| LayoutError::Json(e.to_string()))?;
        Ok(Self {
            storage: raw.storage,
            types: raw.types,
        })
    }

    /// [`Layout::from_json`] on the contents of `path`.
    pub fn from_artifact(path: impl AsRef<Path>) -> Result<Self> {
        Self::from_json(&std::fs::read_to_string(path)?)
    }

    /// Top-level variables in declaration order.
    pub fn fields(&self) -> impl Iterator<Item = &StorageEntry> {
        self.storage.iter()
    }

    fn ty(&self, id: &str) -> Result<&TypeInfo> {
        self.types
            .get(id)
            .ok_or_else(|| LayoutError::UnknownType(id.into()))
    }

    /// Resolve a dotted/indexed path to its storage location.
    pub fn locate(&self, path: &str) -> Result<Location> {
        let segs = parse_path(path)?;
        let Some(Seg::Field(name)) = segs.first() else {
            return Err(LayoutError::Syntax(path.into()));
        };
        let top = self
            .storage
            .iter()
            .find(|e| &e.label == name)
            .ok_or_else(|| LayoutError::UnknownField(name.clone()))?;
        let mut slot = top.slot;
        let mut offset = top.offset;
        let mut type_id = top.type_id.clone();
        let mut walked = name.clone();

        for seg in &segs[1..] {
            let t = self.ty(&type_id)?;
            match (t.encoding, seg, t.base.as_deref(), t.members.as_deref()) {
                (Encoding::Mapping, Seg::Index(k), _, _) => {
                    let key_ty = t
                        .key
                        .as_deref()
                        .ok_or_else(|| LayoutError::UnknownType(type_id.clone()))?;
                    let kt = self.ty(key_ty)?;
                    if kt.encoding == Encoding::Bytes {
                        return Err(LayoutError::DynamicKey(kt.label.clone()));
                    }
                    let key = parse_key(k, &kt.label)?;
                    let mut buf = [0u8; 64];
                    buf[..32].copy_from_slice(key.as_slice());
                    buf[32..].copy_from_slice(&slot.to_be_bytes::<32>());
                    slot = U256::from_be_bytes(keccak256(buf).0);
                    offset = 0;
                    type_id = t
                        .value
                        .clone()
                        .ok_or_else(|| LayoutError::UnknownType(type_id.clone()))?;
                    walked = format!("{walked}[{k}]");
                }
                (Encoding::DynamicArray, Seg::Field(m), _, _) if m == "length" => {
                    return Ok(Location {
                        slot: slot_b(slot),
                        offset: 0,
                        size: 32,
                        type_id: "t_uint256".into(),
                    });
                }
                (Encoding::DynamicArray, Seg::Index(i), Some(base_ty), _) => {
                    let idx = U256::from_be_bytes(parse_key(i, "index")?.0);
                    let data = U256::from_be_bytes(keccak256(slot.to_be_bytes::<32>()).0);
                    let (s, o) = self.element_at(base_ty, data, idx)?;
                    slot = s;
                    offset = o;
                    type_id = base_ty.to_string();
                    walked = format!("{walked}[{i}]");
                }
                (Encoding::Inplace, Seg::Index(i), Some(base_ty), _) => {
                    // fixed-size array, in place
                    let idx = U256::from_be_bytes(parse_key(i, "index")?.0);
                    let (s, o) = self.element_at(base_ty, slot, idx)?;
                    slot = s;
                    offset = o;
                    type_id = base_ty.to_string();
                    walked = format!("{walked}[{i}]");
                }
                (Encoding::Inplace, Seg::Field(m), _, Some(members)) => {
                    let member = members
                        .iter()
                        .find(|e| &e.label == m)
                        .ok_or_else(|| LayoutError::UnknownField(format!("{walked}.{m}")))?;
                    slot += member.slot;
                    offset = member.offset;
                    type_id = member.type_id.clone();
                    walked = format!("{walked}.{m}");
                }
                (enc, _, _, _) => {
                    let (what, expected) = match enc {
                        Encoding::Mapping => ("mapping", "[key]"),
                        Encoding::DynamicArray => ("dynamic array", "[index] or .length"),
                        Encoding::Bytes => ("bytes/string", "no further path"),
                        Encoding::Inplace => ("value", "no further path"),
                    };
                    return Err(LayoutError::Shape {
                        path: walked,
                        what,
                        expected,
                    });
                }
            }
        }
        let size = self.ty(&type_id)?.number_of_bytes;
        Ok(Location {
            slot: slot_b(slot),
            offset,
            size,
            type_id,
        })
    }

    /// Slot and offset of element `idx` of an array whose data starts at `data`.
    fn element_at(&self, base_ty: &str, data: U256, idx: U256) -> Result<(U256, usize)> {
        let size = self.ty(base_ty)?.number_of_bytes;
        if size == 0 {
            return Err(LayoutError::UnknownType(base_ty.into()));
        }
        // Slot arithmetic is modulo 2^256, like the EVM's; an absurd index
        // wraps instead of panicking.
        if size >= 32 {
            let per_elem = U256::from(size.div_ceil(32));
            Ok((data.wrapping_add(idx.wrapping_mul(per_elem)), 0))
        } else {
            let per_slot = U256::from(32 / size);
            let slot = data.wrapping_add(idx / per_slot);
            let offset = (idx % per_slot).to::<usize>() * size;
            Ok((slot, offset))
        }
    }

    /// Decode the value at `loc` out of a full 32-byte storage word.
    pub fn decode(&self, loc: &Location, word: B256) -> Value {
        let size = loc.size.clamp(1, 32);
        let end = 32usize.saturating_sub(loc.offset).max(1);
        let start = end.saturating_sub(size);
        let bytes = &word.0[start..end];
        let Some(t) = self.types.get(&loc.type_id) else {
            return Value::Raw(word);
        };
        let label = t.label.as_str();
        if t.encoding != Encoding::Inplace || t.members.is_some() || t.base.is_some() {
            return Value::Raw(word);
        }
        if label == "bool" {
            return Value::Bool(bytes.iter().any(|b| *b != 0));
        }
        if label == "address" || label == "address payable" || label.starts_with("contract ") {
            let n = bytes.len();
            return Value::Address(Address::from_slice(&bytes[n.saturating_sub(20)..]));
        }
        if label.starts_with("uint") || label.starts_with("enum ") {
            return Value::Uint(U256::from_be_slice(bytes));
        }
        if label.starts_with("int") {
            let fill = if bytes[0] & 0x80 != 0 { 0xFF } else { 0x00 };
            let mut w = [fill; 32];
            w[32 - bytes.len()..].copy_from_slice(bytes);
            return Value::Int(I256::from_be_bytes(w));
        }
        if label.starts_with("bytes") && size <= 32 {
            // bytesN are left-aligned inside their `size` bytes
            return Value::FixedBytes(bytes.to_vec());
        }
        Value::Raw(word)
    }

    /// What kind of thing a path names — a leaf value, or a container that
    /// takes another path segment. Drives the Node `view` proxy.
    pub fn kind_of(&self, path: &str) -> Result<PathKind> {
        let loc = self.locate(path)?;
        if path.trim_end().ends_with(".length") {
            return Ok(PathKind::Value(ValueKind::Uint));
        }
        let t = self.ty(&loc.type_id)?;
        Ok(match (t.encoding, t.members.is_some(), t.base.is_some()) {
            (Encoding::Mapping, _, _) => PathKind::Mapping,
            (Encoding::DynamicArray, _, _) => PathKind::Array,
            (Encoding::Inplace, true, _) => PathKind::Struct,
            (Encoding::Inplace, false, true) => PathKind::FixedArray,
            (Encoding::Bytes, _, _) => PathKind::Value(ValueKind::Raw),
            (Encoding::Inplace, false, false) => PathKind::Value(value_kind(&t.label)),
        })
    }

    /// Decode with an explicit kind, so callers that marshal across a
    /// language boundary know what the text is.
    pub fn decode_typed(&self, loc: &Location, word: B256) -> (ValueKind, Value) {
        let v = self.decode(loc, word);
        let k = match &v {
            Value::Uint(_) => ValueKind::Uint,
            Value::Int(_) => ValueKind::Int,
            Value::Bool(_) => ValueKind::Bool,
            Value::Address(_) => ValueKind::Address,
            Value::FixedBytes(_) => ValueKind::Bytes,
            Value::Raw(_) => ValueKind::Raw,
        };
        (k, v)
    }

    /// TypeScript declaration of the contract's storage as the Node `view`
    /// exposes it: `bigint` for integers, `boolean`, `string` for addresses
    /// and bytes, nested objects for structs, index signatures for mappings
    /// and arrays. `name` is the interface name.
    pub fn typescript(&self, name: &str) -> String {
        let mut out = String::new();
        out.push_str("// Generated by balq from a solc storageLayout. Do not edit.\n");
        out.push_str(&format!("export interface {name} {{\n"));
        for e in &self.storage {
            out.push_str(&format!(
                "  readonly {}: {};\n",
                e.label,
                self.ts_type(&e.type_id, 1)
            ));
        }
        out.push_str("}\n");
        out
    }

    fn ts_type(&self, type_id: &str, depth: usize) -> String {
        // A crafted layout can reference itself; solc never emits that, but a
        // file is not solc. Stop instead of overflowing the stack.
        if depth > MAX_NESTING {
            return "unknown".into();
        }
        let Ok(t) = self.ty(type_id) else {
            return "string".into();
        };
        let pad = "  ".repeat(depth + 1);
        let close = "  ".repeat(depth);
        match (t.encoding, t.members.as_deref(), t.base.as_deref()) {
            (Encoding::Mapping, _, _) => {
                let v = t
                    .value
                    .as_deref()
                    .map(|v| self.ts_type(v, depth))
                    .unwrap_or_else(|| "string".into());
                format!("{{ readonly [key: string]: {v} }}")
            }
            (Encoding::DynamicArray, _, base) => {
                let v = base
                    .map(|b| self.ts_type(b, depth))
                    .unwrap_or_else(|| "string".into());
                format!("{{ readonly [index: number]: {v}; readonly length: bigint }}")
            }
            (Encoding::Inplace, Some(members), _) => {
                let mut s = String::from("{\n");
                for m in members {
                    s.push_str(&format!(
                        "{pad}readonly {}: {};\n",
                        m.label,
                        self.ts_type(&m.type_id, depth + 1)
                    ));
                }
                s.push_str(&format!("{close}}}"));
                s
            }
            (Encoding::Inplace, None, Some(base)) => {
                format!(
                    "{{ readonly [index: number]: {} }}",
                    self.ts_type(base, depth)
                )
            }
            (Encoding::Bytes, _, _) => "string".into(),
            (Encoding::Inplace, None, None) => match value_kind(&t.label) {
                ValueKind::Uint | ValueKind::Int => "bigint".into(),
                ValueKind::Bool => "boolean".into(),
                ValueKind::Address | ValueKind::Bytes | ValueKind::Raw => "string".into(),
            },
        }
    }

    /// Human name(s) for a raw slot: top-level values, struct members, and
    /// dynamic-array elements within `array_probe` slots of the data start.
    /// Mapping entries cannot be named without a candidate key.
    pub fn describe_slot(&self, slot: B256, array_probe: u64) -> Vec<(String, Location)> {
        let target = U256::from_be_bytes(slot.0);
        let mut out = Vec::new();
        for e in &self.storage {
            self.describe_in(
                &e.label,
                e.slot,
                e.offset,
                &e.type_id,
                target,
                array_probe,
                &mut out,
            );
        }
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn describe_in(
        &self,
        name: &str,
        base: U256,
        offset: usize,
        type_id: &str,
        target: U256,
        probe: u64,
        out: &mut Vec<(String, Location)>,
    ) {
        // Recursion depth is bounded by the path length being built.
        if name.matches(['.', '[']).count() > MAX_NESTING {
            return;
        }
        let Ok(t) = self.ty(type_id) else { return };
        match (t.encoding, t.members.as_deref(), t.base.as_deref()) {
            (Encoding::Inplace, Some(members), _) => {
                for m in members {
                    self.describe_in(
                        &format!("{name}.{}", m.label),
                        base + m.slot,
                        m.offset,
                        &m.type_id,
                        target,
                        probe,
                        out,
                    );
                }
            }
            (Encoding::Inplace, None, Some(base_ty)) => {
                let words = U256::from(t.number_of_bytes.div_ceil(32));
                if target < base || target >= base + words {
                    return;
                }
                self.describe_elements(name, base, base_ty, target, probe, out);
            }
            (Encoding::Inplace, None, None) | (Encoding::Bytes, _, _) => {
                let words = U256::from(t.number_of_bytes.div_ceil(32).max(1));
                if target >= base && target < base + words {
                    out.push((
                        name.to_string(),
                        Location {
                            slot: slot_b(target),
                            offset,
                            size: t.number_of_bytes,
                            type_id: type_id.to_string(),
                        },
                    ));
                }
            }
            (Encoding::DynamicArray, _, base_ty) => {
                if target == base {
                    out.push((
                        format!("{name}.length"),
                        Location {
                            slot: slot_b(base),
                            offset: 0,
                            size: 32,
                            type_id: "t_uint256".into(),
                        },
                    ));
                    return;
                }
                let data = U256::from_be_bytes(keccak256(base.to_be_bytes::<32>()).0);
                if target < data || target - data >= U256::from(probe) {
                    return;
                }
                let Some(base_ty) = base_ty else { return };
                self.describe_elements(name, data, base_ty, target, probe, out);
            }
            (Encoding::Mapping, _, _) => {}
        }
    }

    /// Name the element(s) of an array whose data starts at `data` that live
    /// in slot `target`. Multi-word elements (structs, nested arrays) recurse
    /// so their members get named too.
    fn describe_elements(
        &self,
        name: &str,
        data: U256,
        base_ty: &str,
        target: U256,
        probe: u64,
        out: &mut Vec<(String, Location)>,
    ) {
        let Ok(bt) = self.ty(base_ty) else { return };
        let size = bt.number_of_bytes;
        if size >= 32 {
            let per_elem = size.div_ceil(32) as u64;
            let i = (target - data).to::<u64>() / per_elem;
            self.describe_in(
                &format!("{name}[{i}]"),
                data + U256::from(i * per_elem),
                0,
                base_ty,
                target,
                probe,
                out,
            );
        } else {
            let per = (32 / size) as u64;
            let first = (target - data).to::<u64>() * per;
            for i in first..first + per {
                out.push((
                    format!("{name}[{i}]"),
                    Location {
                        slot: slot_b(target),
                        offset: ((i % per) as usize) * size,
                        size,
                        type_id: base_ty.to_string(),
                    },
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAYGROUND: &str = include_str!("../tests/fixtures/Playground.layout.json");

    fn layout() -> Layout {
        Layout::from_json(PLAYGROUND).unwrap()
    }

    fn s(n: u64) -> B256 {
        slot_b(U256::from(n))
    }

    #[test]
    fn flat_and_packed() {
        let l = layout();
        assert_eq!(
            l.locate("counter").unwrap(),
            Location {
                slot: s(0),
                offset: 0,
                size: 32,
                type_id: "t_uint256".into()
            }
        );
        let b = l.locate("b").unwrap();
        assert_eq!((b.slot, b.offset, b.size), (s(1), 16, 8));
        let c = l.locate("c").unwrap();
        assert_eq!((c.slot, c.offset, c.size), (s(1), 24, 1));
        // word = a=5 | b=7<<128 | c=1<<192
        let word = slot_b(U256::from(5) | (U256::from(7) << 128) | (U256::from(1) << 192));
        assert_eq!(
            l.decode(&l.locate("a").unwrap(), word),
            Value::Uint(U256::from(5))
        );
        assert_eq!(l.decode(&b, word), Value::Uint(U256::from(7)));
        assert_eq!(l.decode(&c, word), Value::Bool(true));
    }

    #[test]
    fn struct_members() {
        let l = layout();
        let idx = l.locate("totals.index").unwrap();
        assert_eq!((idx.slot, idx.offset, idx.size), (s(2), 8, 24));
        assert!(matches!(
            l.locate("totals.nope"),
            Err(LayoutError::UnknownField(_))
        ));
    }

    #[test]
    fn mappings() {
        let l = layout();
        let addr = "0x000000000000000000000000000000000000dEaD";
        let loc = l.locate(&format!("balances[{addr}]")).unwrap();
        let mut buf = [0u8; 64];
        buf[12..32].copy_from_slice(&alloy_primitives::hex::decode(&addr[2..]).unwrap());
        buf[63] = 3;
        assert_eq!(loc.slot, keccak256(buf));
        let inner = l.locate(&format!("nested[{addr}][7]")).unwrap();
        buf[63] = 4;
        let first = keccak256(buf);
        let mut buf2 = [0u8; 64];
        buf2[31] = 7;
        buf2[32..].copy_from_slice(first.as_slice());
        assert_eq!(inner.slot, keccak256(buf2));
        assert!(matches!(
            l.locate("balances.x"),
            Err(LayoutError::Shape { .. })
        ));
    }

    #[test]
    fn dynamic_array_and_describe() {
        let l = layout();
        assert_eq!(l.locate("items.length").unwrap().slot, s(5));
        let data = U256::from_be_bytes(keccak256(U256::from(5).to_be_bytes::<32>()).0);
        assert_eq!(
            l.locate("items[2]").unwrap().slot,
            slot_b(data + U256::from(2))
        );

        let names = |slot: B256| -> Vec<String> {
            l.describe_slot(slot, 64)
                .into_iter()
                .map(|(n, _)| n)
                .collect()
        };
        assert_eq!(names(s(1)), vec!["a", "b", "c"]);
        assert_eq!(names(s(2)), vec!["totals.lastTime", "totals.index"]);
        assert_eq!(names(slot_b(data + U256::from(3))), vec!["items[3]"]);
        assert!(names(s(99)).is_empty());
    }

    #[test]
    fn kinds_and_typescript() {
        let l = layout();
        assert_eq!(
            l.kind_of("counter").unwrap(),
            PathKind::Value(ValueKind::Uint)
        );
        assert_eq!(l.kind_of("c").unwrap(), PathKind::Value(ValueKind::Bool));
        assert_eq!(
            l.kind_of("lastPoker").unwrap(),
            PathKind::Value(ValueKind::Address)
        );
        assert_eq!(l.kind_of("totals").unwrap(), PathKind::Struct);
        assert_eq!(l.kind_of("balances").unwrap(), PathKind::Mapping);
        assert_eq!(l.kind_of("nested[0x1]").unwrap(), PathKind::Mapping);
        assert_eq!(l.kind_of("items").unwrap(), PathKind::Array);
        assert_eq!(
            l.kind_of("items.length").unwrap(),
            PathKind::Value(ValueKind::Uint)
        );
        assert_eq!(
            l.kind_of("items[2]").unwrap(),
            PathKind::Value(ValueKind::Uint)
        );

        let ts = l.typescript("PlaygroundView");
        assert!(ts.contains("export interface PlaygroundView {"));
        assert!(ts.contains("readonly counter: bigint;"));
        assert!(ts.contains("readonly c: boolean;"));
        assert!(ts.contains("readonly lastPoker: string;"));
        assert!(ts.contains("readonly balances: { readonly [key: string]: bigint };"));
        assert!(ts.contains(
            "readonly nested: { readonly [key: string]: { readonly [key: string]: bigint } };"
        ));
        assert!(ts.contains(
            "readonly items: { readonly [index: number]: bigint; readonly length: bigint };"
        ));
        assert!(ts.contains(
            "readonly totals: {\n    readonly lastTime: bigint;\n    readonly index: bigint;\n  };"
        ));
    }

    /// A layout is a file, not solc: a type that contains itself must not
    /// recurse forever in `typescript()` or `describe_slot()`.
    #[test]
    fn self_referential_layout_terminates() {
        let json = r#"{
          "storage": [{"label":"a","slot":"0","offset":0,"type":"t_struct(A)"}],
          "types": {
            "t_struct(A)": {"encoding":"inplace","label":"struct A","numberOfBytes":"64",
              "members":[{"label":"inner","slot":"0","offset":0,"type":"t_struct(A)"},
                         {"label":"n","slot":"1","offset":0,"type":"t_uint256"}]},
            "t_uint256": {"encoding":"inplace","label":"uint256","numberOfBytes":"32"}
          }}"#;
        let l = Layout::from_json(json).unwrap();
        let ts = l.typescript("Evil");
        assert!(ts.contains("unknown"), "recursion must be cut, got:\n{ts}");
        let names = l.describe_slot(s(1), 16);
        assert!(names.iter().any(|(n, _)| n.ends_with(".n")));
        // path resolution itself is iterative and bounded by the path length
        assert!(l.locate("a.inner.inner.n").is_ok());
    }

    #[test]
    fn decode_address_and_never_panics() {
        let l = layout();
        let loc = l.locate("lastPoker").unwrap();
        let w = slot_b(U256::from_be_slice(&[0xAB; 20]));
        assert_eq!(
            l.decode(&loc, w),
            Value::Address(Address::repeat_byte(0xAB))
        );
        let bogus = Location {
            slot: s(0),
            offset: 40,
            size: 64,
            type_id: "t_uint256".into(),
        };
        let _ = l.decode(&bogus, B256::ZERO);
    }
}
