//! The sync loop: fetch → verify → detect reorg → apply → early bootstrap.
//! Memory: one block's BAL at a time.

use crate::{Archive, ArchiveError, Result};
use alloy_primitives::{Address, B256};
use bal_source::{check_requested, verify_account_proof, BalSource, SourceError, StateSource};
use tracing::{debug, info, warn};

/// Block hashes retained behind the head when the source has no
/// `finalized` tag. Deeper reorgs than this are refused as
/// [`ArchiveError::ReorgBeyondHorizon`] rather than guessed.
pub const REORG_HORIZON_FALLBACK: u64 = 4096;

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
        if !self.begin_sync() {
            return Err(ArchiveError::SyncInProgress);
        }
        let result = self.sync_inner(source, state).await;
        self.sync_idle();
        result
    }

    async fn sync_inner<S: BalSource + ?Sized>(
        &self,
        source: &S,
        state: Option<&dyn StateSource>,
    ) -> Result<SyncReport> {
        let mut report = SyncReport::default();
        let Some(earliest_start) = self.watchlist()?.iter().map(|(_, s)| *s).min() else {
            return Ok(report);
        };
        let src_head = source.head().await?;
        // Reorg horizon: the node's `finalized` tag, or — for nodes without
        // one — a fixed distance, so the header table cannot grow without
        // bound. Only the retained range can be rolled back to.
        let finalized = match source.finalized().await {
            // A node cannot be allowed to prune our head by claiming a
            // finalized block above it.
            Ok(f) => Some(f.min(src_head)),
            Err(e) => {
                debug!(%e, "no finalized tag; using fixed reorg horizon");
                Some(src_head.saturating_sub(REORG_HORIZON_FALLBACK))
            }
        };

        let mut next = match self.head()? {
            Some((h, hash)) => {
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
                    fork + 1
                } else {
                    h + 1
                }
            }
            None => earliest_start,
        };
        report.from = Some(next);

        // Guards against a source that keeps contradicting itself about the
        // same parent link (pooled upstreams on different forks).
        let mut last_fork: Option<u64> = None;

        while next <= src_head {
            let blk = match source.block(next).await {
                Ok(b) => b,
                Err(SourceError::BlockNotFound(n)) if n >= src_head.saturating_sub(2) => {
                    debug!(block = n, "not yet available upstream; stopping this pass");
                    break;
                }
                Err(e) => return Err(e.into()),
            };
            let header = blk.header.clone();

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
                finalized.map(|f| f.min(src_head.saturating_sub(self.config.bootstrap_window + 1)));
            let (fresh, written) =
                self.apply_block(&header, &blk.bal, &watches, verified, prune_below)?;
            report.slots_written += written;
            report.blocks_applied += 1;
            report.to = Some(next);
            debug!(block = next, written, fresh = fresh.len(), "applied");

            // Early bootstrap: prove pre-values at `next - 1` while the node
            // still has that state. Failure leaves the slot Pending.
            if !fresh.is_empty() {
                let prev_root = match state {
                    None => None,
                    Some(_) => self.root_of(source, next - 1).await,
                };
                for (addr, start, slots) in &fresh {
                    match (state, prev_root) {
                        (Some(st), Some(root)) => {
                            match self
                                .bootstrap_at(st, root, *addr, *start, slots, next - 1)
                                .await
                            {
                                Ok(n) => report.bootstrapped += n,
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

        info!(?report, "sync done");
        Ok(report)
    }

    /// State root of `block`: from the stored headers, or fetched from the
    /// source and remembered. `None` if neither works.
    async fn root_of<S: BalSource + ?Sized>(&self, source: &S, block: u64) -> Option<B256> {
        match self.header_at(block) {
            Ok(Some((_, root))) => return Some(root),
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
                Some(h.state_root)
            }
            Err(e) => {
                warn!(block, %e, "cannot fetch header for proof root");
                None
            }
        }
    }

    /// Walk back from `from` until a stored hash matches the source.
    async fn find_fork<S: BalSource + ?Sized>(&self, source: &S, from: u64) -> Result<u64> {
        let mut b = from;
        loop {
            let Some((stored, _)) = self.header_at(b)? else {
                return Err(ArchiveError::ReorgBeyondHorizon(b));
            };
            let live = source.header(b).await?.hash;
            if live == stored {
                return Ok(b);
            }
            if b == 0 {
                return Err(ArchiveError::ReorgBeyondHorizon(0));
            }
            b -= 1;
        }
    }

    /// Fetch and verify proofs for exactly `slots` at `block`, store the
    /// ones that are genuine pre-values. Returns how many were stored.
    async fn bootstrap_at(
        &self,
        state: &dyn StateSource,
        state_root: B256,
        addr: Address,
        start: u64,
        slots: &[B256],
        block: u64,
    ) -> Result<usize> {
        let proof = state.proof(addr, slots, block).await?;
        check_requested(slots, &proof)?;
        let values = verify_account_proof(state_root, &proof)?;
        let values: Vec<(B256, B256)> = values
            .into_iter()
            .map(|(k, v)| (k, B256::from(v.to_be_bytes::<32>())))
            .collect();
        self.put_bootstrap(addr, start, block, &values)
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
        let mut groups: Vec<(Address, u64, Vec<B256>)> = Vec::new();
        for (addr, slot, first_seen) in pending {
            match groups
                .iter_mut()
                .find(|(a, f, _)| *a == addr && *f == first_seen)
            {
                Some(g) => g.2.push(slot),
                None => groups.push((addr, first_seen, vec![slot])),
            }
        }
        for (addr, first_seen, slots) in groups {
            let Some(start) = start_of(addr) else {
                continue;
            };
            let at = first_seen - 1;
            if src_head.saturating_sub(at) > self.config.bootstrap_window {
                self.mark_lost(addr, &slots, first_seen)?;
                lost += slots.len();
                continue;
            }
            let Some(root) = self.root_of(source, at).await else {
                still += slots.len();
                continue;
            };
            match self
                .bootstrap_at(state, root, addr, start, &slots, at)
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
    /// is skipped if a change at or before that head has been recorded in
    /// the meantime — a sync running concurrently cannot turn a post-value
    /// into a stored pre-value.
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
        let (_, root) = self
            .header_at(head)?
            .ok_or(ArchiveError::ReorgBeyondHorizon(head))?;
        self.bootstrap_at(state, root, addr, start, &[slot], head)
            .await?;
        Ok(())
    }
}
