#![doc = include_str!("../README.md")]
//!
//! `bal-archive`: accumulate storage changes from verified BALs, serve
//! versioned reads, backfill older blocks, handle reorgs. Knows blocks and slots;
//! knows nothing about Solidity.
//!
//! Every value in the store carries its [`Provenance`]. Every miss is a typed
//! [`NotAvailable`]. There is no code path that returns a zero for "unknown".
//!
//! All methods take `&self`: redb serialises writers itself, so an
//! [`Archive`] can be shared (e.g. in an `Arc`) and read while [`Archive::sync`]
//! is running. [`Archive::watch`] and the sync loop coordinate through a
//! small gate so that a watch added mid-sync is never silently skipped.

mod backfill;
mod keys;
mod reads;
mod sync;
mod writes;

pub use backfill::{BackfillOpts, BackfillReport, BackfillStop};
pub use keys::{BootState, Provenance, OLDEST_UPGRADABLE, SCHEMA_VERSION};
pub use sync::{SyncReport, REORG_HORIZON_FALLBACK, TOUCHED_CAP};

use alloy_primitives::{Address, B256};
use bal_codec::BlockAccessIndex;
use keys::*;
use redb::{Database, ReadableTable, ReadableTableMetadata, TableDefinition};
use std::ops::{Bound, Range, RangeBounds};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

pub(crate) const SLOTS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("slots");
/// addr || block -> the slots written in that block (v3). One entry per
/// address and block; `diff`, rendering and rollback read it.
pub(crate) const BLOCKIDX: TableDefinition<&[u8], &[u8]> = TableDefinition::new("blockslots");
/// v1/v2 block index (one key per slot), migrated on open and dropped.
const LEGACY_BLOCKIDX: TableDefinition<&[u8], ()> = TableDefinition::new("blockidx");
pub(crate) const META: TableDefinition<&str, &[u8]> = TableDefinition::new("meta");
pub(crate) const WATCH: TableDefinition<&[u8], u64> = TableDefinition::new("watch");
/// block -> hash(32) || state_root(32)
const HASHES: TableDefinition<u64, &[u8]> = TableDefinition::new("blockhashes");
pub(crate) const BOOT: TableDefinition<&[u8], &[u8]> = TableDefinition::new("bootstrap");
/// addr || slot -> first_seen, for slots whose bootstrap is pending. Lets
/// the retry path scan only what is pending instead of every slot.
pub(crate) const PENDING: TableDefinition<&[u8], u64> = TableDefinition::new("pending");
/// addr -> block in which the contract was created, when a verified BAL
/// showed the creation. Before that block the account had no storage, so
/// every slot's pre-value is zero by protocol rule (EIP-7610) — no proof
/// needed, ever, for such an address.
pub(crate) const CREATED: TableDefinition<&[u8], u64> = TableDefinition::new("created");

