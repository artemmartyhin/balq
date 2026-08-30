//! Writes used by the sync and backfill loops: bootstrap records, rollback,
//! applying one block in one transaction.

use super::*;

impl Archive {
    /// Store pre-values proven at `proof_block` as records at `start - 1`,
    /// marking the slots `Done`. A value is written only if it is actually
    /// the pre-value: the slot is unseen, or its first recorded change is
    /// *after* `proof_block`. Anything else (already `Done`, or a proof taken
    /// at or after the first change) is skipped, so a racing sync or a node
    /// answering for the wrong slots cannot plant a post-value as a
    /// pre-value. Returns how many were written.
    pub(crate) fn put_bootstrap(
        &self,
        addr: Address,
        start: u64,
        proof_block: u64,
        proof_block_hash: B256,
        values: &[(B256, B256)],
    ) -> Result<usize> {
        let mut written = 0;
        let txn = self.db.begin_write()?;
        {
            // The proof was taken against a watch and a block. If either
            // changed while it was in flight (unwatch + watch, a reorg that
            // replaced the block), the values describe nothing we hold.
            let watch = txn.open_table(WATCH)?;
            if watch.get(addr.as_slice())?.map(|v| v.value()) != Some(start) {
                return Ok(0);
            }
            let hashes = txn.open_table(HASHES)?;
            let same_block = hashes
                .get(proof_block)?
                .map(|v| v.value().len() == 64 && v.value()[..32] == proof_block_hash[..])
                .unwrap_or(false);
            if !same_block {
                return Ok(0);
            }
            let mut slots = txn.open_table(SLOTS)?;
            let mut boot = txn.open_table(BOOT)?;
            let mut pending = txn.open_table(PENDING)?;
            for (slot, value) in values {
                let key = slot_prefix(addr, *slot);
                let ok = match boot
                    .get(key.as_slice())?
                    .and_then(|v| decode_boot(v.value()))
                {
                    None => true,
                    Some(BootState::Pending { first_seen })
                    | Some(BootState::Lost { first_seen }) => first_seen > proof_block,
                    Some(BootState::Done) => false,
                };
                if !ok {
                    continue;
                }
                slots.insert(
                    slot_key(addr, *slot, start - 1, u32::MAX).as_slice(),
                    encode_value(Provenance::Proof, *value).as_slice(),
                )?;
                boot.insert(key.as_slice(), encode_boot(BootState::Done).as_slice())?;
                pending.remove(key.as_slice())?;
                written += 1;
            }
        }
        txn.commit()?;
        Ok(written)
    }

    pub(crate) fn mark_lost(&self, addr: Address, slots: &[B256], first_seen: u64) -> Result<()> {
        let txn = self.db.begin_write()?;
        {
            if txn.open_table(WATCH)?.get(addr.as_slice())?.is_none() {
                return Ok(()); // unwatched meanwhile; nothing to mark
            }
            let mut boot = txn.open_table(BOOT)?;
            let mut pending = txn.open_table(PENDING)?;
            for slot in slots {
                let key = slot_prefix(addr, *slot);
                boot.insert(
                    key.as_slice(),
                    encode_boot(BootState::Lost { first_seen }).as_slice(),
                )?;
                pending.remove(key.as_slice())?;
            }
        }
        txn.commit()?;
        Ok(())
    }

    /// All slots whose bootstrap is pending: `(addr, slot, first_seen)`.
    /// Reads the pending index only.
    pub(crate) fn pending_bootstraps(&self) -> Result<Vec<(Address, B256, u64)>> {
        let rtx = self.db.begin_read()?;
        let t = rtx.open_table(PENDING)?;
        let mut out = Vec::new();
        for item in t.iter()? {
            let (k, v) = item?;
            let k = k.value();
            if k.len() != SLOT_PREFIX_LEN {
                return Err(ArchiveError::Corrupt("pending key"));
            }
            out.push((
                Address::from_slice(&k[..20]),
                B256::from_slice(&k[20..]),
                v.value(),
            ));
        }
        Ok(out)
    }

