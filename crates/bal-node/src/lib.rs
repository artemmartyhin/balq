//! Node.js bindings. Every function here converts arguments, calls a crate
//! below, and converts the result back. Nothing is decided here.
//!
//! Errors: `NotAvailable` is thrown as a JS `Error` whose message starts
//! with `[NotAvailable:<Variant>]`; `lib.js` turns that into a
//! `NotAvailableError` with a `.code` property. Never `null`.

use alloy_primitives::{Address, B256, U256};
use bal_archive::{Archive as RsArchive, ArchiveConfig, NotAvailable, Provenance};
use bal_layout::{Layout as RsLayout, Location as RsLocation};
use bal_source::{Fallback, JsonRpcSource};
use napi::bindgen_prelude::*;
use napi_derive::napi;
use std::sync::Arc;

fn err(e: impl std::fmt::Display) -> Error {
    Error::from_reason(e.to_string())
}

fn na(e: NotAvailable) -> Error {
    let code = match &e {
        NotAvailable::NotWatched(_) => "NotWatched",
        NotAvailable::BeforeStart { .. } => "BeforeStart",
        NotAvailable::AfterHead { .. } => "AfterHead",
        NotAvailable::NotSynced => "NotSynced",
        NotAvailable::InvalidRange { .. } => "InvalidRange",
        NotAvailable::NeverRecorded => "NeverRecorded",
        NotAvailable::UnknownBefore { .. } => "UnknownBefore",
        NotAvailable::Internal(_) => "Internal",
    };
    Error::from_reason(format!("[NotAvailable:{code}] {e}"))
}

fn addr(s: &str) -> Result<Address> {
    s.parse::<Address>()
        .map_err(|e| err(format!("bad address {s}: {e}")))
}

/// Slot / word: 0x-hex of any length (left-padded) or a decimal string.
fn word(s: &str) -> Result<B256> {
    let s = s.trim();
    let u = if let Some(h) = s.strip_prefix("0x") {
        U256::from_str_radix(h, 16).map_err(|e| err(format!("bad hex {s}: {e}")))?
    } else {
        s.parse::<U256>()
            .map_err(|e| err(format!("bad number {s}: {e}")))?
    };
    Ok(B256::from(u.to_be_bytes::<32>()))
}

fn hex32(b: B256) -> String {
    format!("{b}")
}

/// Block numbers arrive as JS numbers; reject anything that is not a
/// non-negative integer instead of letting `as u64` turn -1 or 1.5 into a
/// plausible block.
fn blocknum(n: f64, what: &str) -> Result<u64> {
    if !n.is_finite() || n < 0.0 || n.fract() != 0.0 || n > 9_007_199_254_740_991.0 {
        return Err(err(format!(
            "{what} must be a non-negative integer, got {n}"
        )));
    }
    Ok(n as u64)
}

/// Primary JSON-RPC source, optionally backed by a second endpoint that is
/// asked only when the primary cannot answer. Both are verified the same way.
fn source_with_backup(
    rpc_url: String,
    backup: Option<String>,
) -> Fallback<JsonRpcSource, Option<JsonRpcSource>> {
    Fallback::new(JsonRpcSource::new(rpc_url), backup.map(JsonRpcSource::new))
}

fn prov(p: Provenance) -> String {
    match p {
        Provenance::Bal => "bal",
        Provenance::Proof => "proof",
        Provenance::Imported => "imported",
        Provenance::Unverified => "unverified",
    }
    .to_string()
}

/// Options for `Archive.open`.
#[napi(object)]
pub struct ArchiveOptions {
    /// Blocks behind head the node still serves eth_getProof
    /// (reth: `--rpc.eth-proof-window`). Default 120.
    pub proof_window: Option<u32>,
    /// Store every intra-block change instead of the last one. Fixed at creation.
    pub full_detail: Option<bool>,
    /// Apply blocks without a BAL hash in the header. Debug only.
    pub allow_unverified: Option<bool>,
}

