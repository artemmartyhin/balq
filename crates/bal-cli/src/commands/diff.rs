use super::Ctx;
use crate::util::{emit, load_layout, na_code, na_short, short};
use alloy_primitives::Address;
use anyhow::{anyhow, Result};
use bal_archive::{NotAvailable, StorageValue};
use serde_json::{json, Value};
use std::path::PathBuf;

type Read = std::result::Result<StorageValue, NotAvailable>;

fn side_json(r: &Read, decoded: Option<String>) -> Value {
    match r {
        Ok(v) => match decoded {
            Some(d) => json!({ "value": v.value, "decoded": d, "setAt": v.set_at }),
            None => json!({ "value": v.value, "setAt": v.set_at }),
        },
        Err(e) => json!({ "error": na_code(e) }),
    }
}

pub fn run(ctx: &Ctx, address: Address, from: u64, to: u64, layout: Option<PathBuf>) -> Result<()> {
    let ar = ctx.open()?;
    if to <= from {
        return Err(anyhow!("--to must be > --from"));
    }
    let layout = layout.as_deref().map(load_layout).transpose()?;
    let mut slots = std::collections::BTreeSet::new();
    for b in from + 1..=to {
        for s in ar.changed_slots(address, b).map_err(|e| anyhow!("{e}"))? {
            slots.insert(s);
        }
    }
    let fmt = |r: &Read| match r {
        Ok(v) => short(v.value),
        Err(e) => na_short(e),
    };
    let mut rows = Vec::new();
    for s in slots {
        let before = ar.storage_at(address, s, from);
        let after = ar.storage_at(address, s, to);
        let named = layout
            .as_ref()
            .map(|l| (l, l.describe_slot(s, 4096)))
            .filter(|(_, names)| !names.is_empty());
        let Some((l, names)) = named else {
            if ctx.json {
                rows.push(json!({ "slot": s, "before": side_json(&before, None), "after": side_json(&after, None) }));
            } else {
                println!("[raw] {s}  {} -> {}", fmt(&before), fmt(&after));
            }
            continue;
        };
        for (name, loc) in names {
            let dec = |r: &Read| match r {
                Ok(v) => Some(l.decode(&loc, v.value).to_string()),
                Err(_) => None,
            };
            if ctx.json {
                rows.push(json!({
                    "slot": s, "field": name,
                    "before": side_json(&before, dec(&before)),
                    "after": side_json(&after, dec(&after)),
                }));
            } else {
                let show = |r: &Read| {
                    dec(r).unwrap_or_else(|| {
                        na_short(r.as_ref().err().map_or(&NotAvailable::NotSynced, |e| e))
                    })
                };
                println!("{name:<32} {} -> {}", show(&before), show(&after));
            }
        }
    }
    if ctx.json {
        emit(&json!({ "address": address, "from": from, "to": to, "changes": rows }));
    }
    Ok(())
}
