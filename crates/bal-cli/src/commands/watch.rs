use super::Ctx;
use crate::util::emit;
use alloy_primitives::Address;
use anyhow::Result;
use serde_json::json;

pub fn watch(ctx: &Ctx, address: Address, from: u64) -> Result<()> {
    ctx.open()?.watch(address, from)?;
    if ctx.json {
        emit(&json!({ "watching": address, "from": from }));
    } else {
        println!("watching {address} from block {from}");
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
    if ctx.json {
        emit(&json!({
            "data": ctx.data,
            "head": s.head.map(|(n, h)| json!({ "number": n, "hash": h })),
            "watch": s.watches.iter().map(|(a, f)| json!({ "address": a, "from": f })).collect::<Vec<_>>(),
            "slotRecords": s.slot_records,
            "bootstrap": { "done": s.slots_done, "pending": s.slots_pending, "lost": s.slots_lost },
            "retainedHeaders": s.retained_headers,
            "fileBytes": s.file_bytes,
        }));
        return Ok(());
    }
    println!("data:      {}", ctx.data.display());
    match s.head {
        Some((n, h)) => println!("head:      {n} ({h})"),
        None => println!("head:      (nothing synced yet)"),
    }
    println!("watch:     {} address(es)", s.watches.len());
    for (a, f) in &s.watches {
        println!("  {a}  from {f}");
    }
    println!("records:   {} slot record(s)", s.slot_records);
    println!(
        "bootstrap: {} proven, {} pending, {} lost",
        s.slots_done, s.slots_pending, s.slots_lost
    );
    println!(
        "headers:   {} retained for reorg detection",
        s.retained_headers
    );
    println!("file:      {:.1} MB", s.file_bytes as f64 / 1e6);
    Ok(())
}
