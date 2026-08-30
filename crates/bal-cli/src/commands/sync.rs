use super::Ctx;
use crate::util::emit;
use anyhow::Result;
use bal_archive::{Archive, ArchiveConfig, SyncReport};
use bal_source::{Fallback, JsonRpcSource, StateSource};
use serde_json::json;

pub struct Opts {
    pub rpc: Option<String>,
    pub no_bootstrap: bool,
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
    let state: Option<&dyn StateSource> = if o.no_bootstrap { None } else { Some(&src) };

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
                println!(
                    "{:?}..={:?}: {} block(s), {} record(s), bootstrap +{} proven / {} pending / {} lost{}",
                    rep.from,
                    rep.to,
                    rep.blocks_applied,
                    rep.slots_written,
                    rep.bootstrapped,
                    rep.bootstrap_pending,
                    rep.bootstrap_lost,
                    rep.reorged_to.map(|f| format!(" — REORG to {f}")).unwrap_or_default()
                );
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(o.poll)).await;
    }

    if ctx.json {
        emit(&report_json(&rep));
        return Ok(());
    }
    println!(
        "applied {} block(s) {:?}..={:?}, {} slot record(s)",
        rep.blocks_applied, rep.from, rep.to, rep.slots_written
    );
    if let Some(f) = rep.reorged_to {
        println!("reorg: rolled back to {f}");
    }
    println!(
        "bootstrap: {} proven, {} pending, {} lost",
        rep.bootstrapped, rep.bootstrap_pending, rep.bootstrap_lost
    );
    if rep.unverified_blocks > 0 {
        println!(
            "WARNING: {} block(s) applied WITHOUT verification",
            rep.unverified_blocks
        );
    }
    Ok(())
}
