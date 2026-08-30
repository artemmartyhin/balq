use super::Ctx;
use crate::util::emit;
use anyhow::Result;
use bal_archive::{Archive, ArchiveConfig, SyncReport};
use bal_source::{Fallback, JsonRpcSource, StateSource};
use serde_json::json;

pub struct Opts {
    pub rpc: Option<String>,
    pub prove: bool,
    pub allow_unverified: bool,
    pub follow: bool,
    pub poll: u64,
    pub proof_window: Option<u64>,
    pub backup_rpc: Option<String>,
}

fn report_json(r: &SyncReport) -> serde_json::Value {
    json!({
        "from": r.from, "to": r.to, "blocksApplied": r.blocks_applied,
        "reorgedTo": r.reorged_to, "slotsWritten": r.slots_written,
        "bootstrapped": r.bootstrapped, "bootstrapPending": r.bootstrap_pending,
        "bootstrapLost": r.bootstrap_lost, "unverifiedBlocks": r.unverified_blocks,
    })
}

/// One line per pass that did something.
fn pass_line(r: &SyncReport, prove: bool) -> String {
    let mut s = match (r.from, r.to) {
        (Some(f), Some(t)) if f == t => format!("block {t}: {} record(s)", r.slots_written),
        (Some(f), Some(t)) => format!(
            "blocks {f}..={t}: +{} block(s), {} record(s)",
            r.blocks_applied, r.slots_written
        ),
        _ => "nothing new".into(),
    };
    if let Some(f) = r.reorged_to {
        s.push_str(&format!("  (reorg: rolled back to {f})"));
    }
    if prove {
        s.push_str(&format!(
            "  proofs: {} proven, {} pending, {} lost",
            r.bootstrapped, r.bootstrap_pending, r.bootstrap_lost
        ));
    }
    if r.unverified_blocks > 0 {
        s.push_str(&format!(
            "  WARNING: {} block(s) applied WITHOUT verification",
            r.unverified_blocks
        ));
    }
    s
}

/// Addresses that have slots with an unknown pre-value, for the one-time hint.
fn unknown_pre_values(ar: &Archive) -> Result<Vec<(alloy_primitives::Address, u64)>> {
    let s = ar.stats()?;
    if s.slots_pending + s.slots_lost == 0 {
        return Ok(vec![]);
    }
    Ok(s.watches
        .into_iter()
        .filter(|(a, _)| !s.created.iter().any(|(c, _)| c == a))
        .collect())
}

pub async fn run(ctx: &Ctx, o: Opts) -> Result<()> {
    let rpc = ctx.cfg.rpc(o.rpc)?;
    let backup = o.backup_rpc.or_else(|| ctx.cfg.backup_rpc.clone());
    let proof_window = o
        .proof_window
        .or(ctx.cfg.proof_window)
        .unwrap_or(ArchiveConfig::default().bootstrap_window);
    let ar = Archive::open_with(
        &ctx.data,
        ArchiveConfig {
            allow_unverified: o.allow_unverified,
            bootstrap_window: proof_window,
            ..Default::default()
        },
    )?;
    let src = Fallback::new(JsonRpcSource::new(&rpc), backup.map(JsonRpcSource::new));
    let state: Option<&dyn StateSource> = if o.prove { Some(&src) } else { None };

    let mut hinted = false;
    let mut hint = |ar: &Archive| -> Result<()> {
        if hinted || o.prove || ctx.json {
            return Ok(());
        }
        let addrs = unknown_pre_values(ar)?;
        if addrs.is_empty() {
            return Ok(());
        }
        hinted = true;
        eprintln!(
            "note: some slots have no recorded value before their first write. Their earlier\n      values are in older blocks: `balq backfill <address> --resolve` (or to the\n      contract's creation without --resolve). Affected: {}",
            addrs
                .iter()
                .map(|(a, _)| a.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
        Ok(())
    };

    let mut rep;
    loop {
        rep = match ar.sync(&src, state).await {
            Ok(r) => r,
            Err(e) if o.follow => {
                if ctx.json {
                    emit(&json!({ "error": e.to_string(), "retryInSeconds": o.poll }));
                } else {
                    eprintln!("sync error: {e} — retrying in {}s", o.poll);
                }
                tokio::time::sleep(std::time::Duration::from_secs(o.poll)).await;
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        if !o.follow {
            break;
        }
        if rep.blocks_applied > 0 {
            if ctx.json {
                emit(&report_json(&rep));
            } else {
                println!("{}", pass_line(&rep, o.prove));
            }
            if rep.bootstrap_pending + rep.bootstrap_lost > 0 {
                hint(&ar)?;
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(o.poll)).await;
    }

    if ctx.json {
        emit(&report_json(&rep));
        return Ok(());
    }
    match ar.head()? {
        Some((h, _)) if rep.blocks_applied == 0 => println!("up to date at block {h}"),
        None if rep.blocks_applied == 0 => println!(
            "nothing to apply yet: the node has not reached the first watched block ({})",
            rep.from
                .map(|f| f.to_string())
                .unwrap_or_else(|| "none watched".into())
        ),
        _ => println!("{}", pass_line(&rep, o.prove)),
    }
    hint(&ar)?;
    Ok(())
}