    /// Delete everything above `block` and reset head to it. Walks the block
    /// index per watched address, so the cost is proportional to the records
    /// being removed, not to the size of the archive.
    ///
    /// If the fork is below an address's `start - 1`, every proven pre-value
    /// of that address is dropped too: those proofs were taken on a branch
    /// that may have written the slot before `start`, so they no longer
    /// describe the canonical chain. They are re-proven when needed.
    pub fn rollback_to(&self, block: u64) -> Result<()> {
        let (hash, _) = self
            .header_at(block)?
            .ok_or(ArchiveError::ReorgBeyondHorizon(block))?;
        let watches = self.watchlist()?;
        let txn = self.db.begin_write()?;
        {
            let mut slots = txn.open_table(SLOTS)?;
            let mut idx = txn.open_table(BLOCKIDX)?;
            let mut boot = txn.open_table(BOOT)?;
            let mut pending = txn.open_table(PENDING)?;
            for (addr, start) in &watches {
                if block + 1 < *start {
                    // Fork below start - 1: nothing of this address survives.
                    for k in collect_prefix_keys(&slots, addr.as_slice())? {
                        slots.remove(k.as_slice())?;
                    }
                    for k in collect_prefix_keys(&idx, addr.as_slice())? {
                        idx.remove(k.as_slice())?;
                    }
                    for k in collect_prefix_keys(&boot, addr.as_slice())? {
                        boot.remove(k.as_slice())?;
                    }
                    for k in collect_prefix_keys(&pending, addr.as_slice())? {
                        pending.remove(k.as_slice())?;
                    }
                    continue;
                }
                let lo = blockidx_key(*addr, block + 1);
                let hi = prefix_end(addr.as_slice());
                let mut victims = Vec::new();
                for item in idx.range::<&[u8]>(bounds(&lo, hi.as_deref()))? {
                    let (k, v) = item?;
                    victims.push((k.value().to_vec(), v.value().to_vec()));
                }
                for (k, v) in victims {
                    let (_, b) = parse_blockidx_key(&k).ok_or(ArchiveError::Corrupt("blockidx"))?;
                    let written =
                        decode_slots(&v).ok_or(ArchiveError::Corrupt("blockidx value"))?;
                    for slot in written {
                        let sl = slot_key(*addr, slot, b, 0);
                        let sh = slot_key(*addr, slot, b, u32::MAX);
                        let ks: Vec<Vec<u8>> = slots
                            .range::<&[u8]>(sl.as_slice()..=sh.as_slice())?
                            .map(|r| r.map(|(k, _)| k.value().to_vec()))
                            .collect::<std::result::Result<_, _>>()?;
                        for sk in ks {
                            slots.remove(sk.as_slice())?;
                        }
                        // A slot first seen above the fork was never seen on
                        // the canonical chain: forget its pending/lost state.
                        let bk = slot_prefix(*addr, slot);
                        let forget = match boot
                            .get(bk.as_slice())?
                            .and_then(|v| decode_boot(v.value()))
                        {
                            Some(BootState::Pending { first_seen })
                            | Some(BootState::Lost { first_seen }) => first_seen > block,
                            _ => false,
                        };
                        if forget {
                            boot.remove(bk.as_slice())?;
                            pending.remove(bk.as_slice())?;
                        }
                    }
                    idx.remove(k.as_slice())?;
                }
            }
            let mut hashes = txn.open_table(HASHES)?;
            let above: Vec<u64> = hashes
                .range(block + 1..)?
                .map(|r| r.map(|(k, _)| k.value()))
                .collect::<std::result::Result<_, _>>()?;
            for b in above {
                hashes.remove(b)?;
            }
            // A creation seen above the fork was seen on a dead branch.
            let mut created = txn.open_table(CREATED)?;
            let stale: Vec<Vec<u8>> = created
                .iter()?
                .filter_map(|r| r.ok())
                .filter(|(_, v)| v.value() > block)
                .map(|(k, _)| k.value().to_vec())
                .collect();
            for k in stale {
                created.remove(k.as_slice())?;
            }
            let mut meta = txn.open_table(META)?;
            meta.insert(META_HEAD, head_bytes(block, hash).as_slice())?;
        }
        txn.commit()?;
        Ok(())
    }

