//! `balq index`: the one command. Watch the addresses, catch up to the
//! head, backfill each to its deploy, then follow — with the archive's
//! state and every change shown by variable name.

use super::Ctx;
use crate::commands::sync::report_json;
use crate::ui;
use crate::util::{emit, load_layout, short};
use alloy_primitives::{Address, B256};
use anyhow::{bail, Result};
use bal_archive::{Archive, BackfillOpts, BackfillStop, SyncReport};
use bal_layout::Layout;
use bal_source::{BalSource, Fallback, JsonRpcSource};
use serde_json::json;
use std::path::PathBuf;

pub struct Opts {
    pub addresses: Vec<Address>,
    pub rpc: Option<String>,
    pub layout: Option<PathBuf>,
    pub history: Option<u64>,
    pub no_backfill: bool,
    pub once: bool,
    pub poll: u64,
    pub backup_rpc: Option<String>,
}

/// Blocks per backfill step between progress updates.
const STEP: u64 = 32;
/// Consecutive source failures tolerated by `--once` before giving up;
/// while following there is no limit — the node will be back.
const MAX_FAILURES: u32 = 10;

fn retry_note(ctx: &Ctx, e: &impl std::fmt::Display, poll: u64) {
    if ctx.json {
        emit(&json!({ "error": e.to_string(), "retryInSeconds": poll }));
    } else {
        ui::warn(format!("{e} — retrying in {poll}s"));
    }
}

async fn node_info(src: &JsonRpcSource) -> (Option<String>, Option<u64>) {
    let client = src
        .call("web3_clientVersion", json!([]))
        .await
        .ok()
        .and_then(|v| v.as_str().map(String::from));
    let chain = src
        .call("eth_chainId", json!([]))
        .await
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok());
    (client, chain)
}

fn host(url: &str) -> String {
    url.split("://")
        .nth(1)
        .unwrap_or(url)
        .split('/')
        .next()
        .unwrap_or(url)
        .to_string()
}

