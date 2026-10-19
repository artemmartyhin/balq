//! Formatting shared by the commands. Text for humans, JSON for scripts —
//! never a `null` where the archive said "not available".

use alloy_primitives::{B256, U256};
use anyhow::{Context, Result};
use bal_archive::{NotAvailable, Provenance};
use bal_layout::Layout;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Slot or word: `0x`-hex of any length, or a decimal of any size. A bare
/// digit string is always decimal — never guessed as hex.
pub fn parse_slot(s: &str) -> Result<B256> {
    let s = s.trim();
    let u = match s.strip_prefix("0x") {
        Some(h) => U256::from_str_radix(h, 16).with_context(|| format!("bad hex slot {s}"))?,
        None => s
            .parse::<U256>()
            .with_context(|| format!("bad decimal slot {s} (prefix hex with 0x)"))?,
    };
    Ok(B256::from(u.to_be_bytes::<32>()))
}

/// Candidate mapping keys: comma-separated addresses (left-padded) or words
/// (`0x` hex / decimal), as `--keys` takes them.
pub fn parse_keys(s: &str) -> Result<Vec<B256>> {
    s.split(',')
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .map(|k| {
            if k.len() == 42 && k.starts_with("0x") {
                let a: alloy_primitives::Address =
                    k.parse().with_context(|| format!("bad key {k}"))?;
                Ok(B256::left_padding_from(a.as_slice()))
            } else {
                parse_slot(k)
            }
        })
        .collect()
}

/// Addresses as mapping-key candidates (left-padded words).
pub fn address_keys(addrs: &[alloy_primitives::Address]) -> Vec<B256> {
    addrs
        .iter()
        .map(|a| B256::left_padding_from(a.as_slice()))
        .collect()
}

/// Decimal when it fits comfortably, else the full word.
pub fn short(v: B256) -> String {
    let u = U256::from_be_bytes(v.0);
    if u < U256::from(u128::MAX) {
        format!("{u}")
    } else {
        format!("{v}")
    }
}

pub fn prov(p: Provenance) -> &'static str {
    match p {
        Provenance::Bal => "bal",
        Provenance::Proof => "proof",
        Provenance::Imported => "IMPORTED-UNVERIFIED",
        Provenance::Unverified => "UNVERIFIED",
    }
}

/// Stable machine-readable code for a miss.
pub fn na_code(e: &NotAvailable) -> &'static str {
    match e {
        NotAvailable::NotWatched(_) => "NotWatched",
        NotAvailable::BeforeStart { .. } => "BeforeStart",
        NotAvailable::AfterHead { .. } => "AfterHead",
        NotAvailable::NotSynced => "NotSynced",
        NotAvailable::InvalidRange { .. } => "InvalidRange",
        NotAvailable::NeverRecorded => "NeverRecorded",
        NotAvailable::UnknownBefore { .. } => "UnknownBefore",
        NotAvailable::Internal(_) => "Internal",
    }
}

/// Compact tag for a missing value in tabular output; `balq get` prints the full reason.
pub fn na_short(e: &NotAvailable) -> String {
    match e {
        NotAvailable::NotWatched(_) => "<not watched>".into(),
        NotAvailable::BeforeStart { .. } => "<before start>".into(),
        NotAvailable::AfterHead { .. } => "<after head>".into(),
        NotAvailable::NotSynced => "<not synced>".into(),
        NotAvailable::InvalidRange { start, end } => format!("<invalid range {start}..{end}>"),
        NotAvailable::NeverRecorded => "<unknown: backfill>".into(),
        NotAvailable::UnknownBefore { first_seen } => {
            format!("<unknown before @{first_seen}: backfill>")
        }
        NotAvailable::Internal(s) => format!("<internal: {s}>"),
    }
}

/// JSON form of a miss: `{ "error": { "code", "message" } }`.
pub fn na_json(e: &NotAvailable) -> Value {
    json!({ "error": { "code": na_code(e), "message": e.to_string() } })
}

