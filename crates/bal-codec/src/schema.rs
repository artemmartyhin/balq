//! Wire structures. This file — and only this file — mirrors the EIP text.
//! A spec change lands here as field edits; derive macros regenerate RLP.

use alloy_primitives::{Address, Bytes, U256};
use alloy_rlp::{RlpDecodable, RlpEncodable};

/// Position of a change in the block's life cycle.
/// `0` = pre-execution system calls, `1..=n` = transactions, `n+1` = post-execution.
pub type BlockAccessIndex = u32;

/// One write to a storage slot.
#[derive(Clone, Debug, PartialEq, Eq, RlpEncodable, RlpDecodable)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct StorageChange {
    /// Where in the block the write happened.
    pub block_access_index: BlockAccessIndex,
    /// Post-value. There are no pre-values anywhere in a BAL.
    pub value: U256,
}

/// One balance change.
#[derive(Clone, Debug, PartialEq, Eq, RlpEncodable, RlpDecodable)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct BalanceChange {
    /// Where in the block the change happened.
    pub block_access_index: BlockAccessIndex,
    /// Balance after the change.
    pub post_balance: U256,
}

/// One nonce change.
#[derive(Clone, Debug, PartialEq, Eq, RlpEncodable, RlpDecodable)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct NonceChange {
    /// Where in the block the change happened.
    pub block_access_index: BlockAccessIndex,
    /// Nonce after the change.
    pub new_nonce: u64,
}

/// One code change (deployment or self-destruct).
#[derive(Clone, Debug, PartialEq, Eq, RlpEncodable, RlpDecodable)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CodeChange {
    /// Where in the block the change happened.
    pub block_access_index: BlockAccessIndex,
    /// Code after the change.
    pub new_code: Bytes,
}

/// All writes to one slot within the block.
#[derive(Clone, Debug, PartialEq, Eq, RlpEncodable, RlpDecodable)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SlotChanges {
    /// Slot key.
    pub slot: U256,
    /// Sorted by `block_access_index`, non-empty.
    pub changes: Vec<StorageChange>,
}

/// Everything that happened to one account within the block.
#[derive(Clone, Debug, PartialEq, Eq, RlpEncodable, RlpDecodable)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct AccountChanges {
    /// Account address.
    pub address: Address,
    /// Sorted by slot key.
    pub storage_changes: Vec<SlotChanges>,
    /// Keys only — slots read (or written with an unchanged value). Sorted.
    pub storage_reads: Vec<U256>,
    /// Sorted by index.
    pub balance_changes: Vec<BalanceChange>,
    /// Sorted by index.
    pub nonce_changes: Vec<NonceChange>,
    /// Sorted by index.
    pub code_changes: Vec<CodeChange>,
}

/// The whole list. Wire form is a bare RLP list of `AccountChanges`
/// (no wrapping struct), so encoding is transparent over `accounts`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct BlockAccessList {
    /// Sorted by address; every touched account appears exactly once.
    pub accounts: Vec<AccountChanges>,
}

impl alloy_rlp::Encodable for BlockAccessList {
    fn encode(&self, out: &mut dyn alloy_rlp::BufMut) {
        self.accounts.encode(out)
    }
    fn length(&self) -> usize {
        self.accounts.length()
    }
}

impl alloy_rlp::Decodable for BlockAccessList {
    fn decode(buf: &mut &[u8]) -> alloy_rlp::Result<Self> {
        Ok(Self {
            accounts: Vec::<AccountChanges>::decode(buf)?,
        })
    }
}
