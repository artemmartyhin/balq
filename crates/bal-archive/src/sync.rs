//! The sync loop: fetch → verify → detect reorg → apply → early bootstrap.
//! Memory: one block's BAL at a time.

use crate::backfill::FETCH_AHEAD;
use crate::{Archive, ArchiveError, Result};
use alloy_primitives::{Address, B256};
use bal_source::{
    check_requested, verify_account_proof, BalSource, SourceError, SourcedBlock, StateSource,
};
use futures::future::join_all;
use std::collections::{BTreeMap, VecDeque};
use tracing::{debug, info, warn};

/// Block hashes retained behind the head when the source has no
/// `finalized` tag, and the deepest reorg `find_fork` will walk. Deeper
/// reorgs are refused as [`ArchiveError::ReorgBeyondHorizon`] rather than
/// guessed or walked one RPC call at a time forever.
pub const REORG_HORIZON_FALLBACK: u64 = 4096;

/// Slots per `eth_getProof` call. Nodes refuse very large requests; a block
/// touching thousands of fresh slots must not turn all of them into
/// `Pending` because one request was too big.
const PROOF_CHUNK: usize = 256;

/// What one [`Archive::sync`] pass did.
#[derive(Debug, Default, Clone)]
pub struct SyncReport {
    /// First block this pass tried to apply, if any.
    pub from: Option<u64>,
    /// Last block applied, if any.
    pub to: Option<u64>,
    /// Blocks applied in this pass.
    pub blocks_applied: u64,
    /// Fork point, if a reorg was rolled back.
    pub reorged_to: Option<u64>,
    /// Slot records written (one per changed slot, or per change with `full_detail`).
    pub slots_written: usize,
    /// Pre-values proven and stored in this pass.
    pub bootstrapped: usize,
    /// Slots whose pre-value is still awaiting a proof.
    pub bootstrap_pending: usize,
    /// Slots whose pre-value became unobtainable in this pass.
    pub bootstrap_lost: usize,
    /// Blocks applied without a BAL hash (only with `allow_unverified`).
    pub unverified_blocks: u64,
    /// The source's head when the pass started; `to == source_head` means
    /// the archive caught up.
    pub source_head: Option<u64>,
    /// Every account that appeared in an applied block's BAL, deduplicated
    /// and capped at [`TOUCHED_CAP`]. Useful as candidate mapping keys when
    /// naming what changed (senders and recipients are in the BAL).
    pub touched: Vec<Address>,
}

/// Bound on [`SyncReport::touched`]; beyond it the list stops growing.
pub const TOUCHED_CAP: usize = 4096;

/// Releases the sync slot when the pass ends — by return, error, or the
/// future being dropped.
struct SyncGuard<'a>(&'a Archive);

impl Drop for SyncGuard<'_> {
    fn drop(&mut self) {
        self.0.sync_idle();
    }
}

impl Archive {
    /// Sync from the archive head (or the earliest watch start) to the
    /// source head. `state` enables early bootstrap; without it, first-seen
    /// slots are left `Pending` and will be picked up by a later sync that
    /// has a state source — if the window has not passed by then.
    ///
    /// A block the source reports as missing near its head ends the pass
    /// quietly (pooled gateways lag); the next pass picks it up.
    pub async fn sync<S: BalSource + ?Sized>(
        &self,
        source: &S,
        state: Option<&dyn StateSource>,
    ) -> Result<SyncReport> {
        self.sync_step(source, state, None).await
    }

    /// [`Archive::sync`] that applies at most `max_blocks` and returns, so a
    /// caller can show progress and keep reading between steps. The pass is
    /// complete when `blocks_applied` is 0 or `to == source_head`.
    pub async fn sync_step<S: BalSource + ?Sized>(
        &self,
        source: &S,
        state: Option<&dyn StateSource>,
        max_blocks: Option<u64>,
    ) -> Result<SyncReport> {
        if !self.begin_sync() {
            return Err(ArchiveError::SyncInProgress);
        }
        let _guard = SyncGuard(self);
        self.sync_inner(source, state, max_blocks).await
    }