pub async fn run(ctx: &Ctx, o: Opts) -> Result<()> {
    let rpc = ctx.cfg.rpc(o.rpc)?;
    let backup = o.backup_rpc.or_else(|| ctx.cfg.backup_rpc.clone());
    let addrs: Vec<Address> = if o.addresses.is_empty() {
        ctx.cfg.watch.clone()
    } else {
        o.addresses
    };
    if addrs.is_empty() {
        bail!("nothing to index: pass an address, or set `watch = [\"0x…\"]` in balq.toml");
    }
    let layout = o
        .layout
        .or_else(|| ctx.cfg.layout.clone())
        .as_deref()
        .map(load_layout)
        .transpose()?;

    let info = JsonRpcSource::new(&rpc);
    let src = Fallback::new(JsonRpcSource::new(&rpc), backup.map(JsonRpcSource::new));
    let head = src.head().await?;
    let ar = ctx.open()?;

    if !ctx.json {
        ui::banner();
        let (client, chain) = node_info(&info).await;
        let mut node = host(&rpc);
        if let Some(c) = client {
            node.push_str(&format!(
                " · {}",
                c.split('/').take(2).collect::<Vec<_>>().join(" ")
            ));
        }
        if let Some(c) = chain {
            node.push_str(&format!(" · chain {c}"));
        }
        node.push_str(&format!(" · head {}", ui::num(head)));
        ui::kv("node", node);
        let size = std::fs::metadata(&ctx.data).map(|m| m.len()).unwrap_or(0);
        ui::kv(
            "archive",
            format!(
                "{} {}",
                ctx.data.display(),
                ui::dim(format!("({:.1} MB)", size as f64 / 1e6))
            ),
        );
        if let Some(l) = &layout {
            ui::kv("layout", format!("{} fields", l.fields().count()));
        }
        println!();
    }

    // Watches: a new address starts at the node's head (a block that
    // exists, so the first sync pass reaches it and backfill can begin), or
    // just above the archive head when the archive is already ahead of it.
    let watched = ar.watchlist()?;
    let new_start = ar.head()?.map(|(h, _)| h + 1).unwrap_or(head).max(1);
    for a in &addrs {
        if let Some((_, from)) = watched.iter().find(|(w, _)| w == a) {
            if !ctx.json {
                let created = ar.created_at(*a)?;
                let state = match created {
                    Some(c) => {
                        ui::green(format!("history complete since deploy at {}", ui::num(c)))
                    }
                    None => ui::dim(format!("history from {}", ui::num(*from))),
                };
                println!("  {}  {}", ui::bold(ui::short_addr(a)), state);
            }
        } else {
            ar.watch(*a, new_start)?;
            if ctx.json {
                emit(&json!({ "watching": a, "from": new_start }));
            } else {
                println!(
                    "  {}  {}",
                    ui::bold(ui::short_addr(a)),
                    ui::dim(format!("new — watching from {}", ui::num(new_start)))
                );
            }
        }
    }
    if !ctx.json {
        println!();
    }

    // Forward first, in steps with progress: backfill needs the archive at
    // (or above) each start.
    let first = ar.head()?.map(|(h, _)| h + 1).unwrap_or(new_start);
    let total = if head >= first { head - first + 1 } else { 0 };
    let pb = (!ctx.json && total > 0).then(|| ui::walk_bar("sync", Some(total)));
    let mut applied = 0u64;
    let mut failures = 0u32;
    loop {
        let rep = match ar.sync_step(&src, None, Some(STEP)).await {
            Ok(r) => r,
            Err(e) => {
                failures += 1;
                if o.once && failures > MAX_FAILURES {
                    return Err(e.into());
                }
                match &pb {
                    Some(pb) => pb.suspend(|| retry_note(ctx, &e, o.poll)),
                    None => retry_note(ctx, &e, o.poll),
                }
                tokio::time::sleep(std::time::Duration::from_secs(o.poll)).await;
                continue;
            }
        };
        failures = 0;
        applied += rep.blocks_applied;
        match &pb {
            Some(pb) => {
                pb.set_position(applied.min(total));
                pb.set_message(ui::num(rep.to.unwrap_or(first)));
                pb.suspend(|| render_pass(ctx, &ar, &rep, layout.as_ref(), &addrs))?;
            }
            None => render_pass(ctx, &ar, &rep, layout.as_ref(), &addrs)?,
        }
        if rep.blocks_applied == 0 || (rep.to.is_some() && rep.to >= rep.source_head) {
            break;
        }
    }
    if let Some(pb) = &pb {
        pb.finish_and_clear();
    }

    // Backward: to the deploy, or `--history` blocks. An address whose
    // start block the node has not produced yet is retried while following.
    let mut pending: Vec<Address> = Vec::new();
    if !o.no_backfill {
        for a in &addrs {
            if !backfill_one(ctx, &ar, &src, *a, o.history, o.poll, o.once).await? {
                pending.push(*a);
            }
        }
    }

    if o.once {
        if !ctx.json {
            println!();
            ui::ok(format!(
                "up to date at block {}",
                ui::num(ar.head()?.map(|(h, _)| h).unwrap_or(0))
            ));
        }
        return Ok(());
    }

    if !ctx.json {
        println!();
        println!(
            "  {}",
            ui::dim(format!("following · poll {}s · Ctrl+C to stop", o.poll))
        );
    }
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(o.poll)).await;
        match ar.sync(&src, None).await {
            Ok(rep) => {
                render_pass(ctx, &ar, &rep, layout.as_ref(), &addrs)?;
                if !pending.is_empty() && rep.blocks_applied > 0 {
                    let mut still = Vec::new();
                    for a in pending.drain(..) {
                        if !backfill_one(ctx, &ar, &src, a, o.history, o.poll, o.once).await? {
                            still.push(a);
                        }
                    }
                    pending = still;
                }
            }
            Err(e) => {
                if ctx.json {
                    emit(&json!({ "error": e.to_string(), "retryInSeconds": o.poll }));
                } else {
                    ui::fail(format!("{e} — retrying in {}s", o.poll));
                }
            }
        }
    }
}

