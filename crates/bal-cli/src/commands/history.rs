use super::Ctx;
use crate::util::{emit, na_json, parse_slot, prov, short};
use alloy_primitives::Address;
use anyhow::{anyhow, Result};
use serde_json::json;

pub fn run(ctx: &Ctx, address: Address, slot: &str, range: &str) -> Result<()> {
    let ar = ctx.open()?;
    let slot = parse_slot(slot)?;
    let (a, b) = range
        .split_once("..")
        .ok_or_else(|| anyhow!("range must be A..B"))?;
    let r = a.parse::<u64>()?..b.parse::<u64>()?;
    match ar.history(address, slot, r) {
        Ok(h) => {
            if ctx.json {
                emit(&json!(h
                    .iter()
                    .map(|e| json!({ "block": e.block, "index": e.index, "value": e.value, "provenance": prov(e.provenance) }))
                    .collect::<Vec<_>>()));
                return Ok(());
            }
            for e in h {
                println!(
                    "{:>10}  #{:<4} {}  {}",
                    e.block,
                    e.index,
                    short(e.value),
                    prov(e.provenance)
                );
            }
            Ok(())
        }
        Err(e) => {
            if ctx.json {
                emit(&na_json(&e));
            } else {
                println!("NOT AVAILABLE: {e}");
            }
            std::process::exit(2);
        }
    }
}