    async fn sync_inner<S: BalSource + ?Sized>(
        &self,
        source: &S,
        state: Option<&dyn StateSource>,
        max_blocks: Option<u64>,
    ) -> Result<SyncReport> {
        let mut report = SyncReport::default();
        // Claim the start block before the first await so that no `watch()`
        // below it can be accepted while we are fetching.
        let Some(mut next) = self.claim_start()? else {
            return Ok(report);
        };
        let src_head = source.head().await?;
        report.source_head = Some(src_head);
        // Reorg horizon: the node's `finalized` tag, clamped to its head
        // (a node cannot prune our head by claiming a finalized block above
        // it) and never further back than the fallback horizon (so the
        // header table cannot grow without bound if `finalized` stalls).
        let horizon_floor = src_head.saturating_sub(REORG_HORIZON_FALLBACK);
        let finalized = match source.finalized().await {
            Ok(f) => f.min(src_head).max(horizon_floor),
            Err(e) => {
                debug!(%e, "no finalized tag; using fixed reorg horizon");
                horizon_floor
            }
        };

        if let Some((h, hash)) = self.head()? {
            // Head still canonical? Header only — the BAL is not needed.
            let cur = match source.header(h).await {
                Ok(hdr) => hdr,
                Err(SourceError::BlockNotFound(_)) => {
                    debug!(head = h, "head not served by upstream yet; skipping pass");
                    return Ok(report);
                }
                Err(e) => return Err(e.into()),
            };
            if cur.hash != hash {
                let fork = self.find_fork(source, h).await?;
                warn!(head = h, fork, "reorg detected at start of sync");
                self.rollback_to(fork)?;
                report.reorged_to = Some(fork);
                next = fork + 1;
            }
        }
        report.from = Some(next);

        // Guards against a source that keeps contradicting itself about the
        // same parent link (pooled upstreams on different forks).
        let mut last_fork: Option<u64> = None;
        let mut touched: std::collections::BTreeSet<Address> = std::collections::BTreeSet::new();

        // Blocks are fetched FETCH_AHEAD at a time and applied in order; a
        // reorg discards what was prefetched past the fork.
        let mut queue: VecDeque<(u64, bal_source::Result<SourcedBlock>)> = VecDeque::new();
        while next <= src_head {
            if max_blocks.is_some_and(|m| report.blocks_applied >= m) {
                break;
            }
            if queue.front().map(|(n, _)| *n) != Some(next) {
                queue.clear();
                let mut n = FETCH_AHEAD.min(src_head - next + 1);
                if let Some(m) = max_blocks {
                    n = n.min(m.saturating_sub(report.blocks_applied)).max(1);
                }
                let numbers: Vec<u64> = (0..n).map(|i| next + i).collect();
                let fetched = join_all(numbers.iter().map(|&b| source.block(b))).await;
                queue.extend(numbers.into_iter().zip(fetched));
            }
            let Some((_, res)) = queue.pop_front() else {
                break;
            };
            let blk = match res {
                Ok(b) => b,
                Err(SourceError::BlockNotFound(n)) if n >= src_head.saturating_sub(2) => {
                    debug!(block = n, "not yet available upstream; stopping this pass");
                    break;
                }
                Err(e) => return Err(e.into()),
            };
            let header = blk.header.clone();
            if header.number != next {
                return Err(ArchiveError::Source(SourceError::Malformed(format!(
                    "asked for block {next}, source answered with block {}",
                    header.number
                ))));
            }

            // Parent linkage against what we stored.
            if let Some((stored_hash, _)) = self.header_at(next - 1)? {
                if stored_hash != header.parent_hash {
                    let fork = self.find_fork(source, next - 1).await?;
                    if last_fork == Some(fork) {
                        return Err(ArchiveError::InconsistentSource(next));
                    }
                    last_fork = Some(fork);
                    warn!(block = next, fork, "reorg detected mid-sync");
                    self.rollback_to(fork)?;
                    report.reorged_to = Some(fork);
                    next = fork + 1;
                    continue;
                }
            }

            let verified = match header.block_access_list_hash {
                Some(expected) => {
                    blk.bal
                        .verify(expected)
                        .map_err(|err| ArchiveError::Verification { block: next, err })?;
                    true
                }
                None if self.config.allow_unverified => {
                    report.unverified_blocks += 1;
                    warn!(
                        block = next,
                        "applying block without BAL hash (allow_unverified)"
                    );
                    false
                }
                None => return Err(ArchiveError::NoBalHash(next)),
            };

            // Snapshot the watchlist for this block under the watch gate:
            // a `watch()` racing with us either lands in this snapshot or is
            // refused for this block.
            let watches = self.watchlist_for(next)?;
            let prune_below =
                Some(finalized.min(src_head.saturating_sub(self.config.bootstrap_window + 1)));
            if touched.len() < TOUCHED_CAP {
                touched.extend(blk.bal.accounts.iter().map(|a| a.address));
            }
            let (fresh, written) =
                self.apply_block(&header, &blk.bal, &watches, verified, prune_below)?;
            report.slots_written += written;
            report.blocks_applied += 1;
            report.to = Some(next);
            debug!(block = next, written, fresh = fresh.len(), "applied");

            // Early bootstrap: prove pre-values at `next - 1` while the node
            // still has that state. Failure leaves the slot Pending.
            if !fresh.is_empty() {
                let prev = match state {
                    None => None,
                    Some(_) => self.header_of(source, next - 1).await,
                };
                for (addr, start, slots) in &fresh {
                    match (state, prev) {
                        (Some(st), Some((hash, root))) => {
                            match self
                                .bootstrap_at(st, root, hash, *addr, *start, slots, next - 1)
                                .await
                            {
                                Ok(n) => {
                                    report.bootstrapped += n;
                                    report.bootstrap_pending += slots.len() - n;
                                }
                                Err(e) => {
                                    warn!(%addr, block = next, %e, "early bootstrap failed; left pending");
                                    report.bootstrap_pending += slots.len();
                                }
                            }
                        }
                        _ => report.bootstrap_pending += slots.len(),
                    }
                }
            }

            next += 1;
        }

        // Retry pending bootstraps; expire those the window has passed.
        if let Some(st) = state {
            let (ok, pending, lost) = self.retry_pending(source, st, src_head).await?;
            report.bootstrapped += ok;
            report.bootstrap_pending = pending;
            report.bootstrap_lost = lost;
        }

        report.touched = touched.into_iter().take(TOUCHED_CAP).collect();
        info!(?report, "sync done");
        Ok(report)
    }