async fn backfill_one<S: BalSource + ?Sized>(
    ctx: &Ctx,
    ar: &Archive,
    src: &S,
    a: Address,
    history: Option<u64>,
    poll: u64,
    once: bool,
) -> Result<bool> {
    if ar.created_at(a)?.is_some() {
        return Ok(true);
    }
    let Some((_, start)) = ar.watchlist()?.into_iter().find(|(w, _)| *w == a) else {
        return Ok(true);
    };
    let archive_head = ar.head()?.map(|(h, _)| h).unwrap_or(0);
    if archive_head < start {
        if !ctx.json {
            ui::warn(format!(
                "{}  node has not produced block {} yet; backfill starts once it lands",
                ui::bold(ui::short_addr(a)),
                ui::num(start)
            ));
        }
        return Ok(false);
    }
    let to = history.map(|h| start.saturating_sub(h).max(1));
    let total = to.map(|t| start.saturating_sub(t));
    let pb = (!ctx.json).then(|| ui::walk_bar("backfill", total));
    let (mut scanned, mut records, mut resolved) = (0u64, 0usize, 0usize);
    let mut failures = 0u32;
    let stopped = loop {
        let rep = match ar
            .backfill(
                src,
                a,
                BackfillOpts {
                    to,
                    max_blocks: Some(STEP),
                    resolve_only: false,
                },
            )
            .await
        {
            Ok(r) => r,
            Err(e) => {
                failures += 1;
                if once && failures > MAX_FAILURES {
                    return Err(e.into());
                }
                match &pb {
                    Some(pb) => pb.suspend(|| retry_note(ctx, &e, poll)),
                    None => retry_note(ctx, &e, poll),
                }
                tokio::time::sleep(std::time::Duration::from_secs(poll)).await;
                continue;
            }
        };
        failures = 0;
        scanned += rep.blocks_scanned;
        records += rep.records_written;
        resolved += rep.slots_resolved;
        if let Some(pb) = &pb {
            pb.set_position(scanned);
            pb.set_message(ui::num(rep.to));
        }
        if rep.stopped != BackfillStop::Budget {
            break rep;
        }
    };
    if let Some(pb) = &pb {
        pb.finish_and_clear();
    }
    if ctx.json {
        emit(&json!({
            "backfill": a, "from": start, "to": stopped.to, "blocksScanned": scanned,
            "recordsWritten": records, "slotsResolved": resolved, "unresolved": stopped.unresolved,
            "createdAt": stopped.created_at,
            "stopped": format!("{:?}", stopped.stopped),
        }));
        return Ok(true);
    }
    let tail = ui::dim(format!(
        "({} blocks, {} records)",
        ui::num(scanned),
        ui::num(records as u64)
    ));
    let who = ui::bold(ui::short_addr(a));
    match stopped.stopped {
        BackfillStop::Creation(c) => ui::ok(format!(
            "{who}  created at {} — history complete {tail}",
            ui::num(c)
        )),
        BackfillStop::Target => ui::ok(format!(
            "{who}  history from {} {tail}{}",
            ui::num(stopped.to),
            if stopped.unresolved > 0 {
                ui::dim(format!(" · {} slot(s) unknown before their first write", stopped.unresolved))
            } else {
                String::new()
            }
        )),
        BackfillStop::Nothing => ui::ok(format!("{who}  nothing to backfill")),
        BackfillStop::PreBal(b) => ui::warn(format!(
            "{who}  block {} has no BAL hash (before the fork); older state needs an archive proof {tail}",
            ui::num(b)
        )),
        BackfillStop::HistoryUnavailable(b) => ui::warn(format!(
            "{who}  node does not serve block {} — history expiry? pass --backup-rpc {tail}",
            ui::num(b)
        )),
        BackfillStop::Resolved | BackfillStop::Budget => {
            ui::ok(format!("{who}  history from {} {tail}", ui::num(stopped.to)))
        }
    }
    Ok(true)
}

