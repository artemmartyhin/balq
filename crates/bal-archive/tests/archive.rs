//! Archive behaviour against an in-memory chain with real Merkle proofs.
#![allow(clippy::unwrap_used, clippy::expect_used)]
mod common;

use alloy_primitives::{Address, U256};
use bal_archive::{Archive, ArchiveConfig, ArchiveError, NotAvailable, Provenance};
use common::*;
use std::collections::BTreeMap;

const A: Address = Address::repeat_byte(0xAA);
const B: Address = Address::repeat_byte(0xBB);

/// Chain 8..=14, watch A from 10.
///   pre-state (block 9): s1=7, s3=42
///   block 10: s1 -> 100
///   block 12: s1 -> 200, s2 -> 5
///   block 14: s1 -> 300
fn world() -> (World, Chain) {
    let mut states: BTreeMap<u64, Storage> = BTreeMap::new();
    let mut st = Storage::new();
    st.insert(slot(1), U256::from(7));
    st.insert(slot(3), U256::from(42));
    for b in 8..=9 {
        states.insert(b, st.clone());
    }
    st.insert(slot(1), U256::from(100));
    for b in 10..=11 {
        states.insert(b, st.clone());
    }
    st.insert(slot(1), U256::from(200));
    st.insert(slot(2), U256::from(5));
    for b in 12..=13 {
        states.insert(b, st.clone());
    }
    st.insert(slot(1), U256::from(300));
    states.insert(14, st.clone());
    let world = World { addr: A, states };

    let chain = Chain::new();
    chain.push(8, A, &[], world.root_at(8), 0);
    chain.push(9, A, &[], world.root_at(9), 0);
    chain.push(10, A, &[(1, 100)], world.root_at(10), 0);
    chain.push(11, A, &[], world.root_at(11), 0);
    chain.push(12, A, &[(1, 200), (2, 5)], world.root_at(12), 0);
    chain.push(13, A, &[], world.root_at(13), 0);
    chain.push(14, A, &[(1, 300)], world.root_at(14), 0);
    (world, chain)
}

fn open(cfg: ArchiveConfig) -> (Archive, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let a = Archive::open_with(dir.path().join("a.redb"), cfg).unwrap();
    (a, dir)
}

#[tokio::test]
async fn sync_reads_and_bounds() {
    let (world, chain) = world();
    let (ar, _d) = open(ArchiveConfig::default());
    ar.watch(A, 10).unwrap();
    let rep = ar.sync(&chain, Some(&world)).await.unwrap();
    assert_eq!(
        (rep.from, rep.to, rep.blocks_applied),
        (Some(10), Some(14), 5)
    );
    assert_eq!(rep.bootstrapped, 2, "s1 at 9 and s2 at 11 proven");
    assert_eq!(rep.bootstrap_pending, 0);
    assert_eq!(ar.head().unwrap().map(|h| h.0), Some(14));

    // bounds
    assert_eq!(
        ar.storage_at(B, slot(1), 12).unwrap_err(),
        NotAvailable::NotWatched(B)
    );
    assert_eq!(
        ar.storage_at(A, slot(1), 9).unwrap_err(),
        NotAvailable::BeforeStart {
            requested: 9,
            start: 10
        }
    );
    assert_eq!(
        ar.storage_at(A, slot(1), 15).unwrap_err(),
        NotAvailable::AfterHead {
            requested: 15,
            head: 14
        }
    );

    // values across blocks: seek + step back
    let v = |b| ar.storage_at(A, slot(1), b).unwrap();
    assert_eq!(
        (v(10).value, v(10).provenance, v(10).set_at),
        (val(100), Provenance::Bal, 10)
    );
    assert_eq!((v(11).value, v(11).set_at), (val(100), 10));
    assert_eq!(v(12).value, val(200));
    assert_eq!(v(13).value, val(200));
    assert_eq!(v(14).value, val(300));

    // s2 before its first change: proven zero (exclusion proof), not a guess
    let z = ar.storage_at(A, slot(2), 11).unwrap();
    assert_eq!(
        (z.value, z.provenance, z.set_at),
        (val(0), Provenance::Proof, 9)
    );
    assert_eq!(ar.storage_at(A, slot(2), 12).unwrap().value, val(5));

    // s3 never changed: needs lazy bootstrap at head
    assert_eq!(
        ar.storage_at(A, slot(3), 10).unwrap_err(),
        NotAvailable::NotBootstrapped
    );
    ar.bootstrap_slot(&world, A, slot(3)).await.unwrap();
    let s3 = ar.storage_at(A, slot(3), 10).unwrap();
    assert_eq!((s3.value, s3.provenance), (val(42), Provenance::Proof));
    assert_eq!(ar.storage_at(A, slot(3), 14).unwrap().value, val(42));

    // s9 never existed anywhere: bootstrap proves absence -> zero, Proof
    ar.bootstrap_slot(&world, A, slot(9)).await.unwrap();
    assert_eq!(ar.storage_at(A, slot(9), 12).unwrap().value, val(0));

    // secondary index and history
    assert_eq!(ar.changed_slots(A, 12).unwrap(), vec![slot(1), slot(2)]);
    assert!(ar.changed_slots(A, 13).unwrap().is_empty());
    let h: Vec<(u64, _)> = ar
        .history(A, slot(1), 10..15)
        .unwrap()
        .into_iter()
        .map(|e| (e.block, e.value))
        .collect();
    assert_eq!(h, vec![(10, val(100)), (12, val(200)), (14, val(300))]);
    assert_eq!(
        ar.history(A, slot(1), 9..12).unwrap_err(),
        NotAvailable::BeforeStart {
            requested: 9,
            start: 10
        }
    );

    // idempotent re-sync
    let rep2 = ar.sync(&chain, Some(&world)).await.unwrap();
    assert_eq!(rep2.blocks_applied, 0);
}