    /// `(hash, state_root)` of `block`: from the stored headers, or fetched
    /// from the source and remembered. `None` if neither works.
    async fn header_of<S: BalSource + ?Sized>(
        &self,
        source: &S,
        block: u64,
    ) -> Option<(B256, B256)> {
        match self.header_at(block) {
            Ok(Some(h)) => return Some(h),
            Ok(None) => {}
            Err(e) => {
                warn!(block, %e, "cannot read stored header");
                return None;
            }
        }
        match source.header(block).await {
            Ok(h) => {
                if let Err(e) = self.remember_header(block, h.hash, h.state_root) {
                    warn!(block, %e, "cannot remember header");
                }
                Some((h.hash, h.state_root))
            }
            Err(e) => {
                warn!(block, %e, "cannot fetch header for proof root");
                None
            }
        }
    }

    /// Walk back from `from` until a stored hash matches the source, at most
    /// [`REORG_HORIZON_FALLBACK`] blocks.
    async fn find_fork<S: BalSource + ?Sized>(&self, source: &S, from: u64) -> Result<u64> {
        let floor = from.saturating_sub(REORG_HORIZON_FALLBACK);
        let mut b = from;
        loop {
            let Some((stored, _)) = self.header_at(b)? else {
                return Err(ArchiveError::ReorgBeyondHorizon(b));
            };
            let live = source.header(b).await?.hash;
            if live == stored {
                return Ok(b);
            }
            if b == 0 || b <= floor {
                return Err(ArchiveError::ReorgBeyondHorizon(b));
            }
            b -= 1;
        }
    }

