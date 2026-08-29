//! Primary + backup source.
//!
//! The primary is the node that decides what the chain *is*: head,
//! finalized, headers. That is the one thing balq takes on trust, and it is
//! never delegated — if the primary is down, sync waits.
//!
//! The backup supplies only facts that are verified afterwards: the BAL
//! body of a block the primary has pruned (checked against the primary's
//! header hash), and proofs at blocks outside the primary's state window
//! (checked against a `state_root` the archive already holds). It can be
//! any third-party archive endpoint: it adds reach, not authority.

use crate::{AccountProof, BalSource, Header, Result, SourcedBlock, StateSource};
use alloy_primitives::{Address, B256};
use async_trait::async_trait;
use bal_codec::BlockAccessList;
use tracing::info;

/// Primary for the chain, backup for old data.
pub struct Fallback<P, B> {
    /// Decides head/finalized/headers; asked first for everything.
    pub primary: P,
    /// Asked for a BAL body or a proof when the primary cannot serve it.
    pub backup: B,
}

impl<P, B> Fallback<P, B> {
    /// Wrap `primary` with `backup`.
    pub fn new(primary: P, backup: B) -> Self {
        Self { primary, backup }
    }
}

#[async_trait]
impl<P: BalSource, B: BalSource> BalSource for Fallback<P, B> {
    async fn head(&self) -> Result<u64> {
        self.primary.head().await
    }

    async fn finalized(&self) -> Result<u64> {
        self.primary.finalized().await
    }

    async fn header(&self, number: u64) -> Result<Header> {
        self.primary.header(number).await
    }

    /// Header always from the primary; body from the primary, or from the
    /// backup if the primary has none. The archive verifies the body against
    /// this header, so a backup body is held to the primary's chain.
    async fn block(&self, number: u64) -> Result<SourcedBlock> {
        let header = self.primary.header(number).await?;
        let bal = match self.primary.bal(number).await {
            Ok(b) => b,
            Err(e) => {
                info!(block = number, %e, "primary has no BAL body; asking backup");
                self.backup.bal(number).await?
            }
        };
        Ok(SourcedBlock { header, bal })
    }

    async fn bal(&self, number: u64) -> Result<BlockAccessList> {
        match self.primary.bal(number).await {
            Ok(b) => Ok(b),
            Err(e) => {
                info!(block = number, %e, "primary has no BAL body; asking backup");
                self.backup.bal(number).await
            }
        }
    }
}

#[async_trait]
impl<P: StateSource, B: StateSource> StateSource for Fallback<P, B> {
    async fn proof(&self, addr: Address, slots: &[B256], block: u64) -> Result<AccountProof> {
        match self.primary.proof(addr, slots, block).await {
            Ok(p) => Ok(p),
            Err(e) => {
                info!(block, %addr, slots = slots.len(), %e, "primary cannot prove; asking backup");
                self.backup.proof(addr, slots, block).await
            }
        }
    }
}

/// `Option<S>` is a source that is simply absent: every call fails with
/// [`crate::SourceError::Transport`]. Lets `Fallback<P, Option<B>>` express
/// "backup configured or not" without a second type.
#[async_trait]
impl<S: BalSource> BalSource for Option<S> {
    async fn head(&self) -> Result<u64> {
        match self {
            Some(s) => s.head().await,
            None => Err(absent()),
        }
    }
    async fn finalized(&self) -> Result<u64> {
        match self {
            Some(s) => s.finalized().await,
            None => Err(absent()),
        }
    }
    async fn block(&self, number: u64) -> Result<SourcedBlock> {
        match self {
            Some(s) => s.block(number).await,
            None => Err(absent()),
        }
    }
    async fn header(&self, number: u64) -> Result<Header> {
        match self {
            Some(s) => s.header(number).await,
            None => Err(absent()),
        }
    }
    async fn bal(&self, number: u64) -> Result<BlockAccessList> {
        match self {
            Some(s) => s.bal(number).await,
            None => Err(absent()),
        }
    }
}

#[async_trait]
impl<S: StateSource> StateSource for Option<S> {
    async fn proof(&self, addr: Address, slots: &[B256], block: u64) -> Result<AccountProof> {
        match self {
            Some(s) => s.proof(addr, slots, block).await,
            None => Err(absent()),
        }
    }
}

#[async_trait]
impl<S: BalSource + ?Sized> BalSource for &S {
    async fn head(&self) -> Result<u64> {
        (**self).head().await
    }
    async fn finalized(&self) -> Result<u64> {
        (**self).finalized().await
    }
    async fn block(&self, number: u64) -> Result<SourcedBlock> {
        (**self).block(number).await
    }
    async fn header(&self, number: u64) -> Result<Header> {
        (**self).header(number).await
    }
    async fn bal(&self, number: u64) -> Result<BlockAccessList> {
        (**self).bal(number).await
    }
}

#[async_trait]
impl<S: StateSource + ?Sized> StateSource for &S {
    async fn proof(&self, addr: Address, slots: &[B256], block: u64) -> Result<AccountProof> {
        (**self).proof(addr, slots, block).await
    }
}