const META_SCHEMA: &str = "schema_version";
/// `anchor:<addr>` -> hash of the address's current start block, written by
/// backfill so the next backward step can check the parent link.
const META_ANCHOR: &str = "anchor:";
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
    #[error("watch from block {from_block} is in the past (head {head}); use backfill for history before the head")]
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
    /// Backfill found that the node's block at the watch start is not the
    /// block the archive holds: the start was reorged. A forward sync
    /// resolves that; backfill will not guess which branch to extend.
    #[error("block {0} on the node is not the block the archive holds; run sync first")]
    StartReplaced(u64),
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
    redb::CommitError,
    redb::CompactionError
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
    /// No change to the slot has been recorded since `start`, and the address
    /// was not seen being created: its value is not known. Backfill to the
    /// contract's creation ([`Archive::backfill`]) or prove it at the head
    /// ([`Archive::bootstrap_slot`]).
    #[error("no change to this slot is recorded since the watch start; backfill to the contract's creation, or prove it at the head")]
    NeverRecorded,
    /// The slot's earliest recorded change is at `first_seen`; nothing is
    /// known before it yet. Backfill further back (or prove it while the
    /// node's state window still allows).
    #[error("no record before block {first_seen} (the slot's earliest recorded change); backfill further back")]
    UnknownBefore {
        /// Block of the earliest recorded change.
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

/// What [`Archive::stats`] reports.
#[derive(Debug, Clone)]
pub struct ArchiveStats {
    /// Last applied block and hash.
    pub head: Option<(u64, B256)>,
    /// Watched addresses with start blocks.
    pub watches: Vec<(Address, u64)>,
    /// Addresses whose creation was seen, with the creation block. Their
    /// history is complete: no pre-value is ever unknown.
    pub created: Vec<(Address, u64)>,
    /// Slot records in the primary index.
    pub slot_records: u64,
    /// Slots whose pre-value is proven.
    pub slots_done: u64,
    /// Slots whose pre-value is still awaited.
    pub slots_pending: u64,
    /// Slots whose pre-value was lost.
    pub slots_lost: u64,
    /// Block hashes kept for reorg detection.
    pub retained_headers: u64,
    /// Size of the archive file on disk.
    pub file_bytes: u64,
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
    path: std::path::PathBuf,
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
        let path_buf = path.as_ref().to_path_buf();
        let db = Database::create(&path_buf)?;
        let txn = db.begin_write()?;
        {
            txn.open_table(SLOTS)?;
            txn.open_table(BLOCKIDX)?;
            txn.open_table(WATCH)?;
            txn.open_table(HASHES)?;
            txn.open_table(CREATED)?;
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
                    if (OLDEST_UPGRADABLE..SCHEMA_VERSION).contains(&found) {
                        if found < 3 {
                            migrate_block_index(&txn)?;
                        }
                        // Stamp the new version so an older build refuses
                        // the file cleanly from now on.
                        meta.insert(META_SCHEMA, SCHEMA_VERSION.to_be_bytes().as_slice())?;
                    } else if found != SCHEMA_VERSION {
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
            path: path_buf,
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
    /// currently applying: history before now is [`Archive::backfill`], an
    /// explicit call. An address can be watched once; change
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
            txn.open_table(CREATED)?.remove(addr.as_slice())?;
            txn.open_table(META)?.remove(anchor_key(addr).as_str())?;
        }
        txn.commit()?;
        Ok(())
    }

    /// Rewrite the file without free pages. redb frees pages as records are
    /// overwritten and transactions retire, but never shrinks the file on
    /// its own; after a long backfill the difference is large. Needs the
    /// file to itself: call with no [`Archive`] open on it. Returns whether
    /// anything changed.
    pub fn compact_file(path: impl AsRef<Path>) -> Result<bool> {
        let mut db = Database::open(path.as_ref())?;
        Ok(db.compact()?)
    }

    /// Block in which `addr` was created, if a verified BAL showed it.
    pub fn created_at(&self, addr: Address) -> Result<Option<u64>> {
        let rtx = self.db.begin_read()?;
        let t = rtx.open_table(CREATED)?;
        Ok(t.get(addr.as_slice())?.map(|v| v.value()))
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

    /// Counts and sizes for `status`-style reporting. Scans the bootstrap
    /// table, so it is proportional to the number of distinct slots seen —
    /// fine for a command, not for a hot path.
    pub fn stats(&self) -> Result<ArchiveStats> {
        let rtx = self.db.begin_read()?;
        let slot_records = rtx.open_table(SLOTS)?.len()?;
        let pending = rtx.open_table(PENDING)?.len()?;
        let boot = rtx.open_table(BOOT)?;
        let (mut done, mut lost) = (0u64, 0u64);
        for item in boot.iter()? {
            let (_, v) = item?;
            match decode_boot(v.value()) {
                Some(BootState::Done) => done += 1,
                Some(BootState::Lost { .. }) => lost += 1,
                _ => {}
            }
        }
        let retained_headers = rtx.open_table(HASHES)?.len()?;
        let file_bytes = std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
        let mut created = Vec::new();
        for item in rtx.open_table(CREATED)?.iter()? {
            let (k, v) = item?;
            created.push((Address::from_slice(k.value()), v.value()));
        }
        Ok(ArchiveStats {
            head: self.head()?,
            watches: self.watchlist()?,
            created,
            slot_records,
            slots_done: done,
            slots_pending: pending,
            slots_lost: lost,
            retained_headers,
            file_bytes,
        })
    }

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
}

/// `true` if this account's changes show a contract being created: a code
/// change to non-empty code that is not an EIP-7702 delegation designator
/// (`0xef0100 || address`, which an EOA can set and clear while keeping its
/// storage). Contract creation at an address with non-empty storage is
/// impossible (EIP-7610), so this implies "no storage before this block".
pub(crate) fn creation_in(acc: &bal_codec::AccountChanges) -> bool {
    acc.code_changes
        .iter()
        .any(|c| !c.new_code.is_empty() && !c.new_code.starts_with(&[0xef, 0x01, 0x00]))
}

/// Once an address is known to be created, every slot's pre-value is zero:
/// mark whatever was pending or lost as done and drop the retry entries.
pub(crate) fn settle_created(
    boot: &mut redb::Table<'_, &[u8], &[u8]>,
    pending: &mut redb::Table<'_, &[u8], u64>,
    addr: Address,
) -> Result<()> {
    for k in collect_prefix_keys(boot, addr.as_slice())? {
        boot.insert(k.as_slice(), encode_boot(BootState::Done).as_slice())?;
    }
    for k in collect_prefix_keys(pending, addr.as_slice())? {
        pending.remove(k.as_slice())?;
    }
    Ok(())
}

/// v1/v2 -> v3: regroup the per-slot block index into one entry per
/// (address, block), then drop the old table. Runs inside the open
/// transaction, so a crash midway leaves the old layout and version intact.
fn migrate_block_index(txn: &redb::WriteTransaction) -> Result<()> {
    let mut grouped: std::collections::BTreeMap<(Address, u64), Vec<B256>> =
        std::collections::BTreeMap::new();
    {
        let old = txn.open_table(LEGACY_BLOCKIDX)?;
        for item in old.iter()? {
            let (k, _) = item?;
            let (a, b, s) = parse_legacy_blockidx_key(k.value())
                .ok_or(ArchiveError::Corrupt("legacy blockidx"))?;
            grouped.entry((a, b)).or_default().push(s);
        }
    }
    {
        let mut new = txn.open_table(BLOCKIDX)?;
        for ((a, b), slots) in &grouped {
            new.insert(
                blockidx_key(*a, *b).as_slice(),
                encode_slots(slots).as_slice(),
            )?;
        }
    }
    txn.delete_table(LEGACY_BLOCKIDX)?;
    Ok(())
}

pub(crate) fn anchor_key(addr: Address) -> String {
    format!("{META_ANCHOR}{addr}")
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
pub(crate) fn collect_prefix_keys<V: redb::Value + 'static>(
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
