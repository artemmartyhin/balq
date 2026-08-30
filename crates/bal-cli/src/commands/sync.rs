use super::Ctx;
use crate::commands::index::render_pass;
use crate::ui;
use crate::util::{emit, load_layout};
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

pub fn report_json(r: &SyncReport) -> serde_json::Value {
    json!({
        "from": r.from, "to": r.to, "blocksApplied": r.blocks_applied,
        "reorgedTo": r.reorged_to, "slotsWritten": r.slots_written,
        "bootstrapped": r.bootstrapped, "bootstrapPending": r.bootstrap_pending,
        "bootstrapLost": r.bootstrap_lost, "unverifiedBlocks": r.unverified_blocks,
    })
}

/// One-time note when slots appeared whose earlier value is not recorded.
fn hint(ar: &Archive, o: &Opts, ctx: &Ctx, hinted: &mut bool) -> Result<()> {
    if *hinted || o.prove || ctx.json {
        return Ok(());
    }
    let s = ar.stats()?;
    if s.slots_pending + s.slots_lost == 0 {
        return Ok(());
    }
    let addrs: Vec<String> = s
        .watches
        .iter()
        .filter(|(a, _)| !s.created.iter().any(|(c, _)| c == a))
        .map(|(a, _)| a.to_string())
        .collect();
    if addrs.is_empty() {
        return Ok(());
    }
    *hinted = true;
    ui::warn(format!(
        "{} slot(s) have no recorded value before their first write — `balq index {}` fills history back to the deploy",
        s.slots_pending + s.slots_lost,
        addrs.join(" ")
    ));
    Ok(())
}

fn proofs_line(rep: &SyncReport) {
    println!(
        "  {}",
        ui::dim(format!(
            "proofs: {} proven, {} pending, {} lost",
            rep.bootstrapped, rep.bootstrap_pending, rep.bootstrap_lost
        ))
    );
}

pub async fn run(ctx: &Ctx, o: Opts) -> Result<()> {
    let rpc = ctx.cfg.rpc(o.rpc.clone())?;
    let backup = o.backup_rpc.clone().or_else(|| ctx.cfg.backup_rpc.clone());
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
    let layout = ctx.cfg.layout.as_deref().map(load_layout).transpose()?;
    let addrs: Vec<_> = ar.watchlist()?.into_iter().map(|(a, _)| a).collect();
    let src = Fallback::new(JsonRpcSource::new(&rpc), backup.map(JsonRpcSource::new));
    let state: Option<&dyn StateSource> = if o.prove { Some(&src) } else { None };

    let mut hinted = false;
    let mut rep;
    loop {
        rep = match ar.sync(&src, state).await {
            Ok(r) => r,
            Err(e) if o.follow => {
                if ctx.json {
                    emit(&json!({ "error": e.to_string(), "retryInSeconds": o.poll }));
                } else {
                    ui::fail(format!("{e} — retrying in {}s", o.poll));
                }
                tokio::time::sleep(std::time::Duration::from_secs(o.poll)).await;
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        if !o.follow {
            break;
        }
        render_pass(ctx, &ar, &rep, layout.as_ref(), &addrs)?;
        if rep.bootstrap_pending + rep.bootstrap_lost > 0 {
            hint(&ar, &o, ctx, &mut hinted)?;
        }
        if o.prove && !ctx.json && rep.blocks_applied > 0 {
            proofs_line(&rep);
        }
        tokio::time::sleep(std::time::Duration::from_secs(o.poll)).await;
    }

    if ctx.json {
        emit(&report_json(&rep));
        return Ok(());
    }
    render_pass(ctx, &ar, &rep, layout.as_ref(), &addrs)?;
    match ar.head()? {
        Some((h, _)) if rep.blocks_applied == 0 => {
            ui::ok(format!("up to date at block {}", ui::num(h)))
        }
        Some((h, _)) => ui::ok(format!(
            "synced to block {} · +{} block(s), {} record(s)",
            ui::num(h),
            rep.blocks_applied,
            rep.slots_written
        )),
        None => ui::warn(format!(
            "nothing to apply yet: the node has not reached the first watched block ({})",
            rep.from
                .map(|f| f.to_string())
                .unwrap_or_else(|| "none watched".into())
        )),
    }
    if o.prove {
        proofs_line(&rep);
    }
    if rep.unverified_blocks > 0 {
        ui::fail(format!(
            "{} block(s) applied WITHOUT verification",
            rep.unverified_blocks
        ));
    }
    hint(&ar, &o, ctx, &mut hinted)?;
    Ok(())
}