/// One line per block with changes (fields by name when a layout is
/// given), empty blocks collapsed into one dim line.
pub fn render_pass(
    ctx: &Ctx,
    ar: &Archive,
    rep: &SyncReport,
    layout: Option<&Layout>,
    addrs: &[Address],
) -> Result<()> {
    if ctx.json {
        if rep.blocks_applied > 0 {
            emit(&report_json(rep));
        }
        return Ok(());
    }
    if let Some(f) = rep.reorged_to {
        ui::warn(format!("reorg: rolled back to block {}", ui::num(f)));
    }
    let (Some(from), Some(to)) = (rep.from, rep.to) else {
        return Ok(());
    };
    let many = addrs.len() > 1;
    let mut quiet: Option<(u64, u64)> = None;
    let flush = |q: &mut Option<(u64, u64)>| {
        if let Some((a, b)) = q.take() {
            let range = if a == b {
                ui::num(a)
            } else {
                format!("{}..{}", ui::num(a), ui::num(b))
            };
            println!("  {}", ui::dim(format!("{range:<15} ·")));
        }
    };
    for b in from..=to {
        let mut lines = Vec::new();
        for a in addrs {
            let slots = match ar.changed_slots(*a, b) {
                Ok(s) => s,
                Err(_) => continue,
            };
            if slots.is_empty() {
                continue;
            }
            let mut parts: Vec<String> = Vec::new();
            for slot in &slots {
                if parts.len() == 4 {
                    parts.push(ui::dim(format!("+{} more", slots.len() - 4)));
                    break;
                }
                parts.push(describe_change(ar, layout, *a, *slot, b));
            }
            let who = if many {
                format!("{} ", ui::short_addr(a))
            } else {
                String::new()
            };
            lines.push(format!(
                "{}{} {}   {}",
                who,
                ui::green("▲"),
                ui::dim(format!("{} record(s)", slots.len())),
                parts.join(ui::dim(", ").as_str())
            ));
        }
        if lines.is_empty() {
            quiet = Some(quiet.map_or((b, b), |(a, _)| (a, b)));
            continue;
        }
        flush(&mut quiet);
        for (i, l) in lines.iter().enumerate() {
            let label = if i == 0 { ui::num(b) } else { String::new() };
            println!("  {label:<15} {l}");
        }
    }
    flush(&mut quiet);
    Ok(())
}

/// `counter 20 → 21`, `[0xc0e2…8cd7] 66428 → 66500`, `? → 5` when the
/// earlier value is not known yet.
fn describe_change(
    ar: &Archive,
    layout: Option<&Layout>,
    a: Address,
    slot: B256,
    b: u64,
) -> String {
    let now = ar.storage_at(a, slot, b).ok();
    let before = if b > 0 {
        ar.storage_at(a, slot, b - 1).ok()
    } else {
        None
    };
    let named = layout.and_then(|l| {
        l.describe_slot(slot, 4096)
            .into_iter()
            .next()
            .map(|n| (l, n))
    });
    let (name, fmt): (String, Box<dyn Fn(B256) -> String>) = match named {
        Some((l, (name, loc))) => (name, Box::new(move |w| l.decode(&loc, w).to_string())),
        None => {
            let s = slot.to_string();
            (
                format!("[{}…{}]", &s[..6], &s[s.len() - 4..]),
                Box::new(short),
            )
        }
    };
    let now = now.map(|v| fmt(v.value)).unwrap_or_else(|| "?".into());
    let before = before.map(|v| fmt(v.value)).unwrap_or_else(|| "?".into());
    format!(
        "{} {} {} {}",
        ui::bold(name),
        ui::dim(before),
        ui::dim("→"),
        now
    )
}