#[tokio::test]
async fn pending_then_resolved_then_lost() {
    let (world, chain) = world();
    let (ar, _d) = open(ArchiveConfig {
        bootstrap_window: 3,
        ..Default::default()
    });
    ar.watch(A, 10).unwrap();

    // No state source: first-seen slots stay pending.
    let rep = ar.sync(&chain, None).await.unwrap();
    assert_eq!(rep.bootstrap_pending, 2);
    assert_eq!(
        ar.storage_at(A, slot(2), 11).unwrap_err(),
        NotAvailable::BootstrapPending { first_seen: 12 }
    );

    // Later sync with state: s2 (first seen 12, proof at 11, head 14 -> age 3 <= window) resolves;
    // s1 (first seen 10, proof at 9, age 5 > window) is lost.
    let rep = ar.sync(&chain, Some(&world)).await.unwrap();
    assert_eq!(
        (rep.bootstrapped, rep.bootstrap_lost, rep.bootstrap_pending),
        (1, 1, 0)
    );
    assert_eq!(ar.storage_at(A, slot(2), 11).unwrap().value, val(0));
    // s1's loss is invisible to reads (start = first_seen), but recorded.
    assert_eq!(
        ar.boot_state(A, slot(1)).unwrap(),
        Some(bal_archive::BootState::Lost { first_seen: 10 })
    );
}

#[tokio::test]
async fn reorg_rolls_back_and_reapplies() {
    let (world, chain) = world();
    let (ar, _d) = open(ArchiveConfig::default());
    ar.watch(A, 10).unwrap();
    ar.sync(&chain, Some(&world)).await.unwrap();
    assert_eq!(ar.storage_at(A, slot(1), 14).unwrap().value, val(300));

    // Competing branch from 13: block 13' writes s1=999, 14' writes s2=6.
    chain.truncate(12);
    chain.push(13, A, &[(1, 999)], world.root_at(13), 1);
    chain.push(14, A, &[(2, 6)], world.root_at(14), 1);

    let rep = ar.sync(&chain, Some(&world)).await.unwrap();
    assert_eq!(rep.reorged_to, Some(12));
    assert_eq!(rep.blocks_applied, 2);
    assert_eq!(ar.storage_at(A, slot(1), 13).unwrap().value, val(999));
    assert_eq!(ar.storage_at(A, slot(1), 14).unwrap().value, val(999));
    assert_eq!(ar.storage_at(A, slot(2), 14).unwrap().value, val(6));
    assert_eq!(ar.storage_at(A, slot(2), 13).unwrap().value, val(5));
    assert_eq!(ar.changed_slots(A, 14).unwrap(), vec![slot(2)]);
    let h: Vec<u64> = ar
        .history(A, slot(1), 10..15)
        .unwrap()
        .into_iter()
        .map(|e| e.block)
        .collect();
    assert_eq!(h, vec![10, 12, 13]);
}