pub fn load_layout(p: &Path) -> Result<Layout> {
    Layout::from_artifact(p).with_context(|| format!("loading layout {}", p.display()))
}

/// A `--layout` argument of a one-address command (`get`, `diff`) as a
/// file: the path itself, or for `auto` the cached Sourcify layout of
/// `addr` — fetched first if `index` has not fetched it yet, which needs
/// `rpc` (or `rpc` in balq.toml) to learn the chain.
pub fn layout_file(
    ctx: &crate::commands::Ctx,
    p: &Path,
    addr: alloy_primitives::Address,
    rpc: Option<&str>,
) -> Result<PathBuf> {
    if !crate::sourcify::is_auto(p) {
        return Ok(p.to_path_buf());
    }
    if let Some(c) = crate::sourcify::cached_any_chain(&ctx.data, addr) {
        return Ok(c);
    }
    let rpc = ctx.cfg.rpc(rpc.map(String::from)).context(
        "--layout auto: no cached layout for this address, and no RPC to learn the chain",
    )?;
    let chain = crate::sourcify::chain_id(&rpc)?;
    let key = ctx.cfg.etherscan_key();
    let opts = crate::sourcify::Opts {
        server: ctx
            .cfg
            .sourcify
            .as_deref()
            .unwrap_or(crate::sourcify::DEFAULT_SERVER),
        import: key.is_some(),
        etherscan_key: key.as_deref(),
    };
    let f = crate::sourcify::fetch(
        &opts,
        chain,
        addr,
        &crate::sourcify::cache_path(&ctx.data, chain, addr),
    )?;
    Ok(f.path)
}

/// Every named field living in a raw slot, decoded from `word` if given;
/// `keys` are candidate mapping keys.
pub fn named(layout: &Layout, slot: B256, word: Option<B256>, keys: &[B256]) -> Vec<String> {
    layout
        .describe_slot_with_keys(slot, 4096, keys)
        .into_iter()
        .map(|(name, loc)| match word {
            Some(w) => format!("{name} = {}", layout.decode(&loc, w)),
            None => name,
        })
        .collect()
}

/// Print one JSON document.
pub fn emit(v: &Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
    );
}

/// A contract as the CLI knows it: its storage layout and, when the file
/// carried an ABI, its `view` functions — so `eth_call` on a getter can be
/// answered from the archive.
pub struct Contract {
    pub layout: Layout,
    pub getters: bal_layout::Getters,
    /// Where it came from, for messages.
    pub source: std::path::PathBuf,
}

fn load_contract(p: &Path) -> Result<Contract> {
    let layout = load_layout(p)?;
    let getters = std::fs::read_to_string(p)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .map(|v| bal_layout::Getters::from_artifact_json(&v))
        .unwrap_or_default();
    Ok(Contract {
        layout,
        getters,
        source: p.to_path_buf(),
    })
}

/// Layouts by address, with an optional default for the rest. A layout
/// given as `auto` is not loaded here: it is recorded in `auto_default` /
/// `auto_per` and fetched by [`Layouts::resolve_auto`].
#[derive(Default)]
pub struct Layouts {
    pub default: Option<Contract>,
    pub per: std::collections::HashMap<alloy_primitives::Address, Contract>,
    /// `layout = "auto"` / `--layout auto`: every address without its own entry.
    pub auto_default: bool,
    /// `0xADDR=auto`.
    pub auto_per: Vec<alloy_primitives::Address>,
}

/// One address's outcome of [`Layouts::resolve_auto`], for messages.
pub enum AutoNote {
    /// Fetched now.
    Fetched(alloy_primitives::Address, crate::sourcify::Fetched),
    /// Taken from the cache (no fetch asked, or the fetch failed: the error).
    Cached(alloy_primitives::Address, PathBuf, Option<String>),
    /// Neither: the address stays without names.
    Failed(alloy_primitives::Address, String),
}

