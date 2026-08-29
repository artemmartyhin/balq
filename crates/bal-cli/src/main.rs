//! `balq` — glue only. Every line here calls into a crate below; nothing is
//! decided here.

use alloy_primitives::{Address, B256, U256};
use anyhow::{anyhow, Context, Result};
use bal_archive::{Archive, ArchiveConfig, NotAvailable, Provenance, StorageValue};
use bal_layout::Layout;
use bal_source::{Fallback, JsonRpcSource, StateSource};
use clap::{Parser, Subcommand};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "balq",
    version,
    about = "Local, verified archive of contract storage built from EIP-7928 BALs"
)]
struct Cli {
    /// Archive file.
    #[arg(long, global = true, default_value = "balq.redb")]
    data: PathBuf,
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Day 0: does this node serve BALs, for old blocks too, and proofs?
    Probe {
        #[arg(long)]
        rpc: String,
        /// How many blocks back the "old" block is.
        #[arg(long, default_value_t = 50_000)]
        age: u64,
    },
    /// Start accumulating an address from a block (must be >= current head + 1).
    Watch {
        address: Address,
        #[arg(long)]
        from: u64,
    },
    /// Stop watching and delete the address's data.
    Unwatch { address: Address },
    /// Watchlist, head, config.
    Status,
    /// Pull blocks from the node, verify, apply, bootstrap.
    Sync {
        #[arg(long)]
        rpc: String,
        /// Skip eth_getProof bootstrap (slots stay pending; window still ticks).
        #[arg(long)]
        no_bootstrap: bool,
        /// Apply blocks whose header has no BAL hash. Debug only.
        #[arg(long)]
        allow_unverified: bool,
        /// Keep running: poll for new blocks and apply them as they arrive.
        /// This is the mode that keeps early bootstrap inside the node's
        /// proof window; one-shot sync after a pause loses pre-values.
        #[arg(long)]
        follow: bool,
        /// Poll interval in seconds for --follow.
        #[arg(long, default_value_t = 4)]
        poll: u64,
        /// How many blocks back the node still serves eth_getProof.
        #[arg(long, default_value_t = 120)]
        proof_window: u64,
        /// Second endpoint (any archive provider) asked only when --rpc lacks a
        /// block's BAL or cannot prove a slot. Verified exactly like --rpc.
        #[arg(long)]
        backup_rpc: Option<String>,
    },
    /// Read one slot (or one named field with --layout) at one block.
    Get {
        address: Address,
        /// Raw slot: decimal or 0x-hex.
        #[arg(long, conflicts_with = "field")]
        slot: Option<String>,
        /// Field path, e.g. `totals.index`, `balances[0xabc…]`, `items[2]`. Needs --layout.
        #[arg(long, requires = "layout")]
        field: Option<String>,
        /// solc storageLayout JSON or a forge/hardhat artifact containing one.
        #[arg(long)]
        layout: Option<PathBuf>,
        #[arg(long)]
        block: u64,
        /// If the slot was never bootstrapped, prove it now via this node.
        #[arg(long)]
        rpc: Option<String>,
        /// Backup endpoint for the proof, tried if --rpc cannot serve it.
        #[arg(long, requires = "rpc")]
        backup_rpc: Option<String>,
    },
    /// All changes of one slot in a block range `A..B` (half-open).
    History {
        address: Address,
        #[arg(long)]
        slot: String,
        #[arg(long)]
        range: String,
    },
    /// Storage diff of an address between two blocks (values at `from` vs at `to`).
    Diff {
        address: Address,
        #[arg(long)]
        from: u64,
        #[arg(long)]
        to: u64,
        /// Name slots via a storage layout. Unresolvable slots stay `[raw]`.
        #[arg(long)]
        layout: Option<PathBuf>,
    },
    /// Compare the archive against a journal of known-true rows
    /// (`{"block","address","slot","value"}` per line, as produced by testbed/poke.mjs).
    Verify {
        #[arg(long)]
        journal: PathBuf,
        /// Also print every matching row.
        #[arg(long)]
        show_matches: bool,
    },
    /// Emit a TypeScript interface for a storage layout, matching what
    /// `archive.view(addr, layout).at(block)` exposes in @balq/node.
    Typegen {
        /// solc storageLayout JSON or a forge/hardhat artifact containing one.
        layout: PathBuf,
        /// Interface name (default: file stem + "View").
        #[arg(long)]
        name: Option<String>,
    },
}