#[tokio::test]
async fn watch_rules_and_verification() {
    let (world, chain) = world();
    let (ar, _d) = open(ArchiveConfig::default());
    assert!(matches!(ar.watch(A, 0), Err(ArchiveError::InvalidStart(0))));
    ar.watch(A, 10).unwrap();
    ar.sync(&chain, Some(&world)).await.unwrap();
    assert!(matches!(
        ar.watch(B, 12),
        Err(ArchiveError::StartInPast {
            from_block: 12,
            head: 14
        })
    ));
    ar.watch(B, 15).unwrap();

    // A block whose BAL does not match its header hash must stop sync.
    {
        let mut blocks = chain.blocks.lock().unwrap();
        let mut b = blocks[&14].clone();
        b.header.number = 15;
        b.header.parent_hash = blocks[&14].header.hash;
        b.header.hash = alloy_primitives::B256::repeat_byte(0x15);
        b.header.block_access_list_hash = Some(alloy_primitives::B256::repeat_byte(0xEE));
        blocks.insert(15, b);
    }
    let err = ar.sync(&chain, Some(&world)).await.unwrap_err();
    assert!(
        matches!(err, ArchiveError::Verification { block: 15, .. }),
        "{err}"
    );
    assert_eq!(
        ar.head().unwrap().map(|h| h.0),
        Some(14),
        "nothing applied past the bad block"
    );
}

#[tokio::test]
#[allow(clippy::reversed_empty_ranges)]
async fn inverted_history_range_is_an_error() {
    let (world, chain) = world();
    let (ar, _d) = open(ArchiveConfig::default());
    ar.watch(A, 10).unwrap();
    ar.sync(&chain, Some(&world)).await.unwrap();
    assert_eq!(
        ar.history(A, slot(1), 13..11).unwrap_err(),
        NotAvailable::InvalidRange { start: 13, end: 11 }
    );
    assert_eq!(
        ar.history(A, slot(1), 12..12).unwrap_err(),
        NotAvailable::InvalidRange { start: 12, end: 12 }
    );
}

/// A node that answers for slots nobody asked about must not be able to
/// plant values under those keys.
struct LyingWorld<'a>(&'a World);

#[async_trait::async_trait]
impl bal_source::StateSource for LyingWorld<'_> {
    async fn proof(
        &self,
        addr: Address,
        slots: &[alloy_primitives::B256],
        block: u64,
    ) -> bal_source::Result<bal_source::AccountProof> {
        let mut with_extra = slots.to_vec();
        with_extra.push(slot(1)); // s1: already Done in the archive
        self.0.proof(addr, &with_extra, block).await
    }
}

#[tokio::test]
async fn proof_with_unrequested_slot_is_rejected() {
    let (world, chain) = world();
    let (ar, _d) = open(ArchiveConfig::default());
    ar.watch(A, 10).unwrap();
    ar.sync(&chain, Some(&world)).await.unwrap();
    let before = ar.storage_at(A, slot(1), 10).unwrap();
    let err = ar
        .bootstrap_slot(&LyingWorld(&world), A, slot(3))
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            ArchiveError::Proof(bal_source::ProofError::UnexpectedSlot(_))
        ),
        "{err}"
    );
    assert_eq!(
        ar.storage_at(A, slot(1), 10).unwrap(),
        before,
        "s1 untouched"
    );
    assert_eq!(
        ar.storage_at(A, slot(3), 10).unwrap_err(),
        NotAvailable::NotBootstrapped
    );
}