/// A stored word with its provenance.
#[napi(object)]
pub struct StorageValue {
    /// 0x-prefixed 32-byte word.
    pub value: String,
    /// "bal" | "proof" | "imported" | "unverified"
    pub provenance: String,
    /// Block at which this value was set (watch start - 1 for proven pre-values).
    pub set_at: f64,
    /// Position within `setAt`.
    pub index: u32,
}

/// One recorded change.
#[napi(object)]
pub struct HistoryEntry {
    /// Block of the change.
    pub block: f64,
    /// Position within the block.
    pub index: u32,
    /// 0x-prefixed 32-byte post-value.
    pub value: String,
    /// "bal" for entries inside a valid range.
    pub provenance: String,
}

/// Archive head.
#[napi(object)]
pub struct Head {
    /// Last applied block.
    pub number: f64,
    /// Its hash.
    pub hash: String,
}

/// One watchlist entry.
#[napi(object)]
pub struct WatchEntry {
    /// Watched address.
    pub address: String,
    /// First block covered.
    pub from: f64,
}

/// What one `sync()` did.
#[napi(object)]
pub struct SyncReport {
    /// First block this pass tried to apply.
    pub from: Option<f64>,
    /// Last block applied.
    pub to: Option<f64>,
    /// Blocks applied.
    pub blocks_applied: f64,
    /// Fork point, if a reorg was rolled back.
    pub reorged_to: Option<f64>,
    /// Slot records written.
    pub slots_written: f64,
    /// Pre-values proven in this pass.
    pub bootstrapped: f64,
    /// Slots still awaiting a proof.
    pub bootstrap_pending: f64,
    /// Slots whose pre-value became unobtainable.
    pub bootstrap_lost: f64,
    /// Blocks applied without a BAL hash (debug only).
    pub unverified_blocks: f64,
}

/// Options for `backfill()`.
#[napi(object)]
#[derive(Default)]
pub struct BackfillOptions {
    /// Lowest block to read (inclusive). Default: the contract's creation.
    pub to: Option<f64>,
    /// Stop after this many blocks (`stopped === "budget"`); call again to continue.
    pub max_blocks: Option<f64>,
    /// Stop as soon as every slot with an unknown earlier value has found its
    /// last write before the current start.
    pub resolve_only: Option<bool>,
}

/// What one `backfill()` call did.
#[napi(object)]
pub struct BackfillReport {
    /// Watch start before the call.
    pub from: f64,
    /// Watch start after the call: the lowest block now covered.
    pub to: f64,
    /// Blocks read and verified.
    pub blocks_scanned: f64,
    /// Slot records written.
    pub records_written: f64,
    /// Slots whose unknown earlier value was found.
    pub slots_resolved: f64,
    /// Slots whose earlier value is still unknown.
    pub unresolved: f64,
    /// Creation block, if known after the call.
    pub created_at: Option<f64>,
    /// `"target" | "creation" | "resolved" | "budget" | "preBal" | "historyUnavailable" | "nothing"`.
    pub stopped: String,
    /// Block for `creation`, `preBal`, `historyUnavailable`.
    pub stopped_at: Option<f64>,
}

fn backfill_report(r: bal_archive::BackfillReport) -> BackfillReport {
    use bal_archive::BackfillStop as S;
    let (stopped, stopped_at) = match r.stopped {
        S::Target => ("target", None),
        S::Creation(b) => ("creation", Some(b)),
        S::Resolved => ("resolved", None),
        S::Budget => ("budget", None),
        S::PreBal(b) => ("preBal", Some(b)),
        S::HistoryUnavailable(b) => ("historyUnavailable", Some(b)),
        S::Nothing => ("nothing", None),
    };
    BackfillReport {
        from: r.from as f64,
        to: r.to as f64,
        blocks_scanned: r.blocks_scanned as f64,
        records_written: r.records_written as f64,
        slots_resolved: r.slots_resolved as f64,
        unresolved: r.unresolved as f64,
        created_at: r.created_at.map(|b| b as f64),
        stopped: stopped.to_string(),
        stopped_at: stopped_at.map(|b| b as f64),
    }
}