impl Layouts {
    /// From `balq.toml` (`layout`, `[layouts]`) and `--layout` flags, each
    /// either a path (the default) or `0xADDR=path` (for one address).
    /// Flags win over the file.
    pub fn load(cfg: &crate::config::Config, flags: &[String]) -> Result<Self> {
        let mut out = Self::default();
        if let Some(p) = &cfg.layout {
            out.set_default(p)?;
        }
        for (a, p) in &cfg.layouts {
            out.set_one(*a, p)?;
        }
        for f in flags {
            match f.split_once('=') {
                Some((addr, path)) if addr.starts_with("0x") => {
                    let a: alloy_primitives::Address = addr
                        .parse()
                        .with_context(|| format!("bad address in --layout {f}"))?;
                    out.set_one(a, Path::new(path))?;
                }
                _ => out.set_default(Path::new(f))?,
            }
        }
        Ok(out)
    }

    fn set_default(&mut self, p: &Path) -> Result<()> {
        if crate::sourcify::is_auto(p) {
            (self.default, self.auto_default) = (None, true);
        } else {
            (self.default, self.auto_default) = (Some(load_contract(p)?), false);
        }
        Ok(())
    }

    fn set_one(&mut self, a: alloy_primitives::Address, p: &Path) -> Result<()> {
        self.auto_per.retain(|x| *x != a);
        if crate::sourcify::is_auto(p) {
            self.per.remove(&a);
            self.auto_per.push(a);
        } else {
            self.per.insert(a, load_contract(p)?);
        }
        Ok(())
    }

    /// Fetch (or take from the cache) every `auto` layout among `addrs`.
    /// `refresh`: ask Sourcify even when a cached copy exists (falling back
    /// to it if Sourcify cannot be reached); otherwise the cache wins and
    /// Sourcify is asked only for what is missing. A failure leaves the
    /// address without names and says so — it never stops indexing.
    pub fn resolve_auto(
        &mut self,
        cfg: &crate::config::Config,
        data: &Path,
        chain: u64,
        addrs: &[alloy_primitives::Address],
        refresh: bool,
    ) -> Vec<AutoNote> {
        let wanted: Vec<_> = addrs
            .iter()
            .copied()
            .filter(|a| {
                self.auto_per.contains(a) || (self.auto_default && !self.per.contains_key(a))
            })
            .collect();
        let key = cfg.etherscan_key();
        let opts = crate::sourcify::Opts {
            server: cfg
                .sourcify
                .as_deref()
                .unwrap_or(crate::sourcify::DEFAULT_SERVER),
            import: key.is_some(),
            etherscan_key: key.as_deref(),
        };
        let mut notes = Vec::new();
        for a in wanted {
            let cache = crate::sourcify::cache_path(data, chain, a);
            if !refresh && cache.is_file() {
                if let Ok(c) = load_contract(&cache) {
                    self.per.insert(a, c);
                    notes.push(AutoNote::Cached(a, cache, None));
                    continue;
                }
            }
            let note = match crate::sourcify::fetch(&opts, chain, a, &cache) {
                Ok(f) => match load_contract(&f.path) {
                    Ok(c) => {
                        self.per.insert(a, c);
                        AutoNote::Fetched(a, f)
                    }
                    Err(e) => AutoNote::Failed(a, format!("{e:#}")),
                },
                Err(e) => match cache.is_file().then(|| load_contract(&cache)) {
                    Some(Ok(c)) => {
                        self.per.insert(a, c);
                        AutoNote::Cached(a, cache, Some(format!("{e:#}")))
                    }
                    _ => AutoNote::Failed(a, format!("{e:#}")),
                },
            };
            notes.push(note);
        }
        notes
    }

    pub fn contract(&self, addr: &alloy_primitives::Address) -> Option<&Contract> {
        self.per.get(addr).or(self.default.as_ref())
    }