    /// Fetch and verify proofs for exactly `slots` at `block` (in chunks),
    /// store the ones that are genuine pre-values. Returns how many were
    /// stored.
    #[allow(clippy::too_many_arguments)]
    async fn bootstrap_at(
        &self,
        state: &dyn StateSource,
        state_root: B256,
        block_hash: B256,
        addr: Address,
        start: u64,
        slots: &[B256],
        block: u64,
    ) -> Result<usize> {
        let mut stored = 0;
        for chunk in slots.chunks(PROOF_CHUNK) {
            let proof = state.proof(addr, chunk, block).await?;
            check_requested(chunk, &proof)?;
            let values = verify_account_proof(state_root, &proof)?;
            let values: Vec<(B256, B256)> = values
                .into_iter()
                .map(|(k, v)| (k, B256::from(v.to_be_bytes::<32>())))
                .collect();
            stored += self.put_bootstrap(addr, start, block, block_hash, &values)?;
        }
        Ok(stored)
    }

    /// Returns `(proven, still pending, newly lost)`.
    async fn retry_pending<S: BalSource + ?Sized>(
        &self,
        source: &S,
        state: &dyn StateSource,
        src_head: u64,
    ) -> Result<(usize, usize, usize)> {
        let pending = self.pending_bootstraps()?;
        if pending.is_empty() {
            return Ok((0, 0, 0));
        }
        let watches = self.watchlist()?;
        let start_of = |a: Address| watches.iter().find(|(x, _)| *x == a).map(|(_, s)| *s);
        let (mut ok, mut still, mut lost) = (0, 0, 0);
        // Group by (addr, first_seen) → one proof call each.
        let mut groups: BTreeMap<(Address, u64), Vec<B256>> = BTreeMap::new();
        for (addr, slot, first_seen) in pending {
            groups.entry((addr, first_seen)).or_default().push(slot);
        }
        for ((addr, first_seen), slots) in groups {
            let Some(start) = start_of(addr) else {
                continue;
            };
            // `first_seen` is written by `apply_block` for blocks >= start >= 1;
            // a zero can only come from a tampered file.
            let Some(at) = first_seen.checked_sub(1) else {
                return Err(ArchiveError::Corrupt("pending first_seen"));
            };
            if src_head.saturating_sub(at) > self.config.bootstrap_window {
                self.mark_lost(addr, &slots, first_seen)?;
                lost += slots.len();
                continue;
            }
            let Some((hash, root)) = self.header_of(source, at).await else {
                still += slots.len();
                continue;
            };
            match self
                .bootstrap_at(state, root, hash, addr, start, &slots, at)
                .await
            {
                Ok(n) => {
                    ok += n;
                    still += slots.len() - n;
                }
                Err(e) => {
                    debug!(%addr, first_seen, %e, "pending bootstrap retry failed");
                    still += slots.len();
                }
            }
        }
        Ok((ok, still, lost))
    }

    /// Lazy bootstrap for a slot that never changed since watch start: prove
    /// its value at the archive head (which equals its value at `start`, by
    /// BAL completeness) and store it as the pre-value.
    ///
    /// The head is read *before* the slot's state is checked, and the write
    /// is skipped if a change at or before that head has been recorded, if
    /// the watch changed, or if the head block was replaced in the meantime —
    /// a sync running concurrently cannot turn a post-value into a stored
    /// pre-value.
    pub async fn bootstrap_slot(
        &self,
        state: &dyn StateSource,
        addr: Address,
        slot: B256,
    ) -> Result<()> {
        let start = self.start_of(addr)?.ok_or(ArchiveError::NotWatched(addr))?;
        let (head, _) = self
            .head()?
            .ok_or(ArchiveError::HeadBelowStart { head: 0, start })?;
        if head < start {
            return Err(ArchiveError::HeadBelowStart { head, start });
        }
        if self.boot_state(addr, slot)?.is_some() {
            return Ok(()); // seen or proven already; nothing to do
        }
        let (hash, root) = self
            .header_at(head)?
            .ok_or(ArchiveError::ReorgBeyondHorizon(head))?;
        self.bootstrap_at(state, root, hash, addr, start, &[slot], head)
            .await?;
        Ok(())
    }
}