/// Slot or word: `0x`-hex of any length, or a decimal of any size. A bare
/// digit string is always decimal — never guessed as hex.
fn parse_slot(s: &str) -> Result<B256> {
    let s = s.trim();
    let u = match s.strip_prefix("0x") {
        Some(h) => U256::from_str_radix(h, 16).with_context(|| format!("bad hex slot {s}"))?,
        None => s
            .parse::<U256>()
            .with_context(|| format!("bad decimal slot {s} (prefix hex with 0x)"))?,
    };
    Ok(B256::from(u.to_be_bytes::<32>()))
}

fn short(v: B256) -> String {
    let u = U256::from_be_bytes(v.0);
    if u < U256::from(u128::MAX) {
        format!("{u}")
    } else {
        format!("{v}")
    }
}

fn prov(p: Provenance) -> &'static str {
    match p {
        Provenance::Bal => "bal",
        Provenance::Proof => "proof",
        Provenance::Imported => "IMPORTED-UNVERIFIED",
        Provenance::Unverified => "UNVERIFIED",
    }
}

/// Compact tag for a missing value in tabular output; `balq get` prints the full reason.
fn na_short(e: &NotAvailable) -> String {
    match e {
        NotAvailable::NotWatched(_) => "<not watched>".into(),
        NotAvailable::BeforeStart { .. } => "<before start>".into(),
        NotAvailable::AfterHead { .. } => "<after head>".into(),
        NotAvailable::NotSynced => "<not synced>".into(),
        NotAvailable::InvalidRange { start, end } => format!("<invalid range {start}..{end}>"),
        NotAvailable::NotBootstrapped => "<not bootstrapped>".into(),
        NotAvailable::BootstrapPending { first_seen } => {
            format!("<pending, first change @{first_seen}>")
        }
        NotAvailable::BootstrapLost { first_seen } => format!("<lost, first change @{first_seen}>"),
        NotAvailable::Internal(s) => format!("<internal: {s}>"),
    }
}

fn load_layout(p: &PathBuf) -> Result<Layout> {
    Layout::from_artifact(p).with_context(|| format!("loading layout {}", p.display()))
}

