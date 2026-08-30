use super::Ctx;
use crate::util::emit;
use alloy_primitives::Address;
use anyhow::Result;
use bal_source::{BalSource, JsonRpcSource};
use serde_json::json;

/// `from` defaults to the node's head + 1: "from now on".
pub async fn watch(
    ctx: &Ctx,
    address: Address,
    from: Option<u64>,
    rpc: Option<String>,
) -> Result<()> {
    let from = match from {
        Some(f) => f,
        None => {
            let rpc = ctx.cfg.rpc(rpc)?;
            JsonRpcSource::new(&rpc).head().await? + 1
        }
    };
    ctx.open()?.watch(address, from)?;
    if ctx.json {
        emit(&json!({ "watching": address, "from": from }));
    } else {
        println!("watching {address} from block {from}");
        println!(
            "next: `balq sync --follow`; for history before {from}: `balq backfill {address}`"
        );
    }
    Ok(())
}

pub fn unwatch(ctx: &Ctx, address: Address) -> Result<()> {
    ctx.open()?.unwatch(address)?;
    if ctx.json {
        emit(&json!({ "unwatched": address }));
    } else {
        println!("unwatched {address}, data removed");
    }
    Ok(())
}

pub fn status(ctx: &Ctx) -> Result<()> {
    let ar = ctx.open()?;
    let s = ar.stats()?;
    let created_at = |a: &Address| s.created.iter().find(|(c, _)| c == a).map(|(_, b)| *b);
    if ctx.json {
        emit(&json!({
            "data": ctx.data,
            "head": s.head.map(|(n, h)| json!({ "number": n, "hash": h })),
            "watch": s.watches.iter().map(|(a, f)| json!({
                "address": a, "from": f, "createdAt": created_at(a),
            })).collect::<Vec<_>>(),
            "slotRecords": s.slot_records,
            "bootstrap": { "done": s.slots_done, "pending": s.slots_pending, "lost": s.slots_lost },
            "retainedHeaders": s.retained_headers,
            "fileBytes": s.file_bytes,
        }));
        return Ok(());
    }
    println!("data:     {}", ctx.data.display());
    match s.head {
        Some((n, h)) => println!("head:     {n} ({h})"),
        None => println!("head:     (nothing synced yet)"),
    }
    println!("watch:    {} address(es)", s.watches.len());
    for (a, f) in &s.watches {
        match created_at(a) {
            Some(c) => println!("  {a}  from {f}  (created at {c}: history complete)"),
            None => println!("  {a}  from {f}"),
        }
    }
    println!("records:  {} slot record(s)", s.slot_records);
    let unknown = s.slots_pending + s.slots_lost;
    if unknown > 0 {
        println!(
            "unknown:  {unknown} slot(s) have no value before their first recorded write — `balq backfill <address> --resolve`"
        );
    } else {
        println!("unknown:  none — every recorded slot has a known earlier value");
    }
    if s.slots_done > 0 {
        println!("settled:  {} slot(s) with a known earlier value", s.slots_done);
    }
    println!(
        "headers:  {} retained for reorg detection",
        s.retained_headers
    );
    println!("file:     {:.1} MB", s.file_bytes as f64 / 1e6);
    Ok(())
}