    /// Apply one block in a single transaction. `verified` says whether the
    /// BAL matched the header (false only under `allow_unverified`). Returns
    /// the slots that appeared for the first time (candidates for early
    /// bootstrap), grouped by address, and the number of slot records written.
    pub(crate) fn apply_block(
        &self,
        header: &bal_source::Header,
        bal: &bal_codec::BlockAccessList,
        watches: &[(Address, u64)],
        verified: bool,
        prune_hashes_below: Option<u64>,
    ) -> Result<(FreshSlots, usize)> {
        let n = header.number;
        let provenance = if verified {
            Provenance::Bal
        } else {
            Provenance::Unverified
        };
        let mut fresh = Vec::new();
        let mut written = 0usize;
        let txn = self.db.begin_write()?;
        {
            let mut slots = txn.open_table(SLOTS)?;
            let mut idx = txn.open_table(BLOCKIDX)?;
            let mut boot = txn.open_table(BOOT)?;
            let mut pending = txn.open_table(PENDING)?;
            let mut created = txn.open_table(CREATED)?;
            let watch_now = txn.open_table(WATCH)?;
            for (addr, start) in watches {
                if n < *start {
                    continue;
                }
                // Snapshot vs. now: an `unwatch()` since the snapshot must not
                // leave orphan records behind.
                if watch_now.get(addr.as_slice())?.is_none() {
                    continue;
                }
                let Some(acc) = bal.account(addr) else {
                    continue;
                };
                // Creation seen in a verified BAL: from here on no slot of
                // this address needs a proof. Only a verified BAL may say so.
                let mut is_created = created.get(addr.as_slice())?.is_some();
                if !is_created && verified && creation_in(acc) {
                    created.insert(addr.as_slice(), n)?;
                    settle_created(&mut boot, &mut pending, *addr)?;
                    is_created = true;
                }
                let mut fresh_here = Vec::new();
                let mut changed = Vec::with_capacity(acc.storage_changes.len());
                for sc in &acc.storage_changes {
                    let slot = sc.slot_b256();
                    changed.push(slot);
                    let prefix = slot_prefix(*addr, slot);
                    let seen_before = boot.get(prefix.as_slice())?.is_some();
                    if !seen_before && is_created {
                        boot.insert(prefix.as_slice(), encode_boot(BootState::Done).as_slice())?;
                    } else if !seen_before {
                        boot.insert(
                            prefix.as_slice(),
                            encode_boot(BootState::Pending { first_seen: n }).as_slice(),
                        )?;
                        pending.insert(prefix.as_slice(), n)?;
                        fresh_here.push(slot);
                    }
                    if self.config.full_detail {
                        for ch in &sc.changes {
                            slots.insert(
                                slot_key(*addr, slot, n, ch.block_access_index).as_slice(),
                                encode_value(provenance, ch.value_b256()).as_slice(),
                            )?;
                            written += 1;
                        }
                    } else {
                        let ch = sc.final_change();
                        slots.insert(
                            slot_key(*addr, slot, n, ch.block_access_index).as_slice(),
                            encode_value(provenance, ch.value_b256()).as_slice(),
                        )?;
                        written += 1;
                    }
                }
                if !changed.is_empty() {
                    idx.insert(
                        blockidx_key(*addr, n).as_slice(),
                        encode_slots(&changed).as_slice(),
                    )?;
                }
                if !fresh_here.is_empty() {
                    fresh.push((*addr, *start, fresh_here));
                }
            }
            let mut hashes = txn.open_table(HASHES)?;
            hashes.insert(n, header_bytes(header.hash, header.state_root).as_slice())?;
            if let Some(below) = prune_hashes_below {
                let old: Vec<u64> = hashes
                    .range(..below)?
                    .map(|r| r.map(|(k, _)| k.value()))
                    .collect::<std::result::Result<_, _>>()?;
                for b in old {
                    hashes.remove(b)?;
                }
            }
            let mut meta = txn.open_table(META)?;
            meta.insert(META_HEAD, head_bytes(n, header.hash).as_slice())?;
        }
        txn.commit()?;
        Ok((fresh, written))
    }
}
