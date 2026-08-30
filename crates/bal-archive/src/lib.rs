#![doc = include_str!("../README.md")]
//!
//! `bal-archive`: accumulate storage changes from verified BALs, serve
//! versioned reads, handle reorgs and bootstrap. Knows blocks and slots;
//! knows nothing about Solidity.
//!
//! Every value in the store carries its [`Provenance`]. Every miss is a typed
//! [`NotAvailable`]. There is no code path that returns a zero for "unknown".
//!
//! All methods take `&self`: redb serialises writers itself, so an
//! [`Archive`] can be shared (e.g. in an `Arc`) and read while [`Archive::sync`]
//! is running. [`Archive::watch`] and the sync loop coordinate through a
//! small gate so that a watch added mid-sync is never silently skipped.

mod keys;
mod sync;

pub use keys::{BootState, Provenance, SCHEMA_VERSION};
pub use sync::{SyncReport, REORG_HORIZON_FALLBACK};

use alloy_primitives::{Address, B256};
use bal_codec::BlockAccessIndex;
use keys::*;
use redb::{Database, ReadableTable, ReadableTableMetadata, TableDefinition};
use std::ops::{Bound, Range, RangeBounds};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

const SLOTS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("slots");
const BLOCKIDX: TableDefinition<&[u8], ()> = TableDefinition::new("blockidx");
const META: TableDefinition<&str, &[u8]> = TableDefinition::new("meta");
const WATCH: TableDefinition<&[u8], u64> = TableDefinition::new("watch");
/// block -> hash(32) || state_root(32)
const HASHES: TableDefinition<u64, &[u8]> = TableDefinition::new("blockhashes");
const BOOT: TableDefinition<&[u8], &[u8]> = TableDefinition::new("bootstrap");
/// addr || slot -> first_seen, for slots whose bootstrap is pending. Lets
/// the retry path scan only what is pending instead of every slot.
const PENDING: TableDefinition<&[u8], u64> = TableDefinition::new("pending");

const META_SCHEMA: &str = "schema_version";
const META_HEAD: &str = "head";
const META_FULL_DETAIL: &str = "full_detail";

