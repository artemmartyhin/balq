//! Backfill: extend an address's history *backwards* from its watch start by
//! reading older blocks' BALs. No proofs involved — a BAL is committed to by
//! its own header, and headers are chained by `parent_hash` up to the block
//! the archive already holds, so every record written here is verified
//! exactly like one written by the forward sync.
//!
//! Walking back answers "what was in this slot before its first recorded
//! change": the last write before it. Reaching the contract's creation
//! answers it for every slot at once (no storage before creation, EIP-7610).
//! Neither needs `eth_getProof`, so neither is bounded by the node's state
//! window; the only bound is how far back the node still serves blocks.

use crate::keys::{blockidx_key, decode_boot, encode_boot, encode_value, slot_key, slot_prefix};
use crate::{
    anchor_key, creation_in, settle_created, Archive, ArchiveError, BootState, Provenance, Result,
    BLOCKIDX, BOOT, CREATED, META, PENDING, SLOTS, WATCH,
};
use alloy_primitives::{Address, B256};
use bal_source::{BalSource, SourceError, SourcedBlock};
use redb::ReadableTable;
use std::collections::BTreeSet;
use tracing::{debug, info};

/// What to walk back to.
#[derive(Debug, Clone, Default)]
pub struct BackfillOpts {
    /// Lowest block to read (inclusive). `None`: as far as the node serves,
    /// or the contract's creation, whichever comes first.
    pub to: Option<u64>,
    /// Stop after this many blocks; the caller loops and reports progress.
    /// Each call commits what it read, so a stopped backfill resumes where it
    /// left off.
    pub max_blocks: Option<u64>,
    /// Stop as soon as every slot that currently has an unknown pre-value
    /// (pending or lost) has found its last earlier write. Slots that never
    /// changed since the start are not waited for — only creation settles
    /// those.
    pub resolve_only: bool,
}

/// Why a backfill call returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackfillStop {
    /// Reached `opts.to` (or block 1).
    Target,
    /// The contract's creation was seen at this block. History is complete.
    Creation(u64),
    /// `resolve_only` and every unknown pre-value has been found.
    Resolved,
    /// `max_blocks` read; call again to continue.
    Budget,
    /// This block's header carries no BAL hash: history before the BAL fork
    /// cannot be read from blocks. Proofs against an archive are the only
    /// way further back.
    PreBal(u64),
    /// The node does not serve this block (history expiry, or a pruned
    /// backup). A source that still has old blocks can continue from here.
    HistoryUnavailable(u64),
    /// Nothing to do: the address is already known to be created, or the
    /// start is already at the target.
    Nothing,
}

/// What one [`Archive::backfill`] call did.
#[derive(Debug, Clone)]
pub struct BackfillReport {
    /// Watch start before this call.
    pub from: u64,
    /// Watch start after this call (the lowest block now covered).
    pub to: u64,
    /// Blocks read and verified.
    pub blocks_scanned: u64,
    /// Slot records written.
    pub records_written: usize,
    /// Slots whose previously unknown pre-value was found in this call.
    pub slots_resolved: usize,
    /// Slots whose pre-value is still unknown after this call.
    pub unresolved: usize,
    /// Creation block, if known after this call.
    pub created_at: Option<u64>,
    /// Why the call returned.
    pub stopped: BackfillStop,
}

struct Guard<'a>(&'a Archive);

impl Drop for Guard<'_> {
    fn drop(&mut self) {
        self.0.sync_idle();
    }
}

impl Archive {
    /// Extend `addr`'s history backwards from its watch start. Takes the
    /// sync slot (a concurrent `sync` is refused and vice versa); reads stay
    /// available throughout. Every block is verified: header chained to the
    /// one above it, BAL hashed against the header.
    ///
    /// The archive must have synced to at least the watch start, so that the
    /// block above the first one read is one the archive holds.
    pub async fn backfill<S: BalSource + ?Sized>(
        &self,
        source: &S,
        addr: Address,
        opts: BackfillOpts,
    ) -> Result<BackfillReport> {
        if !self.begin_sync() {
            return Err(ArchiveError::SyncInProgress);
        }
        let _guard = Guard(self);
        self.backfill_inner(source, addr, opts).await
    }