/// The archive. Safe to read from while `sync()` is in flight.
#[napi]
pub struct Archive {
    inner: Arc<RsArchive>,
}

#[napi]
impl Archive {
    /// Open (or create) an archive file.
    #[napi(factory)]
    pub fn open(path: String, options: Option<ArchiveOptions>) -> Result<Archive> {
        let mut cfg = ArchiveConfig::default();
        if let Some(o) = options {
            if let Some(w) = o.proof_window {
                cfg.bootstrap_window = u64::from(w);
            }
            if let Some(f) = o.full_detail {
                cfg.full_detail = f;
            }
            if let Some(a) = o.allow_unverified {
                cfg.allow_unverified = a;
            }
        }
        Ok(Archive {
            inner: Arc::new(RsArchive::open_with(path, cfg).map_err(err)?),
        })
    }

    /// Start accumulating `address` from `fromBlock` (must be > current head).
    #[napi]
    pub fn watch(&self, address: String, from_block: f64) -> Result<()> {
        self.inner
            .watch(addr(&address)?, blocknum(from_block, "fromBlock")?)
            .map_err(err)
    }

    /// Stop watching and delete the address's data.
    #[napi]
    pub fn unwatch(&self, address: String) -> Result<()> {
        self.inner.unwatch(addr(&address)?).map_err(err)
    }

    /// Watched addresses with their start blocks.
    #[napi]
    pub fn watchlist(&self) -> Result<Vec<WatchEntry>> {
        Ok(self
            .inner
            .watchlist()
            .map_err(err)?
            .into_iter()
            .map(|(a, f)| WatchEntry {
                address: format!("{a}"),
                from: f as f64,
            })
            .collect())
    }

    /// Last applied block, or null if nothing was synced yet.
    #[napi]
    pub fn head(&self) -> Result<Option<Head>> {
        Ok(self.inner.head().map_err(err)?.map(|(n, h)| Head {
            number: n as f64,
            hash: hex32(h),
        }))
    }

