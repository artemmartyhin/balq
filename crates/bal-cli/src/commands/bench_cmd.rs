use super::Ctx;
use crate::bench;
use crate::util::emit;
use anyhow::{Context, Result};
use std::path::PathBuf;

pub struct Opts {
    pub rpc: Option<String>,
    pub live: bool,
    pub blocks: u64,
    pub top: usize,
    pub samples: usize,
    pub synthetic_blocks: u64,
    pub synthetic_accounts: usize,
    pub synthetic_slots: usize,
    pub out: Option<PathBuf>,
}

pub async fn run(ctx: &Ctx, o: Opts) -> Result<()> {
    // A private, unpredictable directory: the bench deletes and rewrites
    // its archive file, and must never do that to a path another user
    // could have planted.
    let tmp_dir = tempfile::tempdir().context("creating a temp dir")?;
    let tmp = tmp_dir.path().to_path_buf();
    let rpc = match (o.rpc, o.live) {
        (Some(r), _) => Some(r),
        (None, true) => Some(ctx.cfg.rpc(None)?),
        (None, false) => None,
    };
    let live = match rpc {
        Some(url) => {
            eprintln!("live: fetching {} blocks from {url} …", o.blocks);
            Some(bench::live(&url, o.blocks, o.top, o.samples, &tmp).await?)
        }
        None => None,
    };
    let synth = if o.synthetic_blocks > 0 {
        eprintln!(
            "synthetic: {} blocks × {} accounts × {} slots …",
            o.synthetic_blocks, o.synthetic_accounts, o.synthetic_slots
        );
        Some(
            bench::synthetic_run(
                o.synthetic_blocks,
                o.synthetic_accounts,
                o.synthetic_slots,
                o.samples,
                &tmp,
            )
            .await?,
        )
    } else {
        None
    };
    if ctx.json {
        let all: Vec<&bench::BenchResult> = live.iter().chain(synth.iter()).collect();
        emit(&serde_json::to_value(&all)?);
    } else {
        for r in live.iter().chain(synth.iter()) {
            println!("{}", bench::markdown(r));
        }
    }
    if let Some(dir) = o.out {
        for p in bench::write_outputs(&dir, live.as_ref(), synth.as_ref())? {
            eprintln!("wrote {}", p.display());
        }
    }
    Ok(())
}
