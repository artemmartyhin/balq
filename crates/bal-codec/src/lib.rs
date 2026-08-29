//! EIP-7928 Block-Level Access List codec.
//!
//! This crate is the *only* place the 7928 wire format is known. Everything
//! above it (archive, layout, cli) consumes the typed structures and must not
//! touch RLP.
//!
//! Wire schema (from the EIP, Review status, subject to change):
//!
//! ```text
//! StorageChange   = [BlockAccessIndex(u32), StorageValue(u256)]
//! BalanceChange   = [BlockAccessIndex(u32), Balance(u256)]
//! NonceChange     = [BlockAccessIndex(u32), Nonce(u64)]
//! CodeChange      = [BlockAccessIndex(u32), Bytecode(bytes)]
//! SlotChanges     = [StorageKey(u256), [StorageChange...]]
//! AccountChanges  = [Address, [SlotChanges...], [StorageKey...],
//!                    [BalanceChange...], [NonceChange...], [CodeChange...]]
//! BlockAccessList = [AccountChanges...]
//! block_access_list_hash = keccak256(rlp(BlockAccessList))
//! ```
//!
//! Ordering is part of validity: accounts by address, slots by key, changes by
//! index, all strictly ascending. A BAL that violates ordering or uniqueness
//! is rejected by [`BlockAccessList::decode`]; it is never softly accepted.
//!
//! With the `json` feature, [`BlockAccessList::from_rpc_json`] accepts the
//! execution-apis JSON form served by `eth_getBlockAccessList`.

#[cfg(feature = "json")]
mod json;
mod schema;
mod validate;

pub use schema::{
    AccountChanges, BalanceChange, BlockAccessIndex, BlockAccessList, CodeChange, NonceChange,
    SlotChanges, StorageChange,
};

use alloy_primitives::{keccak256, Address, B256, U256};
use alloy_rlp::{Decodable, Encodable};

/// Hash of the empty BAL: `keccak256(rlp([]))` = `keccak256(0xc0)`.
pub const EMPTY_BAL_HASH: B256 = B256::new([
    0x1d, 0xcc, 0x4d, 0xe8, 0xde, 0xc7, 0x5d, 0x7a, 0xab, 0x85, 0xb5, 0x67, 0xb6, 0xcc, 0xd4, 0x1a,
    0xd3, 0x12, 0x45, 0x1b, 0x94, 0x8a, 0x74, 0x13, 0xf0, 0xa1, 0x42, 0xfd, 0x40, 0xd4, 0x93, 0x47,
]);

/// Decoding, validation and verification failures.
#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    /// Malformed RLP.
    #[error("rlp: {0}")]
    Rlp(#[from] alloy_rlp::Error),
    /// Bytes left over after the outer list.
    #[error("trailing bytes after BAL: {0} bytes")]
    Trailing(usize),
    /// A list is not strictly ascending.
    #[error("ordering violated: {0}")]
    Ordering(&'static str),
    /// A key appears twice in a list.
    #[error("duplicate {what} at position {pos}")]
    Duplicate {
        /// Which list.
        what: &'static str,
        /// Index of the repeated element.
        pos: usize,
    },
    /// The EIP forbids a key in both `storage_changes` and `storage_reads`.
    #[error("storage key appears in both storage_changes and storage_reads for {0}")]
    KeyInChangesAndReads(Address),
    /// A slot entry with no changes.
    #[error("empty change list for slot {slot} of {address}")]
    EmptySlotChanges {
        /// Account.
        address: Address,
        /// Slot key.
        slot: B256,
    },
    /// `keccak(rlp(bal))` differs from the header.
    #[error("BAL hash mismatch: computed {computed}, expected {expected}")]
    HashMismatch {
        /// What this codec computed.
        computed: B256,
        /// What the header says.
        expected: B256,
    },
    /// The JSON form did not match the expected shape (`json` feature).
    #[error("json: {0}")]
    Json(String),
}

impl BlockAccessList {
    /// Decode from RLP and validate ordering/uniqueness. The whole input must
    /// be consumed; trailing bytes are an error.
    pub fn decode(mut bytes: &[u8]) -> Result<Self, CodecError> {
        let bal = <Self as Decodable>::decode(&mut bytes)?;
        if !bytes.is_empty() {
            return Err(CodecError::Trailing(bytes.len()));
        }
        validate::validate(&bal)?;
        Ok(bal)
    }

    /// Canonical RLP encoding.
    pub fn encode_rlp(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.length());
        Encodable::encode(self, &mut out);
        out
    }

    /// `keccak256(rlp(self))` — the value that lives in
    /// `header.block_access_list_hash`.
    pub fn hash(&self) -> B256 {
        keccak256(self.encode_rlp())
    }

    /// Compare against the hash taken from the block header.
    pub fn verify(&self, expected_bal_hash: B256) -> Result<(), CodecError> {
        let computed = self.hash();
        if computed == expected_bal_hash {
            Ok(())
        } else {
            Err(CodecError::HashMismatch {
                computed,
                expected: expected_bal_hash,
            })
        }
    }

    /// Binary search by address (accounts are sorted by construction).
    pub fn account(&self, addr: &Address) -> Option<&AccountChanges> {
        self.accounts
            .binary_search_by(|a| a.address.cmp(addr))
            .ok()
            .map(|i| &self.accounts[i])
    }

    /// `true` when no account was touched in the block.
    pub fn is_empty(&self) -> bool {
        self.accounts.is_empty()
    }
}

impl AccountChanges {
    /// Binary search by slot key.
    pub fn slot(&self, key: &B256) -> Option<&SlotChanges> {
        let k = U256::from_be_bytes(key.0);
        self.storage_changes
            .binary_search_by(|s| s.slot.cmp(&k))
            .ok()
            .map(|i| &self.storage_changes[i])
    }

    /// `true` iff the account has at least one storage write. Presence in the
    /// BAL alone (reads, balance touches) does NOT count.
    pub fn has_storage_changes(&self) -> bool {
        !self.storage_changes.is_empty()
    }
}

impl SlotChanges {
    /// Slot key as a 32-byte word.
    pub fn slot_b256(&self) -> B256 {
        B256::from(self.slot.to_be_bytes::<32>())
    }

    /// Last write in block order — the end-of-block value.
    ///
    /// Validation guarantees `changes` is non-empty; a `SlotChanges` built by
    /// hand with no changes yields a zero change rather than a panic.
    pub fn final_change(&self) -> &StorageChange {
        static EMPTY: StorageChange = StorageChange {
            block_access_index: 0,
            value: U256::ZERO,
        };
        self.changes.last().unwrap_or(&EMPTY)
    }
}

impl StorageChange {
    /// Post-value as a 32-byte word.
    pub fn value_b256(&self) -> B256 {
        B256::from(self.value.to_be_bytes::<32>())
    }
}