    /// Fetch, verify and apply blocks from `rpcUrl` up to its head. Bootstraps
    /// first-seen slots via eth_getProof unless `bootstrap === false`.
    /// `backupRpc` (an archive endpoint, any provider) is asked only when the
    /// primary lacks a block's BAL or cannot prove a slot; its answers are
    /// verified exactly like the primary's.
    #[napi(ts_return_type = "Promise<SyncReport>")]
    pub fn sync<'env>(
        &self,
        env: &'env Env,
        rpc_url: String,
        prove: Option<bool>,
        backup_rpc: Option<String>,
    ) -> Result<PromiseRaw<'env, SyncReport>> {
        let inner = self.inner.clone();
        let do_bootstrap = prove.unwrap_or(false);
        env.spawn_future(async move {
            let src = source_with_backup(rpc_url, backup_rpc);
            let state: Option<&dyn bal_source::StateSource> =
                if do_bootstrap { Some(&src) } else { None };
            let r = inner.sync(&src, state).await.map_err(err)?;
            Ok(SyncReport {
                from: r.from.map(|x| x as f64),
                to: r.to.map(|x| x as f64),
                blocks_applied: r.blocks_applied as f64,
                reorged_to: r.reorged_to.map(|x| x as f64),
                slots_written: r.slots_written as f64,
                bootstrapped: r.bootstrapped as f64,
                bootstrap_pending: r.bootstrap_pending as f64,
                bootstrap_lost: r.bootstrap_lost as f64,
                unverified_blocks: r.unverified_blocks as f64,
            })
        })
    }

    /// Extend `address`'s history backwards by reading older blocks' BALs
    /// (no proofs; every block verified against its header). Default: until
    /// the contract's creation. Resolves with a report; call again after
    /// `stopped === "budget"`.
    #[napi(ts_return_type = "Promise<BackfillReport>")]
    pub fn backfill<'env>(
        &self,
        env: &'env Env,
        rpc_url: String,
        address: String,
        options: Option<BackfillOptions>,
        backup_rpc: Option<String>,
    ) -> Result<PromiseRaw<'env, BackfillReport>> {
        let inner = self.inner.clone();
        let a = addr(&address)?;
        let o = options.unwrap_or_default();
        let opts = bal_archive::BackfillOpts {
            to: o.to.map(|n| blocknum(n, "to")).transpose()?,
            max_blocks: o.max_blocks.map(|n| blocknum(n, "maxBlocks")).transpose()?,
            resolve_only: o.resolve_only.unwrap_or(false),
        };
        env.spawn_future(async move {
            let src = source_with_backup(rpc_url, backup_rpc);
            let r = inner.backfill(&src, a, opts).await.map_err(err)?;
            Ok(backfill_report(r))
        })
    }

    /// `backfill()` for several addresses in one backward walk: every block
    /// is fetched once and applied to each address. One report per address,
    /// in input order.
    #[napi(ts_return_type = "Promise<BackfillReport[]>")]
    pub fn backfill_many<'env>(
        &self,
        env: &'env Env,
        rpc_url: String,
        addresses: Vec<String>,
        options: Option<BackfillOptions>,
        backup_rpc: Option<String>,
    ) -> Result<PromiseRaw<'env, Vec<BackfillReport>>> {
        let inner = self.inner.clone();
        let addrs = addresses
            .iter()
            .map(|a| addr(a))
            .collect::<Result<Vec<_>>>()?;
        let o = options.unwrap_or_default();
        let opts = bal_archive::BackfillOpts {
            to: o.to.map(|n| blocknum(n, "to")).transpose()?,
            max_blocks: o.max_blocks.map(|n| blocknum(n, "maxBlocks")).transpose()?,
            resolve_only: o.resolve_only.unwrap_or(false),
        };
        env.spawn_future(async move {
            let src = source_with_backup(rpc_url, backup_rpc);
            let reps = inner.backfill_many(&src, &addrs, opts).await.map_err(err)?;
            Ok(reps.into_iter().map(backfill_report).collect())
        })
    }

    /// Value of `slot` at the end of `block`. Throws NotAvailableError, never returns null.
    #[napi]
    pub fn storage_at(&self, address: String, slot: String, block: f64) -> Result<StorageValue> {
        let v = self
            .inner
            .storage_at(addr(&address)?, word(&slot)?, blocknum(block, "block")?)
            .map_err(na)?;
        Ok(StorageValue {
            value: hex32(v.value),
            provenance: prov(v.provenance),
            set_at: v.set_at as f64,
            index: v.index,
        })
    }

    /// Changes of `slot` in `[from, to)`.
    #[napi]
    pub fn history(
        &self,
        address: String,
        slot: String,
        from: f64,
        to: f64,
    ) -> Result<Vec<HistoryEntry>> {
        Ok(self
            .inner
            .history(
                addr(&address)?,
                word(&slot)?,
                blocknum(from, "from")?..blocknum(to, "to")?,
            )
            .map_err(na)?
            .into_iter()
            .map(|e| HistoryEntry {
                block: e.block as f64,
                index: e.index,
                value: hex32(e.value),
                provenance: prov(e.provenance),
            })
            .collect())
    }

    /// Slots of `address` written in `block`.
    #[napi]
    pub fn changed_slots(&self, address: String, block: f64) -> Result<Vec<String>> {
        Ok(self
            .inner
            .changed_slots(addr(&address)?, blocknum(block, "block")?)
            .map_err(na)?
            .into_iter()
            .map(hex32)
            .collect())
    }

    /// Prove a never-changed slot at the archive head (lazy bootstrap).
    #[napi(ts_return_type = "Promise<void>")]
    pub fn bootstrap_slot<'env>(
        &self,
        env: &'env Env,
        rpc_url: String,
        address: String,
        slot: String,
        backup_rpc: Option<String>,
    ) -> Result<PromiseRaw<'env, ()>> {
        let inner = self.inner.clone();
        let a = addr(&address)?;
        let s = word(&slot)?;
        env.spawn_future(async move {
            let src = source_with_backup(rpc_url, backup_rpc);
            inner.bootstrap_slot(&src, a, s).await.map_err(err)?;
            Ok(())
        })
    }
}