/// Failures of the archive itself (storage, source, verification). Reads
/// use [`NotAvailable`] instead: a missing value is an answer, not a failure.
#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    /// Embedded database error.
    #[error("db: {0}")]
    Db(Box<redb::Error>),
    /// The file was written by a build with a different key layout.
    #[error("schema version {found} on disk, this build speaks {expected}")]
    SchemaMismatch {
        /// Version found in the file.
        found: u32,
        /// Version this build writes.
        expected: u32,
    },
    /// `from_block` must be at least 1 (the pre-value record lives at `from_block - 1`).
    #[error("from_block must be >= 1 (got {0})")]
    InvalidStart(u64),
    /// Watching from a block the archive has already passed (or is applying
    /// right now) is backfill, not `watch`.
    #[error("watch from block {from_block} is in the past (head {head}); backfill is not part of watch()")]
    StartInPast {
        /// Requested start.
        from_block: u64,
        /// Current archive head (or the block being applied).
        head: u64,
    },
    /// The address is not on the watchlist.
    #[error("address {0} is not watched")]
    NotWatched(Address),
    /// The address is already watched with a different start.
    #[error(
        "address {address} is already watched from block {from_block}; unwatch first to change it"
    )]
    AlreadyWatched {
        /// The address.
        address: Address,
        /// Its existing start block.
        from_block: u64,
    },
    /// Another `sync` pass is running on this archive.
    #[error("a sync pass is already running on this archive")]
    SyncInProgress,
    /// The file was created with a different value of a creation-time option.
    #[error("archive was created with {option} = {on_disk}, opened with {requested}")]
    ConfigMismatch {
        /// Option name.
        option: &'static str,
        /// Value stored in the file.
        on_disk: String,
        /// Value requested now.
        requested: String,
    },
    /// A bootstrap was requested before the archive reached the watch start.
    #[error("archive head {head} is below watch start {start}; nothing to bootstrap yet")]
    HeadBelowStart {
        /// Current head.
        head: u64,
        /// Watch start of the address.
        start: u64,
    },
    /// The BAL / state source failed.
    #[error("source: {0}")]
    Source(#[from] bal_source::SourceError),
    /// `keccak(rlp(bal))` did not match the header. Sync stops here.
    #[error("block {block} failed BAL verification: {err}")]
    Verification {
        /// Offending block.
        block: u64,
        /// What the codec found.
        err: bal_codec::CodecError,
    },
    /// The header has no BAL hash and `allow_unverified` is off.
    #[error(
        "block {0} header carries no block_access_list_hash; refusing to apply unverifiable data"
    )]
    NoBalHash(u64),
    /// A reorg reached below the retained block hashes; the archive cannot
    /// find the fork point and must not guess.
    #[error("reorg deeper than retained block hashes (fork below block {0})")]
    ReorgBeyondHorizon(u64),
    /// The source keeps serving a block whose parent is not the block it
    /// serves for `number - 1` (pooled upstreams on different forks).
    #[error(
        "source is inconsistent around block {0}: parent hash does not match its own block {0}-1"
    )]
    InconsistentSource(u64),
    /// A Merkle proof did not verify against the header's `state_root`.
    #[error("proof: {0}")]
    Proof(#[from] bal_source::ProofError),
    /// A stored record has an unexpected shape.
    #[error("corrupt record: {0}")]
    Corrupt(&'static str),
}

macro_rules! from_redb {
    ($($t:ty),*) => {$(
        impl From<$t> for ArchiveError {
            fn from(e: $t) -> Self { ArchiveError::Db(Box::new(e.into())) }
        }
    )*};
}
from_redb!(
    redb::Error,
    redb::DatabaseError,
    redb::TransactionError,
    redb::TableError,
    redb::StorageError,
    redb::CommitError
);

/// Result of archive operations.
pub type Result<T> = std::result::Result<T, ArchiveError>;

/// Slots seen for the first time in a block: `(addr, watch_start, slots)`.
pub(crate) type FreshSlots = Vec<(Address, u64, Vec<B256>)>;

/// Why a read has no answer. Promise #3 lives here: a caller always learns
/// *which* boundary it hit, and never receives a zero in place of "unknown".
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NotAvailable {
    /// The address is not on the watchlist.
    #[error("address {0} is not watched")]
    NotWatched(Address),
    /// The block precedes the address's watch start.
    #[error("block {requested} is before watch start {start}")]
    BeforeStart {
        /// Requested block.
        requested: u64,
        /// Watch start for this address.
        start: u64,
    },
    /// The block is beyond what the archive has applied.
    #[error("block {requested} is after archive head {head}")]
    AfterHead {
        /// Requested block.
        requested: u64,
        /// Current archive head.
        head: u64,
    },
    /// No block has been applied yet.
    #[error("archive has not synced any block yet")]
    NotSynced,
    /// `start >= end`: a caller error, reported rather than answered with nothing.
    #[error("invalid block range {start}..{end}")]
    InvalidRange {
        /// Range start.
        start: u64,
        /// Range end (exclusive).
        end: u64,
    },
    /// Slot has never changed since `start` and its initial value was never
    /// proven. Call [`Archive::bootstrap_slot`] to obtain it at the head.
    #[error("slot never changed since watch start and has not been bootstrapped yet")]
    NotBootstrapped,
    /// The slot's first change was recorded; its earlier value awaits a proof.
    #[error(
        "slot first changed at block {first_seen}; its earlier value is still pending a proof"
    )]
    BootstrapPending {
        /// Block of the first recorded change.
        first_seen: u64,
    },
    /// The node's state window passed before a proof was obtained; the
    /// value before `first_seen` is unobtainable.
    #[error("slot first changed at block {first_seen}; its earlier value was lost (node state window passed before a proof was obtained)")]
    BootstrapLost {
        /// Block of the first recorded change.
        first_seen: u64,
    },
    /// Storage failure surfaced through a read.
    #[error("internal: {0}")]
    Internal(String),
}

impl From<ArchiveError> for NotAvailable {
    fn from(e: ArchiveError) -> Self {
        NotAvailable::Internal(e.to_string())
    }
}

