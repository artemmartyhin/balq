//! Source abstraction: the archive never talks to a node directly, it talks
//! to a [`BalSource`] (blocks + BALs) and a [`StateSource`] (Merkle proofs for
//! bootstrap). Implementations: JSON-RPC (here), Engine API and in-process
//! (later crates). Which of these actually work is the day-0 question; the
//! archive does not care.

mod fallback;
mod jsonrpc;
pub mod proof;

pub use fallback::Fallback;
pub use jsonrpc::{BalProbe, JsonRpcSource, ProbeReport, BAL_HASH_FIELD, BAL_METHOD};
pub use proof::{check_requested, verify_account_proof, ProofError};

use alloy_primitives::{Address, Bytes, B256, U256};
use async_trait::async_trait;
use bal_codec::BlockAccessList;

/// What can go wrong between the archive and a node.
#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    /// HTTP / connection failure.
    #[error("transport: {0}")]
    Transport(String),
    /// The node answered with a JSON-RPC error object.
    #[error("rpc error {code}: {message}")]
    Rpc {
        /// JSON-RPC error code.
        code: i64,
        /// JSON-RPC error message.
        message: String,
    },
    /// `eth_getBlockByNumber` returned null.
    #[error("block {0} not found")]
    BlockNotFound(u64),
    /// The block exists but the node has no BAL for it (pruned or pre-fork).
    #[error("eth_getBlockAccessList returned null for block {0}")]
    NoBal(u64),
    /// The header carries no `blockAccessListHash`.
    #[error("header of block {0} has no block_access_list_hash")]
    NoBalHash(u64),
    /// A response did not have the expected shape.
    #[error("malformed response: {0}")]
    Malformed(String),
    /// The BAL failed to decode or validate.
    #[error("codec: {0}")]
    Codec(#[from] bal_codec::CodecError),
}

/// Result of source operations.
pub type Result<T> = std::result::Result<T, SourceError>;

/// The parts of a block header the archive needs. Kept minimal on purpose:
/// the full Glamsterdam header layout is not frozen, and only these fields
/// participate in verification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    /// Block height.
    pub number: u64,
    /// Block hash as reported by the node.
    pub hash: B256,
    /// Hash of the parent block; used to detect reorgs between passes.
    pub parent_hash: B256,
    /// Post-state root; Merkle proofs are verified against it.
    pub state_root: B256,
    /// Block timestamp (seconds).
    pub timestamp: u64,
    /// `None` on pre-Glamsterdam blocks or clients that do not expose it.
    pub block_access_list_hash: Option<B256>,
}

/// A block as the archive consumes it: header plus decoded, *unverified* BAL.
/// Verification against `header.block_access_list_hash` is the archive's job
/// so that no source implementation can skip it.
#[derive(Clone, Debug)]
pub struct SourcedBlock {
    /// Header fields relevant to verification.
    pub header: Header,
    /// Decoded BAL, not yet checked against the header.
    pub bal: BlockAccessList,
}

/// Where blocks and their BALs come from.
#[async_trait]
pub trait BalSource: Send + Sync {
    /// Latest block number the source knows.
    async fn head(&self) -> Result<u64>;
    /// Latest finalized block number (reorg horizon).
    async fn finalized(&self) -> Result<u64>;
    /// Header + BAL for one block.
    async fn block(&self, number: u64) -> Result<SourcedBlock>;
    /// Header only. Used for reorg checks, where fetching the BAL would be
    /// wasted work; the default goes through [`BalSource::block`].
    async fn header(&self, number: u64) -> Result<Header> {
        Ok(self.block(number).await?.header)
    }
    /// BAL body only, unverified. Lets a backup supply the body while the
    /// header — the canonical-chain fact — still comes from the primary.
    async fn bal(&self, number: u64) -> Result<BlockAccessList> {
        Ok(self.block(number).await?.bal)
    }
}

/// One slot's proof against an account's storage root.
#[derive(Clone, Debug)]
pub struct StorageProof {
    /// Slot key (32 bytes).
    pub key: B256,
    /// Value at that slot; zero means absent, proven by exclusion.
    pub value: U256,
    /// Trie nodes from the storage root down to the leaf (or to the point of exclusion).
    pub proof: Vec<Bytes>,
}

/// `eth_getProof` response: account leaf plus any number of storage proofs.
#[derive(Clone, Debug)]
pub struct AccountProof {
    /// Account address.
    pub address: Address,
    /// Account balance.
    pub balance: U256,
    /// Account nonce.
    pub nonce: u64,
    /// keccak of the account's code.
    pub code_hash: B256,
    /// Root of the account's storage trie.
    pub storage_hash: B256,
    /// Trie nodes from the state root down to the account leaf.
    pub account_proof: Vec<Bytes>,
    /// Proofs for the requested slots, in request order.
    pub storage_proofs: Vec<StorageProof>,
}

/// Where Merkle proofs come from.
#[async_trait]
pub trait StateSource: Send + Sync {
    /// `eth_getProof(addr, slots, block)`. One call per (address, block)
    /// carries any number of slots — batching happens here, not in JSON-RPC.
    async fn proof(&self, addr: Address, slots: &[B256], block: u64) -> Result<AccountProof>;
}