/// Where a value lives.
#[napi(object)]
pub struct Location {
    /// 0x-prefixed storage slot.
    pub slot: String,
    /// Byte offset from the low-order end of the word.
    pub offset: u32,
    /// Size in bytes.
    pub size: u32,
    /// solc type id, e.g. `t_uint128`.
    pub type_id: String,
}

/// A field name with its location.
#[napi(object)]
pub struct NamedLocation {
    /// Path such as `totals.index` or `items[3]`.
    pub name: String,
    /// Where it lives.
    pub location: Location,
}

fn loc_out(l: RsLocation) -> Location {
    Location {
        slot: hex32(l.slot),
        offset: l.offset as u32,
        size: l.size as u32,
        type_id: l.type_id,
    }
}

fn loc_in(l: Location) -> Result<RsLocation> {
    Ok(RsLocation {
        slot: word(&l.slot)?,
        offset: l.offset as usize,
        size: l.size as usize,
        type_id: l.type_id,
    })
}

/// A parsed solc storage layout.
#[napi]
pub struct Layout {
    inner: RsLayout,
}

#[napi]
impl Layout {
    /// From a solc storageLayout JSON string or a whole forge/hardhat artifact.
    #[napi(factory)]
    pub fn from_json(json: String) -> Result<Layout> {
        Ok(Layout {
            inner: RsLayout::from_json(&json).map_err(err)?,
        })
    }

    /// From a file containing either of the above.
    #[napi(factory)]
    pub fn from_file(path: String) -> Result<Layout> {
        Ok(Layout {
            inner: RsLayout::from_artifact(path).map_err(err)?,
        })
    }

    /// Resolve `totals.index`, `balances[0xabc…]`, `items[2]`, `items.length`.
    #[napi]
    pub fn locate(&self, path: String) -> Result<Location> {
        Ok(loc_out(self.inner.locate(&path).map_err(err)?))
    }

    /// Decode a 32-byte word at a location. Returns the value as a string
    /// (decimal for integers, "true"/"false", 0x-address, or the raw word).
    #[napi]
    pub fn decode(&self, location: Location, word_hex: String) -> Result<String> {
        Ok(self
            .inner
            .decode(&loc_in(location)?, word(&word_hex)?)
            .to_string())
    }

    /// Every named field living in a raw slot (mapping entries excluded — keccak is one-way).
    #[napi]
    pub fn describe_slot(&self, slot: String) -> Result<Vec<NamedLocation>> {
        Ok(self
            .inner
            .describe_slot(word(&slot)?, 4096)
            .into_iter()
            .map(|(name, l)| NamedLocation {
                name,
                location: loc_out(l),
            })
            .collect())
    }

    /// Top-level variable names in declaration order.
    #[napi]
    pub fn fields(&self) -> Vec<String> {
        self.inner.fields().map(|e| e.label.clone()).collect()
    }

    /// What a path names: "value:<uint|int|bool|address|bytes|raw>", "struct",
    /// "mapping", "array" or "fixedArray". Drives the `view` proxy in lib.js.
    #[napi]
    pub fn kind_of(&self, path: String) -> Result<String> {
        use bal_layout::{PathKind, ValueKind};
        Ok(match self.inner.kind_of(&path).map_err(err)? {
            PathKind::Struct => "struct".into(),
            PathKind::Mapping => "mapping".into(),
            PathKind::Array => "array".into(),
            PathKind::FixedArray => "fixedArray".into(),
            PathKind::Value(k) => format!(
                "value:{}",
                match k {
                    ValueKind::Uint => "uint",
                    ValueKind::Int => "int",
                    ValueKind::Bool => "bool",
                    ValueKind::Address => "address",
                    ValueKind::Bytes => "bytes",
                    ValueKind::DynBytes => "dynbytes",
                    ValueKind::String => "string",
                    ValueKind::Raw => "raw",
                }
            ),
        })
    }