/// A stored word with where it came from and when it was set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StorageValue {
    /// The 32-byte word.
    pub value: B256,
    /// BAL, proof, import, or unverified.
    pub provenance: Provenance,
    /// Block at which this value was set; `watch start - 1` for a proven
    /// pre-value.
    pub set_at: u64,
    /// Position within `set_at` (`u32::MAX` for proven pre-values).
    pub index: BlockAccessIndex,
}

/// One recorded change, as returned by [`Archive::history`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryEntry {
    /// Block of the change.
    pub block: u64,
    /// Position within the block.
    pub index: BlockAccessIndex,
    /// Post-value.
    pub value: B256,
    /// [`Provenance::Bal`], or [`Provenance::Unverified`] under `allow_unverified`.
    pub provenance: Provenance,
}

/// Tunables fixed at [`Archive::open_with`].
#[derive(Debug, Clone)]
pub struct ArchiveConfig {
    /// How many blocks back the node can still serve `eth_getProof`.
    /// Pending bootstraps older than this are marked lost.
    pub bootstrap_window: u64,
    /// Store every intra-block change rather than only the last one.
    /// Decided at creation; changing it later means a new archive.
    pub full_detail: bool,
    /// Apply blocks whose header has no BAL hash. Debug only; such values
    /// are stored with [`Provenance::Unverified`].
    pub allow_unverified: bool,
}

impl Default for ArchiveConfig {
    fn default() -> Self {
        Self {
            bootstrap_window: 120,
            full_detail: false,
            allow_unverified: false,
        }
    }
}

/// The store. One file, one process, any number of readers alongside the
/// syncing writer.
pub struct Archive {
    db: Database,
    config: ArchiveConfig,
    /// Serialises `watch()`/`unwatch()` against the sync loop's per-block
    /// watchlist read.
    watch_gate: Mutex<()>,
    /// Block the sync loop is about to apply (0 when idle). `watch()` refuses
    /// starts at or below it, so a watch is never added for a block whose
    /// watchlist snapshot has already been taken.
    in_flight: AtomicU64,
    /// Set while a `sync` pass runs; a second concurrent pass is refused.
    syncing: AtomicBool,
}

