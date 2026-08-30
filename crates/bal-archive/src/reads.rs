//! Reads: one ordered seek per value, and a typed reason for every miss.

use super::*;

impl Archive {
    fn check_range(&self, addr: Address, block: u64) -> std::result::Result<u64, NotAvailable> {
        let start = self.start_of(addr)?.ok_or(NotAvailable::NotWatched(addr))?;
        if block < start {
            return Err(NotAvailable::BeforeStart {
                requested: block,
                start,
            });
        }
        let (head, _) = self.head()?.ok_or(NotAvailable::NotSynced)?;
        if block > head {
            return Err(NotAvailable::AfterHead {
                requested: block,
                head,
            });
        }
        Ok(start)
    }

    /// Value of `slot` at the end of `block`. One ordered seek.
    pub fn storage_at(
        &self,
        addr: Address,
        slot: B256,
        block: u64,
    ) -> std::result::Result<StorageValue, NotAvailable> {
        self.check_range(addr, block)?;
        let rtx = self.db.begin_read().map_err(ArchiveError::from)?;
        let t = rtx.open_table(SLOTS).map_err(ArchiveError::from)?;
        let lo = slot_prefix(addr, slot);
        let hi = slot_key(addr, slot, block, u32::MAX);
        let mut it = t
            .range::<&[u8]>(lo.as_slice()..=hi.as_slice())
            .map_err(ArchiveError::from)?;
        if let Some(item) = it.next_back() {
            let (k, v) = item.map_err(ArchiveError::from)?;
            let (_, _, set_at, index) =
                parse_slot_key(k.value()).ok_or(NotAvailable::Internal("bad slot key".into()))?;
            let (provenance, value) =
                decode_value(v.value()).ok_or(NotAvailable::Internal("bad slot value".into()))?;
            return Ok(StorageValue {
                value,
                provenance,
                set_at,
                index,
            });
        }
        drop(it);
        // No record at or before `block`. If the contract's creation was
        // seen, the slot was never written between creation and `block` (BAL
        // completeness), and before creation the account had no storage: the
        // value is zero, and that is a fact from a verified BAL, not a guess.
        let created = rtx.open_table(CREATED).map_err(ArchiveError::from)?;
        if let Some(c) = created
            .get(addr.as_slice())
            .map_err(ArchiveError::from)?
            .map(|v| v.value())
        {
            return Ok(StorageValue {
                value: B256::ZERO,
                provenance: Provenance::Bal,
                set_at: c,
                index: u32::MAX,
            });
        }
        // Otherwise the slot's earliest change is later and nothing is known
        // before it, or it never changed at all.
        let boot = rtx.open_table(BOOT).map_err(ArchiveError::from)?;
        match boot.get(lo.as_slice()).map_err(ArchiveError::from)? {
            Some(v) => match decode_boot(v.value()) {
                Some(BootState::Pending { first_seen }) | Some(BootState::Lost { first_seen }) => {
                    Err(NotAvailable::UnknownBefore { first_seen })
                }
                Some(BootState::Done) => Err(NotAvailable::Internal(
                    "bootstrap marked done but no record".into(),
                )),
                None => Err(NotAvailable::Internal("bad bootstrap record".into())),
            },
            None => Err(NotAvailable::NeverRecorded),
        }
    }

    /// All recorded changes of `slot` in `range` (half-open), ascending.
    /// Bootstrap records live at `start - 1` and are therefore never inside a
    /// valid range; what comes back is BAL data only.
    pub fn history(
        &self,
        addr: Address,
        slot: B256,
        range: Range<u64>,
    ) -> std::result::Result<Vec<HistoryEntry>, NotAvailable> {
        if range.start >= range.end {
            return Err(NotAvailable::InvalidRange {
                start: range.start,
                end: range.end,
            });
        }
        self.check_range(addr, range.start)?;
        self.check_range(addr, range.end - 1)?;
        let rtx = self.db.begin_read().map_err(ArchiveError::from)?;
        let t = rtx.open_table(SLOTS).map_err(ArchiveError::from)?;
        let lo = slot_key(addr, slot, range.start, 0);
        let hi = slot_key(addr, slot, range.end, 0);
        let mut out = Vec::new();
        for item in t
            .range::<&[u8]>(lo.as_slice()..hi.as_slice())
            .map_err(ArchiveError::from)?
        {
            let (k, v) = item.map_err(ArchiveError::from)?;
            let (_, _, block, index) =
                parse_slot_key(k.value()).ok_or(NotAvailable::Internal("bad slot key".into()))?;
            let (provenance, value) =
                decode_value(v.value()).ok_or(NotAvailable::Internal("bad slot value".into()))?;
            out.push(HistoryEntry {
                block,
                index,
                value,
                provenance,
            });
        }
        Ok(out)
    }

    /// Slots of `addr` written in `block`, from the block index.
    pub fn changed_slots(
        &self,
        addr: Address,
        block: u64,
    ) -> std::result::Result<Vec<B256>, NotAvailable> {
        self.check_range(addr, block)?;
        let rtx = self.db.begin_read().map_err(ArchiveError::from)?;
        let t = rtx.open_table(BLOCKIDX).map_err(ArchiveError::from)?;
        match t
            .get(blockidx_key(addr, block).as_slice())
            .map_err(ArchiveError::from)?
        {
            Some(v) => {
                decode_slots(v.value()).ok_or(NotAvailable::Internal("bad blockidx value".into()))
            }
            None => Ok(Vec::new()),
        }
    }

    /// Bootstrap state of a slot, if it has ever been seen or proven.
    pub fn boot_state(&self, addr: Address, slot: B256) -> Result<Option<BootState>> {
        let rtx = self.db.begin_read()?;
        let t = rtx.open_table(BOOT)?;
        Ok(t.get(slot_prefix(addr, slot).as_slice())?
            .and_then(|v| decode_boot(v.value())))
    }
}