    /// Decode a word and say what the text is, so JS can turn it into
    /// `bigint` / `boolean` / `string` without guessing.
    #[napi]
    pub fn decode_value(&self, location: Location, word_hex: String) -> Result<DecodedValue> {
        use bal_layout::ValueKind;
        let (k, v) = self
            .inner
            .decode_typed(&loc_in(location)?, word(&word_hex)?);
        Ok(DecodedValue {
            kind: match k {
                ValueKind::Uint => "uint",
                ValueKind::Int => "int",
                ValueKind::Bool => "bool",
                ValueKind::Address => "address",
                ValueKind::Bytes => "bytes",
                ValueKind::DynBytes => "dynbytes",
                ValueKind::String => "string",
                ValueKind::Raw => "raw",
            }
            .into(),
            text: v.to_string(),
        })
    }

    /// TypeScript interface for this layout as the `view` proxy exposes it.
    #[napi]
    pub fn typescript(&self, name: String) -> String {
        self.inner.typescript(&name)
    }

    /// For a dynamic `bytes`/`string` whose slot holds `wordHex`: the extra
    /// slots holding the data (empty for short values). Read them at the
    /// same block and pass them to `decodeBytes`.
    #[napi]
    pub fn bytes_data_slots(&self, location: Location, word_hex: String) -> Result<Vec<String>> {
        Ok(self
            .inner
            .bytes_data_slots(&loc_in(location)?, word(&word_hex)?)
            .into_iter()
            .map(hex32)
            .collect())
    }

    /// Assemble a dynamic `bytes`/`string` from its slot word and data words.
    #[napi]
    pub fn decode_bytes(
        &self,
        location: Location,
        word_hex: String,
        chunks: Vec<String>,
    ) -> Result<DecodedValue> {
        use bal_layout::{Value, ValueKind};
        let loc = loc_in(location)?;
        let chunks = chunks.iter().map(|c| word(c)).collect::<Result<Vec<_>>>()?;
        let v = self.inner.decode_bytes(&loc, word(&word_hex)?, &chunks);
        let k = match &v {
            Value::Str(_) => ValueKind::String,
            Value::Bytes(_) => ValueKind::DynBytes,
            _ => ValueKind::Raw,
        };
        Ok(DecodedValue {
            kind: match k {
                ValueKind::String => "string",
                ValueKind::DynBytes => "dynbytes",
                _ => "raw",
            }
            .into(),
            text: v.to_string(),
        })
    }

    /// `describeSlot` plus mapping entries whose key is one of `keys`
    /// (addresses or 0x words / decimals).
    #[napi]
    pub fn describe_slot_with_keys(
        &self,
        slot: String,
        keys: Vec<String>,
    ) -> Result<Vec<NamedLocation>> {
        let keys = keys
            .iter()
            .map(|k| {
                if k.len() == 42 && k.starts_with("0x") {
                    addr(k).map(|a| B256::left_padding_from(a.as_slice()))
                } else {
                    word(k)
                }
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(self
            .inner
            .describe_slot_with_keys(word(&slot)?, 4096, &keys)
            .into_iter()
            .map(|(name, l)| NamedLocation {
                name,
                location: loc_out(l),
            })
            .collect())
    }
}

/// A decoded word with its kind.
#[napi(object)]
pub struct DecodedValue {
    /// "uint" | "int" | "bool" | "address" | "bytes" | "dynbytes" | "string" | "raw"
    pub kind: String,
    /// Decimal for integers, "true"/"false", the text of a string, 0x-hex otherwise.
    pub text: String,
}