#[tokio::test]
async fn lazy_bootstrap_never_stores_a_post_value() {
    let (world, chain) = world();
    let (ar, _d) = open(ArchiveConfig {
        bootstrap_window: 0,
        ..Default::default()
    });
    ar.watch(A, 10).unwrap();
    // No state source: s2 (first change at 12) ends up Lost.
    ar.sync(&chain, None).await.unwrap();
    ar.sync(&chain, Some(&world)).await.unwrap();
    assert!(matches!(
        ar.storage_at(A, slot(2), 11),
        Err(NotAvailable::BootstrapLost { .. })
    ));
    // A lazy bootstrap at head 14 would prove the POST-change value 5; it must be refused.
    ar.bootstrap_slot(&world, A, slot(2)).await.unwrap();
    assert!(matches!(
        ar.storage_at(A, slot(2), 11),
        Err(NotAvailable::BootstrapLost { .. })
    ));
}

#[tokio::test]
async fn watch_below_in_flight_block_is_refused() {
    let (world, chain) = world();
    let (ar, _d) = open(ArchiveConfig::default());
    ar.watch(A, 10).unwrap();
    ar.sync(&chain, Some(&world)).await.unwrap();
    // Idle: the floor is the head.
    assert!(matches!(
        ar.watch(B, 14),
        Err(ArchiveError::StartInPast { head: 14, .. })
    ));
    ar.watch(B, 15).unwrap();
}

#[tokio::test]
async fn reorg_below_start_drops_proofs() {
    let (world, chain) = world();
    let (ar, _d) = open(ArchiveConfig::default());
    // B (never touched) starts at 10 so headers 10.. are retained; A starts at 12.
    ar.watch(B, 10).unwrap();
    ar.watch(A, 12).unwrap();
    ar.sync(&chain, Some(&world)).await.unwrap();
    // s1 proven at 11 (pre-value 100), s2 proven at 11 (0).
    assert_eq!(ar.storage_at(A, slot(1), 12).unwrap().value, val(200));
    ar.bootstrap_slot(&world, A, slot(3)).await.unwrap();
    assert_eq!(
        ar.storage_at(A, slot(3), 12).unwrap().provenance,
        Provenance::Proof
    );

    // Fork at 10, below start - 1 = 11: every proof for A is suspect.
    chain.truncate(10);
    for b in 11..=14 {
        chain.push(b, A, &[], world.root_at(b), 7);
    }
    let rep = ar.sync(&chain, Some(&world)).await.unwrap();
    assert_eq!(rep.reorged_to, Some(10));
    assert_eq!(
        ar.boot_state(A, slot(3)).unwrap(),
        None,
        "orphaned-branch proof dropped"
    );
    assert_eq!(
        ar.storage_at(A, slot(3), 12).unwrap_err(),
        NotAvailable::NotBootstrapped
    );
}

/// A source that blocks inside `block()` until released, so two `sync`
/// passes can be made to overlap deterministically.
struct SlowChain<'a> {
    inner: &'a Chain,
    gate: tokio::sync::Semaphore,
}

#[async_trait::async_trait]
impl bal_source::BalSource for SlowChain<'_> {
    async fn head(&self) -> bal_source::Result<u64> {
        self.inner.head().await
    }
    async fn finalized(&self) -> bal_source::Result<u64> {
        self.inner.finalized().await
    }
    async fn block(&self, n: u64) -> bal_source::Result<bal_source::SourcedBlock> {
        let _p = self.gate.acquire().await.expect("semaphore open");
        self.inner.block(n).await
    }
}

#[tokio::test]
async fn concurrent_sync_is_refused_then_allowed() {
    let (world, chain) = world();
    let (ar, _d) = open(ArchiveConfig::default());
    ar.watch(A, 10).unwrap();
    let slow = SlowChain {
        inner: &chain,
        gate: tokio::sync::Semaphore::new(0),
    };
    let first = ar.sync(&slow, Some(&world));
    tokio::pin!(first);
    // Drive the first pass until it parks on the gate, then start a second one.
    tokio::select! {
        _ = &mut first => panic!("first pass must be parked"),
        _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {}
    }
    let second = ar.sync(&chain, Some(&world)).await;
    assert!(
        matches!(second, Err(ArchiveError::SyncInProgress)),
        "{second:?}"
    );
    slow.gate.add_permits(100);
    first.await.unwrap();
    // The slot is released: a later pass works and is a no-op.
    let rep = ar.sync(&chain, Some(&world)).await.unwrap();
    assert_eq!(rep.blocks_applied, 0);
    assert_eq!(ar.head().unwrap().map(|h| h.0), Some(14));
}

