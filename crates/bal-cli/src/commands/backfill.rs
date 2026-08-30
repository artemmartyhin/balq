use super::Ctx;
use crate::ui;
use crate::util::emit;
use alloy_primitives::Address;
use anyhow::Result;
use bal_archive::{BackfillOpts, BackfillStop};
use bal_source::{Fallback, JsonRpcSource};
use serde_json::json;

pub struct Opts {
    pub address: Address,
    pub to: Option<u64>,
    pub resolve: bool,
    pub rpc: Option<String>,
    pub backup_rpc: Option<String>,
    pub chunk: u64,
}

fn stop_json(s: BackfillStop) -> serde_json::Value {
    match s {
        BackfillStop::Target => json!({ "kind": "target" }),
        BackfillStop::Creation(b) => json!({ "kind": "creation", "block": b }),
        BackfillStop::Resolved => json!({ "kind": "resolved" }),
        BackfillStop::Budget => json!({ "kind": "budget" }),
        BackfillStop::PreBal(b) => json!({ "kind": "preBal", "block": b }),
        BackfillStop::HistoryUnavailable(b) => {
            json!({ "kind": "historyUnavailable", "block": b })
        }
        BackfillStop::Nothing => json!({ "kind": "nothing" }),
    }
}

pub async fn run(ctx: &Ctx, o: Opts) -> Result<()> {
    let rpc = ctx.cfg.rpc(o.rpc)?;
    let backup = o.backup_rpc.or_else(|| ctx.cfg.backup_rpc.clone());
    let ar = ctx.open_local()?;
    let src = Fallback::new(JsonRpcSource::new(&rpc), backup.map(JsonRpcSource::new));

    let start = ar
        .watchlist()?
        .into_iter()
        .find(|(a, _)| *a == o.address)
        .map(|(_, s)| s)
        .unwrap_or(0);
    let total = o.to.map(|t| start.saturating_sub(t));
    let pb = (!ctx.json && !o.resolve).then(|| ui::walk_bar("backfill", total));
    let (mut scanned, mut records, mut resolved) = (0u64, 0usize, 0usize);
    let last = loop {
        let rep = ar
            .backfill(
                &src,
                o.address,
                BackfillOpts {
                    to: o.to,
                    max_blocks: Some(o.chunk.max(1)),
                    resolve_only: o.resolve,
                },
            )
            .await?;
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
            "address": o.address,
            "from": start, "to": last.to,
            "blocksScanned": scanned,
            "recordsWritten": records,
            "slotsResolved": resolved,
            "unresolved": last.unresolved,
            "createdAt": last.created_at,
            "stopped": stop_json(last.stopped),
        }));
        return Ok(());
    }
    let who = ui::bold(ui::short_addr(o.address));
    let tail = ui::dim(format!(
        "({} blocks read, {} records, {} earlier values found)",
        ui::num(scanned),
        ui::num(records as u64),
        resolved
    ));
    match last.stopped {
        BackfillStop::Creation(c) => ui::ok(format!(
            "{who}  created at {} — history complete, no value is unknown {tail}",
            ui::num(c)
        )),
        BackfillStop::Target => ui::ok(format!(
            "{who}  history now starts at {} {tail}",
            ui::num(last.to)
        )),
        BackfillStop::Resolved => ui::ok(format!(
            "{who}  every unknown earlier value found; history now starts at {} {tail}",
            ui::num(last.to)
        )),
        BackfillStop::Nothing => ui::ok(format!(
            "{who}  nothing to do (already at the target, or the deploy is known)"
        )),
        BackfillStop::PreBal(b) => ui::warn(format!(
            "{who}  block {} has no BAL hash (before the fork) — older state needs an archive proof {tail}",
            ui::num(b)
        )),
        BackfillStop::HistoryUnavailable(b) => ui::warn(format!(
            "{who}  the node does not serve block {} (history expiry?) — pass --backup-rpc with an endpoint that has it {tail}",
            ui::num(b)
        )),
        BackfillStop::Budget => ui::ok(format!("{who}  stopped at {} {tail}", ui::num(last.to))),
    }
    if last.unresolved > 0 && !matches!(last.stopped, BackfillStop::Creation(_)) {
        println!(
            "  {}",
            ui::dim(format!(
                "{} slot(s) still unknown before their first write — `balq backfill {}` without --resolve walks to the deploy",
                last.unresolved, o.address
            ))
        );
    }
    Ok(())
}