/// Render a raw word through the layout: every named field living in that slot.
fn named(layout: &Layout, slot: B256, word: Option<B256>) -> Vec<String> {
    layout
        .describe_slot(slot, 4096)
        .into_iter()
        .map(|(name, loc)| match word {
            Some(w) => format!("{name} = {}", layout.decode(&loc, w)),
            None => name,
        })
        .collect()
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let level = match cli.verbose {
        0 => "warn",
        1 => "info",
        _ => "debug",
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| level.into()),
        )
        .with_writer(std::io::stderr)
        .init();

    match cli.cmd {
        Cmd::Probe { rpc, age } => {
            let src = JsonRpcSource::new(&rpc);
            let r = src.probe(age).await?;
            println!(
                "client:        {}",
                r.client_version.as_deref().unwrap_or("?")
            );
            println!(
                "chain id:      {}",
                r.chain_id.map(|c| c.to_string()).unwrap_or("?".into())
            );
            println!("head:          {}", r.head);
            println!(
                "header field:  {}",
                if r.head_fields
                    .iter()
                    .any(|f| f == bal_source::BAL_HASH_FIELD)
                {
                    format!("`{}` present", bal_source::BAL_HASH_FIELD)
                } else {
                    format!(
                        "`{}` ABSENT — fields: {}",
                        bal_source::BAL_HASH_FIELD,
                        r.head_fields.join(", ")
                    )
                }
            );
            println!("method:        {}", bal_source::BAL_METHOD);
            println!();
            let show = |label: &str, p: &bal_source::BalProbe| {
                use bal_source::BalProbe::*;
                let line = match p {
                    Verified {
                        block,
                        accounts,
                        hash,
                    } => {
                        format!("block {block}: VERIFIED — {accounts} accounts, keccak(rlp(bal)) == header ({hash})")
                    }
                    NoHashInHeader { block, accounts } => {
                        format!("block {block}: served ({accounts} accounts) but header has no BAL hash — cannot verify")
                    }
                    Mismatch {
                        block,
                        computed,
                        expected,
                    } => {
                        format!("block {block}: HASH MISMATCH — computed {computed}, header {expected}. Codec/spec drift; do not build on this.")
                    }
                    Missing(b) => format!("block {b}: {} returned null", bal_source::BAL_METHOD),
                    Error(e) => format!("error: {e}"),
                };
                println!("{label:<16}{line}");
            };
            show("Q1 head", &r.head_probe);
            show("Q2 old", &r.old_probe);
            show("Q2 block 1", &r.earliest_probe);
            match &r.proof_window {
                Ok(0) => {
                    println!("eth_getProof    window 0 — proofs only at head.");
                    println!("                Early bootstrap (pre-value of a slot at its first change) is IMPOSSIBLE here:");
                    println!("                such slots become BootstrapLost; history is complete from each slot's first change.");
                    println!("                Own reth node: run with --rpc.eth-proof-window 128 (or more).");
                }
                Ok(w) => {
                    println!("eth_getProof    window {w} blocks — pass `sync --proof-window {w}`")
                }
                Err(e) => println!("eth_getProof    NOT served — no bootstrap at all: {e}"),
            }
        }
        Cmd::Watch { address, from } => {
            let ar = Archive::open(&cli.data)?;
            ar.watch(address, from)?;
            println!("watching {address} from block {from}");
        }
        Cmd::Unwatch { address } => {
            let ar = Archive::open(&cli.data)?;
            ar.unwatch(address)?;
            println!("unwatched {address}, data removed");
        }
        Cmd::Status => {
            let ar = Archive::open(&cli.data)?;
            match ar.head()? {
                Some((n, h)) => println!("head:  {n} ({h})"),
                None => println!("head:  (nothing synced yet)"),
            }
            let wl = ar.watchlist()?;
            println!("watch: {} address(es)", wl.len());
            for (a, s) in wl {
                println!("  {a}  from {s}");
            }
        }
        Cmd::Sync {
            rpc,
            no_bootstrap,
            allow_unverified,
            follow,
            poll,
            proof_window,
            backup_rpc,
        } => {
            let ar = Archive::open_with(
                &cli.data,
                ArchiveConfig {
                    allow_unverified,
                    bootstrap_window: proof_window,
                    ..Default::default()
                },
            )?;
            let src = Fallback::new(JsonRpcSource::new(&rpc), backup_rpc.map(JsonRpcSource::new));
            let state: Option<&dyn StateSource> = if no_bootstrap { None } else { Some(&src) };
            let mut rep;
            loop {
                rep = match ar.sync(&src, state).await {
                    Ok(r) => r,
                    Err(e) if follow => {
                        eprintln!("sync error: {e} — retrying in {poll}s");
                        tokio::time::sleep(std::time::Duration::from_secs(poll)).await;
                        continue;
                    }
                    Err(e) => return Err(e.into()),
                };
                if !follow {
                    break;
                }
                if rep.blocks_applied > 0 {
                    println!(
                        "{:?}..={:?}: {} block(s), {} record(s), bootstrap +{} proven / {} pending / {} lost{}",
                        rep.from,
                        rep.to,
                        rep.blocks_applied,
                        rep.slots_written,
                        rep.bootstrapped,
                        rep.bootstrap_pending,
                        rep.bootstrap_lost,
                        rep.reorged_to.map(|f| format!(" — REORG to {f}")).unwrap_or_default()
                    );
                }
                tokio::time::sleep(std::time::Duration::from_secs(poll)).await;
            }
            println!(
                "applied {} block(s) {:?}..={:?}, {} slot record(s)",
                rep.blocks_applied, rep.from, rep.to, rep.slots_written
            );
            if let Some(f) = rep.reorged_to {
                println!("reorg: rolled back to {f}");
            }
            println!(
                "bootstrap: {} proven, {} pending, {} lost",
                rep.bootstrapped, rep.bootstrap_pending, rep.bootstrap_lost
            );
            if rep.unverified_blocks > 0 {
                println!(
                    "WARNING: {} block(s) applied WITHOUT verification",
                    rep.unverified_blocks
                );
            }
        }
        Cmd::Get {
            address,
            slot,
            field,
            layout,
            block,
            rpc,
            backup_rpc,
        } => {
            let ar = Archive::open(&cli.data)?;
            let layout = layout.as_ref().map(load_layout).transpose()?;
            let (slot, loc) = match (&slot, &field, &layout) {
                (Some(s), _, _) => (parse_slot(s)?, None),
                (None, Some(f), Some(l)) => {
                    let loc = l.locate(f)?;
                    (loc.slot, Some(loc))
                }
                _ => return Err(anyhow!("pass --slot, or --field with --layout")),
            };
            let mut res = ar.storage_at(address, slot, block);
            if matches!(res, Err(NotAvailable::NotBootstrapped)) {
                if let Some(rpc) = rpc {
                    let src =
                        Fallback::new(JsonRpcSource::new(&rpc), backup_rpc.map(JsonRpcSource::new));
                    ar.bootstrap_slot(&src, address, slot).await?;
                    res = ar.storage_at(address, slot, block);
                }
            }
            match res {
                Ok(v) => match (&loc, &layout) {
                    (Some(loc), Some(l)) => println!(
                        "{} = {}  (slot {} @ {}, {})",
                        field.as_deref().unwrap_or(""),
                        l.decode(loc, v.value),
                        slot,
                        v.set_at,
                        prov(v.provenance)
                    ),
                    _ => {
                        println!(
                            "{}  ({} @ {}, {})",
                            short(v.value),
                            v.value,
                            v.set_at,
                            prov(v.provenance)
                        );
                        if let Some(l) = &layout {
                            for n in named(l, slot, Some(v.value)) {
                                println!("  {n}");
                            }
                        }
                    }
                },
                Err(e) => {
                    println!("NOT AVAILABLE: {e}");
                    if matches!(e, NotAvailable::NotBootstrapped) {
                        println!("hint: pass --rpc <url> to prove the slot's value now");
                    }
                    std::process::exit(2);
                }
            }
        }
        Cmd::History {
            address,
            slot,
            range,
        } => {
            let ar = Archive::open(&cli.data)?;
            let slot = parse_slot(&slot)?;
            let (a, b) = range
                .split_once("..")
                .ok_or_else(|| anyhow!("range must be A..B"))?;
            let r = a.parse::<u64>()?..b.parse::<u64>()?;
            match ar.history(address, slot, r) {
                Ok(h) => {
                    for e in h {
                        println!(
                            "{:>10}  #{:<4} {}  {}",
                            e.block,
                            e.index,
                            short(e.value),
                            prov(e.provenance)
                        );
                    }
                }
                Err(e) => {
                    println!("NOT AVAILABLE: {e}");
                    std::process::exit(2);
                }
            }
        }
        Cmd::Diff {
            address,
            from,
            to,
            layout,
        } => {
            let ar = Archive::open(&cli.data)?;
            if to <= from {
                return Err(anyhow!("--to must be > --from"));
            }
            let layout = layout.as_ref().map(load_layout).transpose()?;
            let mut slots = std::collections::BTreeSet::new();
            for b in from + 1..=to {
                for s in ar.changed_slots(address, b).map_err(|e| anyhow!("{e}"))? {
                    slots.insert(s);
                }
            }
            let fmt = |r: &std::result::Result<StorageValue, NotAvailable>| match r {
                Ok(v) => short(v.value),
                Err(e) => na_short(e),
            };
            for s in slots {
                let before = ar.storage_at(address, s, from);
                let after = ar.storage_at(address, s, to);
                let named = layout
                    .as_ref()
                    .map(|l| (l, l.describe_slot(s, 4096)))
                    .filter(|(_, names)| !names.is_empty());
                let Some((l, names)) = named else {
                    println!("[raw] {s}  {} -> {}", fmt(&before), fmt(&after));
                    continue;
                };
                for (name, loc) in names {
                    let dec = |r: &std::result::Result<StorageValue, NotAvailable>| match r {
                        Ok(v) => l.decode(&loc, v.value).to_string(),
                        Err(e) => na_short(e),
                    };
                    println!("{name:<32} {} -> {}", dec(&before), dec(&after));
                }
            }
        }
        Cmd::Typegen { layout, name } => {
            let l = load_layout(&layout)?;
            let name = name.unwrap_or_else(|| {
                let stem = layout
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("Contract");
                let stem = stem.split('.').next().unwrap_or(stem);
                let mut c = stem.chars();
                let cap: String = c
                    .next()
                    .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                    .unwrap_or_default();
                format!("{cap}View")
            });
            print!("{}", l.typescript(&name));
        }
        Cmd::Verify {
            journal,
            show_matches,
        } => {
            let ar = Archive::open(&cli.data)?;
            let text = std::fs::read_to_string(&journal)?;
            let (mut matched, mut mismatched) = (0usize, 0usize);
            let mut unavailable: BTreeMap<String, usize> = BTreeMap::new();
            for (n, line) in text.lines().enumerate() {
                if line.trim().is_empty() {
                    continue;
                }
                let row: serde_json::Value = serde_json::from_str(line)
                    .with_context(|| format!("journal line {}", n + 1))?;
                let get = |k: &str| {
                    row.get(k)
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| anyhow!("line {}: missing {k}", n + 1))
                };
                let block = row
                    .get("block")
                    .and_then(|v| v.as_u64())
                    .ok_or_else(|| anyhow!("line {}: missing block", n + 1))?;
                let address: Address = get("address")?.parse()?;
                let slot = parse_slot(get("slot")?)?;
                let expected = parse_slot(get("value")?)?;
                let field = row.get("field").and_then(|v| v.as_str()).unwrap_or("");
                match ar.storage_at(address, slot, block) {
                    Ok(v) if v.value == expected => {
                        matched += 1;
                        if show_matches {
                            println!("ok       {block} {field:<28} {}", short(v.value));
                        }
                    }
                    Ok(v) => {
                        mismatched += 1;
                        println!(
                            "MISMATCH {block} {field:<28} archive {} ({} @ {}) expected {}",
                            v.value,
                            prov(v.provenance),
                            v.set_at,
                            expected
                        );
                    }
                    Err(e) => {
                        let key = match &e {
                            NotAvailable::NotWatched(_) => "NotWatched".to_string(),
                            NotAvailable::BeforeStart { .. } => "BeforeStart".into(),
                            NotAvailable::AfterHead { .. } => "AfterHead".into(),
                            NotAvailable::NotSynced => "NotSynced".into(),
                            NotAvailable::InvalidRange { .. } => "InvalidRange".into(),
                            NotAvailable::NotBootstrapped => "NotBootstrapped".into(),
                            NotAvailable::BootstrapPending { .. } => "BootstrapPending".into(),
                            NotAvailable::BootstrapLost { .. } => "BootstrapLost".into(),
                            NotAvailable::Internal(s) => format!("Internal: {s}"),
                        };
                        *unavailable.entry(key).or_default() += 1;
                    }
                }
            }
            let na: usize = unavailable.values().sum();
            println!();
            println!("match:          {matched}");
            println!("mismatch:       {mismatched}");
            println!("not_available:  {na}");
            for (k, v) in &unavailable {
                println!("  {k:<18}{v}");
            }
            if mismatched > 0 {
                std::process::exit(1);
            }
        }
    }
    Ok(())
}