/// A primary that never serves proofs (public gateway, window 0).
struct NoProofs;

#[async_trait::async_trait]
impl bal_source::StateSource for NoProofs {
    async fn proof(
        &self,
        _: Address,
        _: &[alloy_primitives::B256],
        block: u64,
    ) -> bal_source::Result<bal_source::AccountProof> {
        Err(bal_source::SourceError::Rpc {
            code: -32602,
            message: format!("distance to target block {block} exceeds maximum proof window"),
        })
    }
}

#[tokio::test]
async fn backup_source_rescues_bootstrap() {
    let (world, chain) = world();
    // Without a backup: nothing can be proven, slots end up pending.
    let (ar, _d) = open(ArchiveConfig::default());
    ar.watch(A, 10).unwrap();
    let rep = ar.sync(&chain, Some(&NoProofs)).await.unwrap();
    assert_eq!((rep.bootstrapped, rep.bootstrap_pending), (0, 2));

    // With the same primary plus a backup that has the state: proven,
    // and verified against the header exactly like a primary proof.
    let (ar2, _d2) = open(ArchiveConfig::default());
    ar2.watch(A, 10).unwrap();
    let with_backup = bal_source::Fallback::new(NoProofs, &world);
    let rep = ar2.sync(&chain, Some(&with_backup)).await.unwrap();
    assert_eq!((rep.bootstrapped, rep.bootstrap_pending), (2, 0));
    let z = ar2.storage_at(A, slot(2), 11).unwrap();
    assert_eq!((z.value, z.provenance), (val(0), Provenance::Proof));

    // A lying backup is caught the same way as a lying primary.
    let lying = bal_source::Fallback::new(NoProofs, LyingWorld(&world));
    let err = ar2.bootstrap_slot(&lying, A, slot(3)).await.unwrap_err();
    assert!(matches!(err, ArchiveError::Proof(_)), "{err}");
}

#[tokio::test]
async fn watch_twice_and_config_mismatch() {
    let (world, chain) = world();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.redb");
    {
        let ar = Archive::open_with(path.clone(), ArchiveConfig::default()).unwrap();
        ar.watch(A, 10).unwrap();
        ar.watch(A, 10).unwrap(); // same start: idempotent
        assert!(matches!(
            ar.watch(A, 11),
            Err(ArchiveError::AlreadyWatched { from_block: 10, .. })
        ));
        ar.sync(&chain, Some(&world)).await.unwrap();
    }
    // Reopening with a different creation-time option is refused.
    let err = Archive::open_with(
        path.clone(),
        ArchiveConfig {
            full_detail: true,
            ..Default::default()
        },
    )
    .err()
    .expect("full_detail mismatch must be refused");
    assert!(
        matches!(
            err,
            ArchiveError::ConfigMismatch {
                option: "full_detail",
                ..
            }
        ),
        "{err}"
    );
    // Same options: fine, data intact.
    let ar = Archive::open_with(path, ArchiveConfig::default()).unwrap();
    assert_eq!(ar.storage_at(A, slot(1), 14).unwrap().value, val(300));
}

#[tokio::test]
async fn unwatch_removes_everything() {
    let (world, chain) = world();
    let (ar, _d) = open(ArchiveConfig::default());
    ar.watch(A, 10).unwrap();
    ar.sync(&chain, Some(&world)).await.unwrap();
    ar.unwatch(A).unwrap();
    assert_eq!(
        ar.storage_at(A, slot(1), 12).unwrap_err(),
        NotAvailable::NotWatched(A)
    );
    assert!(ar.watchlist().unwrap().is_empty());
    assert_eq!(ar.boot_state(A, slot(1)).unwrap(), None);
}