    async fn backfill_inner<S: BalSource + ?Sized>(
        &self,
        source: &S,
        addr: Address,
        opts: BackfillOpts,
    ) -> Result<BackfillReport> {
        let start = self.start_of(addr)?.ok_or(ArchiveError::NotWatched(addr))?;
        let head = self.head()?.map(|(h, _)| h).unwrap_or(0);
        if head < start {
            return Err(ArchiveError::HeadBelowStart { head, start });
        }
        let created = self.created_at(addr)?;
        let mut unresolved = self.unknown_pre_values(addr)?;
        let mut report = BackfillReport {
            from: start,
            to: start,
            blocks_scanned: 0,
            records_written: 0,
            slots_resolved: 0,
            unresolved: unresolved.len(),
            created_at: created,
            stopped: BackfillStop::Nothing,
        };
        let target = opts.to.unwrap_or(1).max(1);
        if created.is_some() || target >= start || (opts.resolve_only && unresolved.is_empty()) {
            return Ok(report);
        }

        // The block above the first one we read must be the block the
        // archive holds — by stored hash near the head, by the backfill
        // anchor below it. Otherwise the start block was reorged and the
        // forward sync has to sort that out first.
        let above = source.header(start).await?;
        if above.number != start {
            return Err(wrong_block(start, above.number));
        }
        let known = match self.header_at(start)? {
            Some((h, _)) => Some(h),
            None => self.anchor(addr)?,
        };
        if let Some(h) = known {
            if h != above.hash {
                return Err(ArchiveError::StartReplaced(start));
            }
        }
        let mut expect = above.parent_hash;

        let mut cur = start - 1;
        loop {
            if cur < target {
                report.stopped = BackfillStop::Target;
                break;
            }
            if opts.max_blocks.is_some_and(|m| report.blocks_scanned >= m) {
                report.stopped = BackfillStop::Budget;
                break;
            }
            let blk = match source.block(cur).await {
                Ok(b) => b,
                Err(SourceError::BlockNotFound(_)) | Err(SourceError::NoBal(_)) => {
                    report.stopped = BackfillStop::HistoryUnavailable(cur);
                    break;
                }
                Err(e) => return Err(e.into()),
            };
            if blk.header.number != cur {
                return Err(wrong_block(cur, blk.header.number));
            }
            if blk.header.hash != expect {
                return Err(ArchiveError::InconsistentSource(cur + 1));
            }
            let Some(bal_hash) = blk.header.block_access_list_hash else {
                report.stopped = BackfillStop::PreBal(cur);
                break;
            };
            blk.bal
                .verify(bal_hash)
                .map_err(|err| ArchiveError::Verification { block: cur, err })?;

            let (written, resolved, is_creation) =
                self.backfill_block(addr, cur, &blk, &mut unresolved)?;
            report.blocks_scanned += 1;
            report.records_written += written;
            report.slots_resolved += resolved;
            report.to = cur;
            debug!(%addr, block = cur, written, "backfilled");
            expect = blk.header.parent_hash;

            if is_creation {
                report.created_at = Some(cur);
                unresolved.clear();
                report.stopped = BackfillStop::Creation(cur);
                break;
            }
            if opts.resolve_only && unresolved.is_empty() {
                report.stopped = BackfillStop::Resolved;
                break;
            }
            cur -= 1;
        }
        report.unresolved = unresolved.len();
        info!(?report, "backfill done");
        Ok(report)
    }

    /// Slots of `addr` whose pre-value is pending or lost.
    fn unknown_pre_values(&self, addr: Address) -> Result<BTreeSet<B256>> {
        let rtx = self.db.begin_read()?;
        let boot = rtx.open_table(BOOT)?;
        let mut out = BTreeSet::new();
        for k in crate::collect_prefix_keys(&boot, addr.as_slice())? {
            let state = boot.get(k.as_slice())?.and_then(|v| decode_boot(v.value()));
            if matches!(
                state,
                Some(BootState::Pending { .. }) | Some(BootState::Lost { .. })
            ) {
                out.insert(B256::from_slice(&k[20..]));
            }
        }
        Ok(out)
    }

