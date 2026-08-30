//! Key layouts. The byte layout IS the schema version: change it here, bump
//! `SCHEMA_VERSION`, and never write a migration in a hurry.
//!
//! ```text
//! SLOTS     addr(20) || slot(32) || block(8, BE) || index(4, BE)  ->  tag(1) || value(32)
//! BLOCKIDX  addr(20) || block(8, BE) || slot(32)                  ->  ()
//! BOOT      addr(20) || slot(32)                                  ->  state(1) || first_seen(8, BE)
//! ```
//!
//! Big-endian block/index make lexicographic order equal numeric order, which
//! is what makes "seek to (addr, slot, block, MAX) and step back" a single
//! ordered-KV operation.

use alloy_primitives::{Address, B256};
use bal_codec::BlockAccessIndex;

/// Key/value layout version written into `meta:schema_version`. Bump when
/// any byte layout in this module changes.
pub const SCHEMA_VERSION: u32 = 1;

pub const SLOT_KEY_LEN: usize = 20 + 32 + 8 + 4;
pub const SLOT_PREFIX_LEN: usize = 20 + 32;
pub const BLOCKIDX_KEY_LEN: usize = 20 + 8 + 32;
pub const BLOCKIDX_PREFIX_LEN: usize = 20 + 8;

/// Where a stored value came from. Stored as the first byte of every value so
/// provenance can never drift from the data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Provenance {
    /// From a BAL verified against `header.block_access_list_hash`.
    Bal = 0,
    /// From `eth_getProof` verified against `header.state_root`.
    Proof = 1,
    /// Imported, unverified. Only ever written by an explicit import command.
    Imported = 2,
    /// From a BAL that could not be checked because the header carried no
    /// hash. Only written with `allow_unverified`; never mistaken for `Bal`.
    Unverified = 3,
}

impl Provenance {
    /// Inverse of `self as u8`; `None` for tags this build does not know.
    pub fn from_byte(b: u8) -> Option<Self> {
        match b {
            0 => Some(Self::Bal),
            1 => Some(Self::Proof),
            2 => Some(Self::Imported),
            3 => Some(Self::Unverified),
            _ => None,
        }
    }

    /// `true` for values anchored to a block header (BAL hash or state root).
    pub fn is_verified(self) -> bool {
        matches!(self, Self::Bal | Self::Proof)
    }
}

pub(crate) fn slot_key(
    addr: Address,
    slot: B256,
    block: u64,
    index: BlockAccessIndex,
) -> [u8; SLOT_KEY_LEN] {
    let mut k = [0u8; SLOT_KEY_LEN];
    k[..20].copy_from_slice(addr.as_slice());
    k[20..52].copy_from_slice(slot.as_slice());
    k[52..60].copy_from_slice(&block.to_be_bytes());
    k[60..64].copy_from_slice(&index.to_be_bytes());
    k
}

pub fn slot_prefix(addr: Address, slot: B256) -> [u8; SLOT_PREFIX_LEN] {
    let mut k = [0u8; SLOT_PREFIX_LEN];
    k[..20].copy_from_slice(addr.as_slice());
    k[20..].copy_from_slice(slot.as_slice());
    k
}

pub fn parse_slot_key(k: &[u8]) -> Option<(Address, B256, u64, BlockAccessIndex)> {
    if k.len() != SLOT_KEY_LEN {
        return None;
    }
    Some((
        Address::from_slice(&k[..20]),
        B256::from_slice(&k[20..52]),
        u64::from_be_bytes(k[52..60].try_into().ok()?),
        u32::from_be_bytes(k[60..64].try_into().ok()?),
    ))
}

pub fn blockidx_key(addr: Address, block: u64, slot: B256) -> [u8; BLOCKIDX_KEY_LEN] {
    let mut k = [0u8; BLOCKIDX_KEY_LEN];
    k[..20].copy_from_slice(addr.as_slice());
    k[20..28].copy_from_slice(&block.to_be_bytes());
    k[28..].copy_from_slice(slot.as_slice());
    k
}