    pub fn get(&self, addr: &alloy_primitives::Address) -> Option<&Layout> {
        self.contract(addr).map(|c| &c.layout)
    }

    pub fn is_empty(&self) -> bool {
        self.default.is_none() && self.per.is_empty()
    }

    /// Any `auto` entry still to resolve?
    pub fn wants_auto(&self) -> bool {
        self.auto_default || !self.auto_per.is_empty()
    }
}

/// JSON form of a miss with every field, so a client can rebuild the
/// exact `NotAvailable` (the `serve` protocol).
pub fn na_json_full(e: &NotAvailable) -> Value {
    let mut err = json!({ "code": na_code(e), "message": e.to_string() });
    match e {
        NotAvailable::NotWatched(a) => err["address"] = json!(a),
        NotAvailable::BeforeStart { requested, start } => {
            err["requested"] = json!(requested);
            err["start"] = json!(start);
        }
        NotAvailable::AfterHead { requested, head } => {
            err["requested"] = json!(requested);
            err["head"] = json!(head);
        }
        NotAvailable::InvalidRange { start, end } => {
            err["start"] = json!(start);
            err["end"] = json!(end);
        }
        NotAvailable::UnknownBefore { first_seen } => err["first_seen"] = json!(first_seen),
        _ => {}
    }
    json!({ "error": err })
}

/// `balq status --json` shape, shared with the `serve` protocol.
pub fn stats_json(s: &bal_archive::ArchiveStats) -> Value {
    let created_at =
        |a: &alloy_primitives::Address| s.created.iter().find(|(c, _)| c == a).map(|(_, b)| *b);
    json!({
        "head": s.head.map(|(n, h)| json!({ "number": n, "hash": h })),
        "watch": s.watches.iter().map(|(a, f)| json!({
            "address": a, "from": f, "createdAt": created_at(a),
        })).collect::<Vec<_>>(),
        "created": s.created.iter().map(|(a, b)| json!({ "address": a, "block": b })).collect::<Vec<_>>(),
        "slotRecords": s.slot_records,
        "bootstrap": { "done": s.slots_done, "pending": s.slots_pending, "lost": s.slots_lost },
        "retainedHeaders": s.retained_headers,
        "fileBytes": s.file_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../bal-layout/tests/fixtures/Playground.layout.json"
    );

    #[test]
    fn layouts_flags_and_config() {
        let a: alloy_primitives::Address = "0x35825972e2ca90851b14576C531F13dA0B5d53ce"
            .parse()
            .unwrap();
        let b: alloy_primitives::Address = "0xf43A4277C415e02c2B2FCe1F4bef8DB890F95959"
            .parse()
            .unwrap();
        let mut cfg = crate::config::Config::default();
        cfg.layouts.insert(b, FIXTURE.into());
        let l = Layouts::load(&cfg, &[format!("{a}={FIXTURE}"), FIXTURE.to_string()]).unwrap();
        assert!(l.get(&a).is_some() && l.get(&b).is_some());
        assert!(
            l.get(&alloy_primitives::Address::ZERO).is_some(),
            "default applies to the rest"
        );
        assert!(!l.is_empty());
        let none = Layouts::load(&crate::config::Config::default(), &[]).unwrap();
        assert!(none.is_empty() && none.get(&a).is_none());
        assert!(Layouts::load(&cfg, &[format!("0xnope={FIXTURE}")]).is_err());
    }

    #[test]
    fn slot_parsing_and_miss_json() {
        assert_eq!(parse_slot("0x10").unwrap(), parse_slot("16").unwrap());
        assert!(parse_slot("0xzz").is_err());
        let v = na_json_full(&NotAvailable::UnknownBefore { first_seen: 7 });
        assert_eq!(v["error"]["code"], "UnknownBefore");
        assert_eq!(v["error"]["first_seen"], 7);
    }
}