impl Archive {
    /// Open or create `path` with default configuration.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with(path, ArchiveConfig::default())
    }

    /// Open or create `path`. Refuses files written with another
    /// [`SCHEMA_VERSION`].
    pub fn open_with(path: impl AsRef<Path>, config: ArchiveConfig) -> Result<Self> {
        let db = Database::create(path)?;
        let txn = db.begin_write()?;
        {
            txn.open_table(SLOTS)?;
            txn.open_table(BLOCKIDX)?;
            txn.open_table(WATCH)?;
            txn.open_table(HASHES)?;
            let boot = txn.open_table(BOOT)?;
            let mut pending = txn.open_table(PENDING)?;
            let mut meta = txn.open_table(META)?;
            let found: Option<Vec<u8>> = meta.get(META_SCHEMA)?.map(|v| v.value().to_vec());
            match found {
                Some(v) => {
                    let found = u32::from_be_bytes(
                        v.as_slice()
                            .try_into()
                            .map_err(|_| ArchiveError::Corrupt("schema_version"))?,
                    );
                    if found != SCHEMA_VERSION {
                        return Err(ArchiveError::SchemaMismatch {
                            found,
                            expected: SCHEMA_VERSION,
                        });
                    }
                }
                None => {
                    meta.insert(META_SCHEMA, SCHEMA_VERSION.to_be_bytes().as_slice())?;
                }
            }
            // `full_detail` decides the key set on disk; it cannot change later.
            let stored_detail = meta.get(META_FULL_DETAIL)?.map(|v| v.value().to_vec());
            match stored_detail {
                Some(v) => {
                    let on_disk = v.first().copied().unwrap_or(0) != 0;
                    if on_disk != config.full_detail {
                        return Err(ArchiveError::ConfigMismatch {
                            option: "full_detail",
                            on_disk: on_disk.to_string(),
                            requested: config.full_detail.to_string(),
                        });
                    }
                }
                None => {
                    meta.insert(META_FULL_DETAIL, [config.full_detail as u8].as_slice())?;
                }
            }
            // Files written before the pending index existed: rebuild it once.
            if pending.is_empty()? {
                let mut rebuilt = Vec::new();
                for item in boot.iter()? {
                    let (k, v) = item?;
                    if let Some(BootState::Pending { first_seen }) = decode_boot(v.value()) {
                        rebuilt.push((k.value().to_vec(), first_seen));
                    }
                }
                for (k, f) in rebuilt {
                    pending.insert(k.as_slice(), f)?;
                }
            }
        }
        txn.commit()?;
        Ok(Self {
            db,
            config,
            watch_gate: Mutex::new(()),
            in_flight: AtomicU64::new(0),
            syncing: AtomicBool::new(false),
        })
    }

    /// Claim the sync slot; `false` if another pass is already running.
    pub(crate) fn begin_sync(&self) -> bool {
        self.syncing
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    /// Configuration this archive was opened with.
    pub fn config(&self) -> &ArchiveConfig {
        &self.config
    }

    // ---- watchlist ------------------------------------------------------

    /// Start accumulating `addr` from `from_block` (inclusive). `from_block`
    /// must be above the current head and above any block the sync loop is
    /// currently applying: history before now is backfill, a separate,
    /// explicitly opt-in mechanism. An address can be watched once; change
    /// its start with [`Archive::unwatch`] first (which drops its data).
    pub fn watch(&self, addr: Address, from_block: u64) -> Result<()> {
        if from_block == 0 {
            return Err(ArchiveError::InvalidStart(from_block));
        }
        let _gate = self.watch_gate.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(existing) = self.start_of(addr)? {
            if existing == from_block {
                return Ok(());
            }
            return Err(ArchiveError::AlreadyWatched {
                address: addr,
                from_block: existing,
            });
        }
        let head = self.head()?.map(|(h, _)| h).unwrap_or(0);
        let floor = head.max(self.in_flight.load(Ordering::SeqCst));
        if from_block <= floor {
            return Err(ArchiveError::StartInPast {
                from_block,
                head: floor,
            });
        }
        let txn = self.db.begin_write()?;
        txn.open_table(WATCH)?.insert(addr.as_slice(), from_block)?;
        txn.commit()?;
        Ok(())
    }

    /// Stop watching and delete everything stored for `addr`. Taken under
    /// the watch gate; a block being applied concurrently re-checks the
    /// watchlist inside its transaction, so nothing is written for `addr`
    /// after this returns.
    pub fn unwatch(&self, addr: Address) -> Result<()> {
        let _gate = self.watch_gate.lock().unwrap_or_else(|p| p.into_inner());
        let txn = self.db.begin_write()?;
        {
            txn.open_table(WATCH)?.remove(addr.as_slice())?;
            let mut slots = txn.open_table(SLOTS)?;
            for k in collect_prefix_keys(&slots, addr.as_slice())? {
                slots.remove(k.as_slice())?;
            }
            let mut boot = txn.open_table(BOOT)?;
            for k in collect_prefix_keys(&boot, addr.as_slice())? {
                boot.remove(k.as_slice())?;
            }
            let mut pending = txn.open_table(PENDING)?;
            for k in collect_prefix_keys(&pending, addr.as_slice())? {
                pending.remove(k.as_slice())?;
            }
            let mut idx = txn.open_table(BLOCKIDX)?;
            for k in collect_prefix_keys(&idx, addr.as_slice())? {
                idx.remove(k.as_slice())?;
            }
        }
        txn.commit()?;
        Ok(())
    }

    /// Watched addresses with their start blocks.
    pub fn watchlist(&self) -> Result<Vec<(Address, u64)>> {
        let rtx = self.db.begin_read()?;
        let t = rtx.open_table(WATCH)?;
        let mut out = Vec::new();
        for item in t.iter()? {
            let (k, v) = item?;
            out.push((Address::from_slice(k.value()), v.value()));
        }
        Ok(out)
    }

    /// Watchlist snapshot for applying `block`, taken under the watch gate
    /// so that no `watch()` can slip in between the snapshot and the apply.
    pub(crate) fn watchlist_for(&self, block: u64) -> Result<Vec<(Address, u64)>> {
        let _gate = self.watch_gate.lock().unwrap_or_else(|p| p.into_inner());
        self.in_flight.store(block, Ordering::SeqCst);
        self.watchlist()
    }

    /// Decide where a sync pass starts, under the watch gate, and publish it
    /// as the in-flight block *before* the pass awaits anything. Without
    /// this, a `watch()` with a start below the pass's first block could be
    /// accepted while the pass is fetching, and its early blocks skipped.
    /// Returns `None` if nothing is watched.
    pub(crate) fn claim_start(&self) -> Result<Option<u64>> {
        let _gate = self.watch_gate.lock().unwrap_or_else(|p| p.into_inner());
        let earliest = self.watchlist()?.iter().map(|(_, s)| *s).min();
        let Some(earliest) = earliest else {
            return Ok(None);
        };
        let next = match self.head()? {
            Some((h, _)) => h + 1,
            None => earliest,
        };
        self.in_flight.store(next, Ordering::SeqCst);
        Ok(Some(next))
    }

    /// Release the sync slot and the in-flight marker. Always called when a
    /// pass ends, successfully or not.
    pub(crate) fn sync_idle(&self) {
        self.in_flight.store(0, Ordering::SeqCst);
        self.syncing.store(false, Ordering::SeqCst);
    }

    pub(crate) fn start_of(&self, addr: Address) -> Result<Option<u64>> {
        let rtx = self.db.begin_read()?;
        let t = rtx.open_table(WATCH)?;
        Ok(t.get(addr.as_slice())?.map(|v| v.value()))
    }

    // ---- head -----------------------------------------------------------

    /// Last applied block and its hash, if any block was applied.
    pub fn head(&self) -> Result<Option<(u64, B256)>> {
        let rtx = self.db.begin_read()?;
        let t = rtx.open_table(META)?;
        match t.get(META_HEAD)? {
            None => Ok(None),
            Some(v) => {
                let b = v.value();
                if b.len() != 40 {
                    return Err(ArchiveError::Corrupt("head"));
                }
                let num: [u8; 8] = b[..8]
                    .try_into()
                    .map_err(|_| ArchiveError::Corrupt("head"))?;
                Ok(Some((u64::from_be_bytes(num), B256::from_slice(&b[8..]))))
            }
        }
    }

    /// Stored `(hash, state_root)` of `block`, if retained.
    pub(crate) fn header_at(&self, block: u64) -> Result<Option<(B256, B256)>> {
        let rtx = self.db.begin_read()?;
        let t = rtx.open_table(HASHES)?;
        match t.get(block)? {
            None => Ok(None),
            Some(v) => {
                let b = v.value();
                if b.len() != 64 {
                    return Err(ArchiveError::Corrupt("blockhashes"));
                }
                Ok(Some((
                    B256::from_slice(&b[..32]),
                    B256::from_slice(&b[32..]),
                )))
            }
        }
    }

    /// Remember a header fetched outside `apply_block` (e.g. the parent of
    /// the first watched block) so bootstrap retries can find its root.
    pub(crate) fn remember_header(&self, block: u64, hash: B256, state_root: B256) -> Result<()> {
        let txn = self.db.begin_write()?;
        {
            let mut hashes = txn.open_table(HASHES)?;
            if hashes.get(block)?.is_none() {
                hashes.insert(block, header_bytes(hash, state_root).as_slice())?;
            }
        }
        txn.commit()?;
        Ok(())
    }

    // ---- reads ----------------------------------------------------------

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
        // No record at or before `block`. Either the slot's first change is
        // later and its pre-value is not (yet) proven, or it never changed.
        let boot = rtx.open_table(BOOT).map_err(ArchiveError::from)?;
        match boot.get(lo.as_slice()).map_err(ArchiveError::from)? {
            Some(v) => match decode_boot(v.value()) {
                Some(BootState::Pending { first_seen }) => {
                    Err(NotAvailable::BootstrapPending { first_seen })
                }
                Some(BootState::Lost { first_seen }) => {
                    Err(NotAvailable::BootstrapLost { first_seen })
                }
                Some(BootState::Done) => Err(NotAvailable::Internal(
                    "bootstrap marked done but no record".into(),
                )),
                None => Err(NotAvailable::Internal("bad bootstrap record".into())),
            },
            None => Err(NotAvailable::NotBootstrapped),
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
        let prefix = blockidx_prefix(addr, block);
        let mut out = Vec::new();
        for k in collect_prefix_keys(&t, &prefix)? {
            let (_, _, slot) =
                parse_blockidx_key(&k).ok_or(NotAvailable::Internal("bad blockidx key".into()))?;
            out.push(slot);
        }
        Ok(out)
    }

    /// Bootstrap state of a slot, if it has ever been seen or proven.
    pub fn boot_state(&self, addr: Address, slot: B256) -> Result<Option<BootState>> {
        let rtx = self.db.begin_read()?;
        let t = rtx.open_table(BOOT)?;
        Ok(t.get(slot_prefix(addr, slot).as_slice())?
            .and_then(|v| decode_boot(v.value())))
    }

    // ---- writes (used by sync) ------------------------------------------

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
                let lo = blockidx_prefix(*addr, block + 1);
                let hi = prefix_end(addr.as_slice());
                let mut victims = Vec::new();
                for item in idx.range::<&[u8]>(bounds(&lo, hi.as_deref()))? {
                    let (k, _) = item?;
                    victims.push(k.value().to_vec());
                }
                for k in victims {
                    let (_, b, slot) =
                        parse_blockidx_key(&k).ok_or(ArchiveError::Corrupt("blockidx"))?;
                    let sl = slot_key(*addr, slot, b, 0);
                    let sh = slot_key(*addr, slot, b, u32::MAX);
                    let ks: Vec<Vec<u8>> = slots
                        .range::<&[u8]>(sl.as_slice()..=sh.as_slice())?
                        .map(|r| r.map(|(k, _)| k.value().to_vec()))
                        .collect::<std::result::Result<_, _>>()?;
                    for sk in ks {
                        slots.remove(sk.as_slice())?;
                    }
                    idx.remove(k.as_slice())?;
                    // A slot first seen above the fork was never seen on the
                    // canonical chain: forget its pending/lost state.
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
            }
            let mut hashes = txn.open_table(HASHES)?;
            let above: Vec<u64> = hashes
                .range(block + 1..)?
                .map(|r| r.map(|(k, _)| k.value()))
                .collect::<std::result::Result<_, _>>()?;
            for b in above {
                hashes.remove(b)?;
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
                let mut fresh_here = Vec::new();
                for sc in &acc.storage_changes {
                    let slot = sc.slot_b256();
                    let prefix = slot_prefix(*addr, slot);
                    let seen_before = boot.get(prefix.as_slice())?.is_some();
                    if !seen_before {
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
                    idx.insert(blockidx_key(*addr, n, slot).as_slice(), ())?;
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

fn head_bytes(block: u64, hash: B256) -> [u8; 40] {
    let mut b = [0u8; 40];
    b[..8].copy_from_slice(&block.to_be_bytes());
    b[8..].copy_from_slice(hash.as_slice());
    b
}

fn header_bytes(hash: B256, state_root: B256) -> [u8; 64] {
    let mut b = [0u8; 64];
    b[..32].copy_from_slice(hash.as_slice());
    b[32..].copy_from_slice(state_root.as_slice());
    b
}

/// `lo..hi`, or `lo..` when the prefix is all `0xFF` and has no successor.
fn bounds<'a>(lo: &'a [u8], hi: Option<&'a [u8]>) -> impl RangeBounds<&'a [u8]> {
    (
        Bound::Included(lo),
        match hi {
            Some(h) => Bound::Excluded(h),
            None => Bound::Unbounded,
        },
    )
}

/// Every key starting with `prefix`, in order.
fn collect_prefix_keys<V: redb::Value + 'static>(
    t: &impl ReadableTable<&'static [u8], V>,
    prefix: &[u8],
) -> Result<Vec<Vec<u8>>> {
    let hi = prefix_end(prefix);
    let mut out = Vec::new();
    for item in t.range::<&[u8]>(bounds(prefix, hi.as_deref()))? {
        let (k, _) = item?;
        out.push(k.value().to_vec());
    }
    Ok(out)
}
