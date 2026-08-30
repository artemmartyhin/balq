use super::Ctx;
use crate::util::{emit, na_code, parse_slot, prov, short};
use alloy_primitives::Address;
use anyhow::{anyhow, Context, Result};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::Path;

pub fn run(ctx: &Ctx, journal: &Path, show_matches: bool) -> Result<()> {
    let ar = ctx.open()?;
    let text = std::fs::read_to_string(journal)?;
    let (mut matched, mut mismatched) = (0usize, 0usize);
    let mut unavailable: BTreeMap<String, usize> = BTreeMap::new();
    let mut mismatches = Vec::new();
    for (n, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let row: serde_json::Value =
            serde_json::from_str(line).with_context(|| format!("journal line {}", n + 1))?;
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
                if show_matches && !ctx.json {
                    println!("ok       {block} {field:<28} {}", short(v.value));
                }
            }
            Ok(v) => {
                mismatched += 1;
                if ctx.json {
                    mismatches.push(json!({
                        "block": block, "address": address, "slot": slot, "field": field,
                        "archive": v.value, "provenance": prov(v.provenance), "setAt": v.set_at,
                        "expected": expected,
                    }));
                } else {
                    println!(
                        "MISMATCH {block} {field:<28} archive {} ({} @ {}) expected {}",
                        v.value,
                        prov(v.provenance),
                        v.set_at,
                        expected
                    );
                }
            }
            Err(e) => {
                *unavailable.entry(na_code(&e).to_string()).or_default() += 1;
            }
        }
    }
    let na: usize = unavailable.values().sum();
    if ctx.json {
        emit(&json!({
            "match": matched, "mismatch": mismatched, "notAvailable": na,
            "notAvailableByCode": unavailable, "mismatches": mismatches,
        }));
    } else {
        println!();
        println!("match:          {matched}");
        println!("mismatch:       {mismatched}");
        println!("not_available:  {na}");
        for (k, v) in &unavailable {
            println!("  {k:<18}{v}");
        }
    }
    if mismatched > 0 {
        std::process::exit(1);
    }
    Ok(())
}
