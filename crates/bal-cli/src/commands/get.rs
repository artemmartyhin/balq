use super::Ctx;
use crate::util::{emit, load_layout, na_json, named, parse_slot, prov, short};
use alloy_primitives::Address;
use anyhow::{anyhow, Result};
use bal_archive::NotAvailable;
use bal_source::{Fallback, JsonRpcSource};
use serde_json::json;
use std::path::PathBuf;

pub struct Opts {
    pub address: Address,
    pub slot: Option<String>,
    pub field: Option<String>,
    pub layout: Option<PathBuf>,
    pub block: u64,
    pub rpc: Option<String>,
    pub prove: bool,
    pub backup_rpc: Option<String>,
}

pub async fn run(ctx: &Ctx, o: Opts) -> Result<()> {
    let ar = ctx.open()?;
    let layout = o.layout.as_deref().map(load_layout).transpose()?;
    let (slot, loc) = match (&o.slot, &o.field, &layout) {
        (Some(s), _, _) => (parse_slot(s)?, None),
        (None, Some(f), Some(l)) => {
            let loc = l.locate(f)?;
            (loc.slot, Some(loc))
        }
        _ => return Err(anyhow!("pass --slot, or --field with --layout")),
    };

    let mut res = ar.storage_at(o.address, slot, o.block);
    if matches!(res, Err(NotAvailable::NeverRecorded)) {
        let rpc = match (o.rpc, o.prove) {
            (Some(r), _) => Some(r),
            (None, true) => Some(ctx.cfg.rpc(None)?),
            (None, false) => None,
        };
        if let Some(rpc) = rpc {
            let Some(local) = ar.local() else {
                anyhow::bail!("proving needs the archive file; it is held by another process (reads are served via {})", ar.via().unwrap_or("?"));
            };
            let backup = o.backup_rpc.or_else(|| ctx.cfg.backup_rpc.clone());
            let src = Fallback::new(JsonRpcSource::new(&rpc), backup.map(JsonRpcSource::new));
            local.bootstrap_slot(&src, o.address, slot).await?;
            res = ar.storage_at(o.address, slot, o.block);
        }
    }

    match res {
        Ok(v) => {
            if ctx.json {
                let mut out = json!({
                    "address": o.address, "slot": slot, "block": o.block,
                    "value": v.value, "provenance": prov(v.provenance),
                    "setAt": v.set_at, "index": v.index,
                });
                if let (Some(loc), Some(l)) = (&loc, &layout) {
                    out["field"] = json!(o.field);
                    out["decoded"] =
                        json!(decode_full(&ar, l, loc, o.address, v.value, o.block).to_string());
                } else if let Some(l) = &layout {
                    out["names"] = json!(named(l, slot, Some(v.value), &[]));
                }
                emit(&out);
                return Ok(());
            }
            match (&loc, &layout) {
                (Some(loc), Some(l)) => println!(
                    "{} = {}  (slot {} @ {}, {})",
                    o.field.as_deref().unwrap_or(""),
                    decode_full(&ar, l, loc, o.address, v.value, o.block),
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
                        for n in named(l, slot, Some(v.value), &[]) {
                            println!("  {n}");
                        }
                    }
                }
            }
            Ok(())
        }
        Err(e) => {
            if ctx.json {
                emit(&na_json(&e));
            } else {
                println!("NOT AVAILABLE ({}): {e}", crate::util::na_code(&e));
                match e {
                    NotAvailable::NeverRecorded => println!(
                        "hint: `balq backfill {}` reads older blocks back to the contract's creation; or --prove to prove it at the head",
                        o.address
                    ),
                    NotAvailable::UnknownBefore { .. } => {
                        println!(
                            "hint: `balq backfill {} --resolve` finds it in older blocks",
                            o.address
                        )
                    }
                    NotAvailable::BeforeStart { .. } => println!(
                        "hint: `balq backfill {} --to {}` extends history back to that block",
                        o.address, o.block
                    ),
                    _ => {}
                }
            }
            std::process::exit(2);
        }
    }
}

/// Decode a field; a dynamic `bytes`/`string` longer than 31 bytes lives in
/// further slots, which are read at the same block.
fn decode_full(
    ar: &super::Backend,
    l: &bal_layout::Layout,
    loc: &bal_layout::Location,
    address: Address,
    word: alloy_primitives::B256,
    block: u64,
) -> bal_layout::Value {
    if !l.is_dynamic_bytes(loc) {
        return l.decode(loc, word);
    }
    let chunks: Vec<alloy_primitives::B256> = l
        .bytes_data_slots(loc, word)
        .into_iter()
        .map(|s| ar.storage_at(address, s, block).map(|v| v.value))
        .collect::<std::result::Result<_, _>>()
        .unwrap_or_default();
    l.decode_bytes(loc, word, &chunks)
}
