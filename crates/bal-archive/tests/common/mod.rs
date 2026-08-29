//! In-memory chain + state with real Merkle proofs. The archive must not be
//! able to tell this from a node.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use alloy_trie::{proof::ProofRetainer, HashBuilder, Nibbles, TrieAccount};
use async_trait::async_trait;
use bal_codec::{AccountChanges, BlockAccessList, SlotChanges, StorageChange};
use bal_source::{AccountProof, BalSource, Header, SourcedBlock, StateSource, StorageProof};
use std::collections::BTreeMap;
use std::sync::Mutex;

pub fn slot(n: u64) -> B256 {
    B256::from(U256::from(n).to_be_bytes::<32>())
}

pub fn val(n: u64) -> B256 {
    slot(n)
}

/// Storage of one account at one block.
pub type Storage = BTreeMap<B256, U256>;

/// Build a trie from sorted leaves, retaining the proof for `target`.
fn trie_with_proof(leaves: Vec<(Nibbles, Vec<u8>)>, target: &Nibbles) -> (B256, Vec<Bytes>) {
    let mut hb = HashBuilder::default().with_proof_retainer(ProofRetainer::new(vec![*target]));
    let mut leaves = leaves;
    leaves.sort_by_key(|l| l.0);
    for (n, v) in &leaves {
        hb.add_leaf(*n, v);
    }
    let root = hb.root();
    let nodes = hb.take_proof_nodes();
    let proof = nodes
        .matching_nodes_sorted(target)
        .into_iter()
        .map(|(_, b)| b)
        .collect();
    (root, proof)
}

fn storage_leaves(storage: &Storage) -> Vec<(Nibbles, Vec<u8>)> {
    storage
        .iter()
        .filter(|(_, v)| !v.is_zero())
        .map(|(k, v)| (Nibbles::unpack(keccak256(k)), alloy_rlp::encode(v)))
        .collect()
}

pub fn storage_root(storage: &Storage) -> B256 {
    let mut hb = HashBuilder::default();
    let mut leaves = storage_leaves(storage);
    leaves.sort_by_key(|l| l.0);
    for (n, v) in &leaves {
        hb.add_leaf(*n, v);
    }
    hb.root()
}

pub fn account_of(storage: &Storage) -> TrieAccount {
    TrieAccount {
        nonce: 1,
        balance: U256::ZERO,
        storage_root: storage_root(storage),
        code_hash: keccak256(b"code"),
    }
}

pub fn state_root(addr: Address, storage: &Storage) -> B256 {
    let leaves = vec![(
        Nibbles::unpack(keccak256(addr)),
        alloy_rlp::encode(account_of(storage)),
    )];
    trie_with_proof(leaves, &Nibbles::unpack(keccak256(addr))).0
}

/// A single-account world: block -> storage of `addr`.
pub struct World {
    pub addr: Address,
    pub states: BTreeMap<u64, Storage>,
}

impl World {
    pub fn root_at(&self, block: u64) -> B256 {
        state_root(self.addr, &self.states[&block])
    }
}

#[async_trait]
impl StateSource for World {
    async fn proof(
        &self,
        addr: Address,
        slots: &[B256],
        block: u64,
    ) -> bal_source::Result<AccountProof> {
        assert_eq!(addr, self.addr, "test world has one account");
        let storage = self.states.get(&block).ok_or_else(|| {
            bal_source::SourceError::Transport(format!("no state at {block} (window)"))
        })?;
        let account = account_of(storage);
        let acct_target = Nibbles::unpack(keccak256(addr));
        let (_, account_proof) = trie_with_proof(
            vec![(acct_target, alloy_rlp::encode(account))],
            &acct_target,
        );
        let storage_proofs = slots
            .iter()
            .map(|s| {
                let target = Nibbles::unpack(keccak256(s));
                let (_, proof) = trie_with_proof(storage_leaves(storage), &target);
                StorageProof {
                    key: *s,
                    value: storage.get(s).copied().unwrap_or(U256::ZERO),
                    proof,
                }
            })
            .collect();
        Ok(AccountProof {
            address: addr,
            balance: account.balance,
            nonce: account.nonce,
            code_hash: account.code_hash,
            storage_hash: account.storage_root,
            account_proof,
            storage_proofs,
        })
    }
}

pub struct Chain {
    pub blocks: Mutex<BTreeMap<u64, SourcedBlock>>,
    pub finalized_lag: u64,
}

impl Chain {
    pub fn new() -> Self {
        Self {
            blocks: Mutex::new(BTreeMap::new()),
            finalized_lag: 2,
        }
    }

    /// Append a block whose BAL changes `changes` on `addr`, with a header
    /// whose state_root is `root` and whose hash depends on `salt`.
    pub fn push(&self, number: u64, addr: Address, changes: &[(u64, u64)], root: B256, salt: u8) {
        let bal = if changes.is_empty() {
            BlockAccessList::default()
        } else {
            BlockAccessList {
                accounts: vec![AccountChanges {
                    address: addr,
                    storage_changes: changes
                        .iter()
                        .map(|(s, v)| SlotChanges {
                            slot: U256::from(*s),
                            changes: vec![StorageChange {
                                block_access_index: 1,
                                value: U256::from(*v),
                            }],
                        })
                        .collect(),
                    storage_reads: vec![],
                    balance_changes: vec![],
                    nonce_changes: vec![],
                    code_changes: vec![],
                }],
            }
        };
        let mut blocks = self.blocks.lock().unwrap();
        let parent_hash = blocks
            .get(&(number - 1))
            .map(|b| b.header.hash)
            .unwrap_or(B256::ZERO);
        let hash = keccak256(
            [
                number.to_be_bytes().as_slice(),
                &[salt],
                bal.hash().as_slice(),
            ]
            .concat(),
        );
        let header = Header {
            number,
            hash,
            parent_hash,
            state_root: root,
            timestamp: number * 12,
            block_access_list_hash: Some(bal.hash()),
        };
        blocks.insert(number, SourcedBlock { header, bal });
    }

    /// Drop everything above `block` (to rebuild a competing branch).
    pub fn truncate(&self, block: u64) {
        self.blocks.lock().unwrap().retain(|n, _| *n <= block);
    }
}

#[async_trait]
impl BalSource for Chain {
    async fn head(&self) -> bal_source::Result<u64> {
        Ok(*self.blocks.lock().unwrap().keys().next_back().unwrap())
    }
    async fn finalized(&self) -> bal_source::Result<u64> {
        Ok(self.head().await?.saturating_sub(self.finalized_lag))
    }
    async fn block(&self, number: u64) -> bal_source::Result<SourcedBlock> {
        self.blocks
            .lock()
            .unwrap()
            .get(&number)
            .cloned()
            .ok_or(bal_source::SourceError::BlockNotFound(number))
    }
}