fn absent() -> crate::SourceError {
    crate::SourceError::Transport("no backup source configured".into())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::SourceError;
    use alloy_primitives::U256;

    struct Fails;
    struct Answers(u64);

    #[async_trait]
    impl StateSource for Fails {
        async fn proof(&self, _: Address, _: &[B256], b: u64) -> Result<AccountProof> {
            Err(SourceError::Rpc {
                code: -32602,
                message: format!("distance to target block {b} exceeds maximum proof window"),
            })
        }
    }

    #[async_trait]
    impl StateSource for Answers {
        async fn proof(&self, addr: Address, slots: &[B256], _: u64) -> Result<AccountProof> {
            Ok(AccountProof {
                address: addr,
                balance: U256::from(self.0),
                nonce: 0,
                code_hash: B256::ZERO,
                storage_hash: B256::ZERO,
                account_proof: vec![],
                storage_proofs: slots
                    .iter()
                    .map(|k| crate::StorageProof {
                        key: *k,
                        value: U256::ZERO,
                        proof: vec![],
                    })
                    .collect(),
            })
        }
    }

    /// A chain source that serves headers but has pruned every BAL.
    struct HeadersOnly;
    /// A chain source that serves BALs but whose headers must never be used.
    struct BodiesOnly;

    fn header(n: u64, tag: u8) -> Header {
        Header {
            number: n,
            hash: B256::repeat_byte(tag),
            parent_hash: B256::ZERO,
            state_root: B256::ZERO,
            timestamp: 0,
            block_access_list_hash: Some(bal_codec::EMPTY_BAL_HASH),
        }
    }

    #[async_trait]
    impl BalSource for HeadersOnly {
        async fn head(&self) -> Result<u64> {
            Ok(100)
        }
        async fn finalized(&self) -> Result<u64> {
            Ok(90)
        }
        async fn block(&self, n: u64) -> Result<SourcedBlock> {
            Err(SourceError::NoBal(n))
        }
        async fn header(&self, n: u64) -> Result<Header> {
            Ok(header(n, 0xAA))
        }
        async fn bal(&self, n: u64) -> Result<BlockAccessList> {
            Err(SourceError::NoBal(n))
        }
    }

    #[async_trait]
    impl BalSource for BodiesOnly {
        async fn head(&self) -> Result<u64> {
            Ok(999)
        }
        async fn finalized(&self) -> Result<u64> {
            Ok(998)
        }
        async fn block(&self, n: u64) -> Result<SourcedBlock> {
            Ok(SourcedBlock {
                header: header(n, 0xBB),
                bal: BlockAccessList::default(),
            })
        }
        async fn bal(&self, _: u64) -> Result<BlockAccessList> {
            Ok(BlockAccessList::default())
        }
    }

    #[tokio::test]
    async fn backup_is_used_when_primary_fails() {
        let f = Fallback::new(Fails, Answers(7));
        let p = f.proof(Address::ZERO, &[B256::ZERO], 1).await.unwrap();
        assert_eq!(p.balance, U256::from(7));
    }

    #[tokio::test]
    async fn primary_wins_when_it_answers() {
        let f = Fallback::new(Answers(1), Answers(2));
        let p = f.proof(Address::ZERO, &[], 1).await.unwrap();
        assert_eq!(p.balance, U256::from(1));
    }

    #[tokio::test]
    async fn both_failing_returns_backup_error() {
        let f = Fallback::new(Fails, Fails);
        assert!(f.proof(Address::ZERO, &[], 1).await.is_err());
    }

    #[tokio::test]
    async fn backup_supplies_body_but_never_the_chain() {
        let f = Fallback::new(HeadersOnly, BodiesOnly);
        // Chain facts: primary only, even though the backup is "ahead".
        assert_eq!(f.head().await.unwrap(), 100);
        assert_eq!(f.finalized().await.unwrap(), 90);
        assert_eq!(f.header(5).await.unwrap().hash, B256::repeat_byte(0xAA));
        // Body from the backup, header from the primary.
        let b = f.block(5).await.unwrap();
        assert_eq!(b.header.hash, B256::repeat_byte(0xAA));
        assert!(b.bal.is_empty());
    }

    #[tokio::test]
    async fn primary_down_means_no_chain_facts() {
        struct Down;
        #[async_trait]
        impl BalSource for Down {
            async fn head(&self) -> Result<u64> {
                Err(SourceError::Transport("down".into()))
            }
            async fn finalized(&self) -> Result<u64> {
                Err(SourceError::Transport("down".into()))
            }
            async fn block(&self, n: u64) -> Result<SourcedBlock> {
                Err(SourceError::BlockNotFound(n))
            }
        }
        let f = Fallback::new(Down, BodiesOnly);
        assert!(f.head().await.is_err(), "backup must not decide the head");
        assert!(
            f.block(1).await.is_err(),
            "no header from primary, no block"
        );
    }
}
