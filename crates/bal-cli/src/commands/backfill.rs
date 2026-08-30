use super::Ctx;
use crate::util::emit;
use alloy_primitives::Address;
use anyhow::Result;
use bal_archive::{BackfillOpts, BackfillReport, BackfillStop};
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
        BackfillStop::HistoryUnavailable(b) => json!({ "kind": "historyUnavailable", "block": b }),
        BackfillStop::Nothing => json!({ "kind": "nothing" }),
    }
}

fn stop_text(s: BackfillStop, addr: Address) -> String {
    match s {
        BackfillStop::Target => "reached the target block".into(),
        BackfillStop::Creation(b) => {
            format!("contract created at block {b} — history is complete, no value is unknown")
        }
        BackfillStop::Resolved => "every unknown pre-value has been found".into(),
        BackfillStop::Budget => "budget exhausted".into(),
        BackfillStop::PreBal(b) => format!(
            "block {b} has no BAL hash (before the BAL fork); older state can only be proven against an archive node"
        ),
        BackfillStop::HistoryUnavailable(b) => format!(
            "the node does not serve block {b} (history expiry?); pass --backup-rpc with an endpoint that still has it"
        ),
        BackfillStop::Nothing => format!(
            "nothing to do for {addr} (already at the target, or its creation is already known)"
        ),
    }
}

pub async fn run(ctx: &Ctx, o: Opts) -> Result<()> {
    let rpc = ctx.cfg.rpc(o.rpc)?;
    let backup = o.backup_rpc.or_else(|| ctx.cfg.backup_rpc.clone());
    let ar = ctx.open()?;
    let src = Fallback::new(JsonRpcSource::new(&rpc), backup.map(JsonRpcSource::new));

    let mut total = BackfillReport {
        from: 0,
        to: 0,
        blocks_scanned: 0,
        records_written: 0,
        slots_resolved: 0,
        unresolved: 0,
        created_at: None,
        stopped: BackfillStop::Nothing,
    };
    let mut first = true;
    loop {
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
        if first {
            total.from = rep.from;
            first = false;
        }
        total.to = rep.to;
        total.blocks_scanned += rep.blocks_scanned;
        total.records_written += rep.records_written;
        total.slots_resolved += rep.slots_resolved;
        total.unresolved = rep.unresolved;
        total.created_at = rep.created_at;
        total.stopped = rep.stopped;
        if !ctx.json && rep.stopped == BackfillStop::Budget {
            eprintln!(
                "backfill {}: at block {}, {} block(s) read, {} record(s), {} pre-value(s) found",
                o.address,
                total.to,
                total.blocks_scanned,
                total.records_written,
                total.slots_resolved
            );
        }
        if rep.stopped != BackfillStop::Budget {
            break;
        }
    }

    if ctx.json {
        emit(&json!({
            "address": o.address,
            "from": total.from, "to": total.to,
            "blocksScanned": total.blocks_scanned,
            "recordsWritten": total.records_written,
            "slotsResolved": total.slots_resolved,
            "unresolved": total.unresolved,
            "createdAt": total.created_at,
            "stopped": stop_json(total.stopped),
        }));
        return Ok(());
    }
    if total.to < total.from {
        println!(
            "history of {} now starts at block {} (was {})",
            o.address, total.to, total.from
        );
    }
    println!(
        "{} block(s) read, {} record(s) written, {} pre-value(s) found, {} still unknown",
        total.blocks_scanned, total.records_written, total.slots_resolved, total.unresolved
    );
    println!("{}", stop_text(total.stopped, o.address));
    if total.unresolved > 0 && total.stopped != BackfillStop::Budget {
        println!(
            "hint: `balq backfill {}` without --resolve walks back to the contract's creation",
            o.address
        );
    }
    Ok(())
}