    fn anchor(&self, addr: Address) -> Result<Option<B256>> {
        let rtx = self.db.begin_read()?;
        let meta = rtx.open_table(META)?;
        Ok(meta
            .get(anchor_key(addr).as_str())?
            .and_then(|v| (v.value().len() == 32).then(|| B256::from_slice(v.value()))))
    }

    /// Write one older block for `addr` and move its start down to `block`,
    /// in one transaction. Returns `(records written, pre-values resolved,
    /// creation seen)`.
    fn backfill_block(
        &self,
        addr: Address,
        block: u64,
        blk: &SourcedBlock,
        unresolved: &mut BTreeSet<B256>,
    ) -> Result<(usize, usize, bool)> {
        let mut written = 0;
        let mut resolved = 0;
        let mut is_creation = false;
        let txn = self.db.begin_write()?;
        {
            let mut watch = txn.open_table(WATCH)?;
            // Unwatched while we were fetching: write nothing.
            if watch.get(addr.as_slice())?.map(|v| v.value()) != Some(block + 1) {
                return Ok((0, 0, false));
            }
            if let Some(acc) = blk.bal.account(&addr) {
                let mut slots = txn.open_table(SLOTS)?;
                let mut idx = txn.open_table(BLOCKIDX)?;
                let mut boot = txn.open_table(BOOT)?;
                let mut pending = txn.open_table(PENDING)?;
                for sc in &acc.storage_changes {
                    let slot = sc.slot_b256();
                    if self.config.full_detail {
                        for ch in &sc.changes {
                            slots.insert(
                                slot_key(addr, slot, block, ch.block_access_index).as_slice(),
                                encode_value(Provenance::Bal, ch.value_b256()).as_slice(),
                            )?;
                            written += 1;
                        }
                    } else {
                        let ch = sc.final_change();
                        slots.insert(
                            slot_key(addr, slot, block, ch.block_access_index).as_slice(),
                            encode_value(Provenance::Bal, ch.value_b256()).as_slice(),
                        )?;
                        written += 1;
                    }
                    idx.insert(blockidx_key(addr, block, slot).as_slice(), ())?;
                    // The slot's earliest known change is now this block; what
                    // is unknown moved below it.
                    let key = slot_prefix(addr, slot);
                    let next = match boot
                        .get(key.as_slice())?
                        .and_then(|v| decode_boot(v.value()))
                    {
                        Some(BootState::Done) => None,
                        Some(BootState::Pending { .. }) => {
                            Some(BootState::Pending { first_seen: block })
                        }
                        Some(BootState::Lost { .. }) => Some(BootState::Lost { first_seen: block }),
                        None => Some(BootState::Pending { first_seen: block }),
                    };
                    if let Some(state) = next {
                        boot.insert(key.as_slice(), encode_boot(state).as_slice())?;
                        if matches!(state, BootState::Pending { .. }) {
                            pending.insert(key.as_slice(), block)?;
                        }
                    }
                    if unresolved.remove(&slot) {
                        resolved += 1;
                    }
                }
                if creation_in(acc) {
                    txn.open_table(CREATED)?.insert(addr.as_slice(), block)?;
                    settle_created(&mut boot, &mut pending, addr)?;
                    is_creation = true;
                }
            }
            watch.insert(addr.as_slice(), block)?;
            txn.open_table(META)?
                .insert(anchor_key(addr).as_str(), blk.header.hash.as_slice())?;
        }
        txn.commit()?;
        Ok((written, resolved, is_creation))
    }
}

fn wrong_block(asked: u64, got: u64) -> ArchiveError {
    ArchiveError::Source(SourceError::Malformed(format!(
        "asked for block {asked}, source answered with block {got}"
    )))
}