pub fn blockidx_prefix(addr: Address, block: u64) -> [u8; BLOCKIDX_PREFIX_LEN] {
    let mut k = [0u8; BLOCKIDX_PREFIX_LEN];
    k[..20].copy_from_slice(addr.as_slice());
    k[20..].copy_from_slice(&block.to_be_bytes());
    k
}

pub fn parse_blockidx_key(k: &[u8]) -> Option<(Address, u64, B256)> {
    if k.len() != BLOCKIDX_KEY_LEN {
        return None;
    }
    Some((
        Address::from_slice(&k[..20]),
        u64::from_be_bytes(k[20..28].try_into().ok()?),
        B256::from_slice(&k[28..]),
    ))
}

pub fn encode_value(p: Provenance, v: B256) -> [u8; 33] {
    let mut out = [0u8; 33];
    out[0] = p as u8;
    out[1..].copy_from_slice(v.as_slice());
    out
}

pub fn decode_value(v: &[u8]) -> Option<(Provenance, B256)> {
    if v.len() != 33 {
        return None;
    }
    Some((Provenance::from_byte(v[0])?, B256::from_slice(&v[1..])))
}

/// Bootstrap bookkeeping per (addr, slot).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootState {
    /// Pre-value obtained and stored (with `Provenance::Proof`).
    Done,
    /// First seen at `first_seen`; proof at `first_seen - 1` not yet obtained.
    Pending {
        /// Block of the first recorded change.
        first_seen: u64,
    },
    /// `first_seen - 1` fell out of the node's state window before a proof was
    /// obtained. Terminal: reads in `[start, first_seen)` are unavailable.
    Lost {
        /// Block of the first recorded change.
        first_seen: u64,
    },
}

pub fn encode_boot(s: BootState) -> [u8; 9] {
    let mut out = [0u8; 9];
    let (tag, n) = match s {
        BootState::Done => (0u8, 0u64),
        BootState::Pending { first_seen } => (1, first_seen),
        BootState::Lost { first_seen } => (2, first_seen),
    };
    out[0] = tag;
    out[1..].copy_from_slice(&n.to_be_bytes());
    out
}

pub fn decode_boot(v: &[u8]) -> Option<BootState> {
    if v.len() != 9 {
        return None;
    }
    let n = u64::from_be_bytes(v[1..].try_into().ok()?);
    match v[0] {
        0 => Some(BootState::Done),
        1 => Some(BootState::Pending { first_seen: n }),
        2 => Some(BootState::Lost { first_seen: n }),
        _ => None,
    }
}

/// Exclusive upper bound for a prefix range: prefix + 1 in big-endian.
/// Returns `None` if the prefix is all 0xFF (then the range is unbounded).
pub fn prefix_end(prefix: &[u8]) -> Option<Vec<u8>> {
    let mut end = prefix.to_vec();
    for i in (0..end.len()).rev() {
        if end[i] != 0xFF {
            end[i] += 1;
            end.truncate(i + 1);
            return Some(end);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_order_numerically() {
        let a = Address::repeat_byte(1);
        let s = B256::repeat_byte(2);
        assert!(slot_key(a, s, 255, 0) < slot_key(a, s, 256, 0));
        assert!(slot_key(a, s, 256, 1) < slot_key(a, s, 256, 2));
        assert!(slot_key(a, s, 256, u32::MAX) < slot_key(a, s, 257, 0));
        assert_eq!(parse_slot_key(&slot_key(a, s, 7, 9)), Some((a, s, 7, 9)));
    }

    #[test]
    fn prefix_end_increments() {
        assert_eq!(prefix_end(&[1, 2, 3]), Some(vec![1, 2, 4]));
        assert_eq!(prefix_end(&[1, 0xFF]), Some(vec![2]));
        assert_eq!(prefix_end(&[0xFF, 0xFF]), None);
    }

    #[test]
    fn value_roundtrip() {
        let v = B256::repeat_byte(9);
        assert_eq!(
            decode_value(&encode_value(Provenance::Proof, v)),
            Some((Provenance::Proof, v))
        );
        assert_eq!(
            decode_boot(&encode_boot(BootState::Pending { first_seen: 5 })),
            Some(BootState::Pending { first_seen: 5 })
        );
    }
}
