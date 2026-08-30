//! `balq` — glue only. Every command calls into a crate below; nothing is
//! decided here. This file parses arguments and dispatches; the commands
//! live in `commands/`, shared formatting in `util.rs`, `balq.toml` in
//! `config.rs`.

mod bench;
mod commands;
mod config;
mod util;

use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};
use std::path::PathBuf;

/// Local, verified archive of contract storage built from EIP-7928 BALs.
#[derive(Parser)]
#[command(name = "balq", version, about)]
struct Cli {
    /// Archive file (default: `data` from balq.toml, else ./balq.redb).
    #[arg(long, global = true)]
    data: Option<PathBuf>,
    /// Config file (default: ./balq.toml if present).
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// Machine-readable output (one JSON document, or one per line for streams).
    #[arg(long, global = true)]
    json: bool,
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Day 0: does this node serve BALs, for old blocks too, and proofs?
    Probe {
        /// JSON-RPC endpoint (or `rpc` from balq.toml).
        #[arg(long)]
        rpc: Option<String>,
        /// How many blocks back the "old" block is.
        #[arg(long, default_value_t = 50_000)]
        age: u64,
    },
    /// Start accumulating an address from a block (must be > current head).
    Watch {
        address: alloy_primitives::Address,
        #[arg(long)]
        from: u64,
    },
    /// Stop watching and delete the address's data.
    Unwatch { address: alloy_primitives::Address },
    /// Head, watchlist, bootstrap counts, file size.
    Status,
    /// Pull blocks from the node, verify, apply, bootstrap.
    Sync {
        /// JSON-RPC endpoint (or `rpc` from balq.toml).
        #[arg(long)]
        rpc: Option<String>,
        /// Skip eth_getProof bootstrap (slots stay pending; window still ticks).
        #[arg(long)]
        no_bootstrap: bool,
        /// Apply blocks whose header has no BAL hash. Debug only.
        #[arg(long)]
        allow_unverified: bool,
        /// Keep running: poll for new blocks and apply them as they arrive.
        /// This is the mode that keeps early bootstrap inside the node's
        /// proof window; one-shot sync after a pause loses pre-values.
        #[arg(long)]
        follow: bool,
        /// Poll interval in seconds for --follow.
        #[arg(long, default_value_t = 4)]
        poll: u64,
        /// How many blocks back the node still serves eth_getProof
        /// (or `proof_window` from balq.toml; default 120).
        #[arg(long)]
        proof_window: Option<u64>,
        /// Second endpoint (any archive provider) asked only when --rpc lacks a
        /// block's BAL or cannot prove a slot. Verified exactly like --rpc.
        #[arg(long)]
        backup_rpc: Option<String>,
    },
    /// Read one slot (or one named field with --layout) at one block.
    Get {
        address: alloy_primitives::Address,
        /// Raw slot: decimal or 0x-hex.
        #[arg(long, conflicts_with = "field")]
        slot: Option<String>,
        /// Field path, e.g. `totals.index`, `balances[0xabc…]`, `items[2]`. Needs --layout.
        #[arg(long, requires = "layout")]
        field: Option<String>,
        /// solc storageLayout JSON or a forge/hardhat artifact containing one.
        #[arg(long)]
        layout: Option<PathBuf>,
        #[arg(long)]
        block: u64,
        /// If the slot was never bootstrapped, prove it now via this node
        /// (or `rpc` from balq.toml when --prove is given).
        #[arg(long)]
        rpc: Option<String>,
        /// Prove a never-bootstrapped slot using the config's rpc.
        #[arg(long)]
        prove: bool,
        /// Backup endpoint for the proof, tried if the primary cannot serve it.
        #[arg(long)]
        backup_rpc: Option<String>,
    },
    /// All changes of one slot in a block range `A..B` (half-open).
    History {
        address: alloy_primitives::Address,
        #[arg(long)]
        slot: String,
        #[arg(long)]
        range: String,
    },
    /// Storage diff of an address between two blocks (values at `from` vs at `to`).
    Diff {
        address: alloy_primitives::Address,
        #[arg(long)]
        from: u64,
        #[arg(long)]
        to: u64,
        /// Name slots via a storage layout. Unresolvable slots stay `[raw]`.
        #[arg(long)]
        layout: Option<PathBuf>,
    },
    /// Compare the archive against a journal of known-true rows
    /// (`{"block","address","slot","value"}` per line, as produced by testbed/poke.mjs).
    Verify {
        #[arg(long)]
        journal: PathBuf,
        /// Also print every matching row.
        #[arg(long)]
        show_matches: bool,
    },
    /// Measure: catch-up sync of the last N blocks for the most active
    /// addresses (live), or an in-memory chain (synthetic). Emits a Markdown
    /// table and, with --out, results.json + SVG charts.
    Bench {
        /// Live mode: JSON-RPC endpoint (or `rpc` from balq.toml with --live).
        #[arg(long)]
        rpc: Option<String>,
        /// Use the config's rpc for live mode.
        #[arg(long)]
        live: bool,
        /// Live: how many recent blocks to replay.
        #[arg(long, default_value_t = 300)]
        blocks: u64,
        /// Live: watch the N addresses with the most storage writes.
        #[arg(long, default_value_t = 20)]
        top: usize,
        /// Random reads to time.
        #[arg(long, default_value_t = 5000)]
        samples: usize,
        /// Synthetic: blocks to generate (0 = skip synthetic).
        #[arg(long, default_value_t = 500)]
        synthetic_blocks: u64,
        /// Synthetic: accounts per block.
        #[arg(long, default_value_t = 100)]
        synthetic_accounts: usize,
        /// Synthetic: changed slots per account per block.
        #[arg(long, default_value_t = 20)]
        synthetic_slots: usize,
        /// Directory for results.json and SVG charts.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Emit a TypeScript interface for a storage layout, matching what
    /// `archive.view(addr, layout).at(block)` exposes in @balq/node.
    Typegen {
        /// solc storageLayout JSON or a forge/hardhat artifact containing one.
        layout: PathBuf,
        /// Interface name (default: file stem + "View").
        #[arg(long)]
        name: Option<String>,
    },
    /// Shell completions: `balq completions bash > /etc/bash_completion.d/balq`.
    Completions { shell: clap_complete::Shell },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let level = match cli.verbose {
        0 => "warn",
        1 => "info",
        _ => "debug",
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| level.into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let cfg = config::Config::load(cli.config.as_deref())?;
    let ctx = commands::Ctx {
        data: cli
            .data
            .clone()
            .or_else(|| cfg.data.clone())
            .unwrap_or_else(|| "balq.redb".into()),
        json: cli.json,
        cfg,
    };

    match cli.cmd {
        Cmd::Probe { rpc, age } => commands::probe::run(&ctx, rpc, age).await,
        Cmd::Watch { address, from } => commands::watch::watch(&ctx, address, from),
        Cmd::Unwatch { address } => commands::watch::unwatch(&ctx, address),
        Cmd::Status => commands::watch::status(&ctx),
        Cmd::Sync {
            rpc,
            no_bootstrap,
            allow_unverified,
            follow,
            poll,
            proof_window,
            backup_rpc,
        } => {
            commands::sync::run(
                &ctx,
                commands::sync::Opts {
                    rpc,
                    no_bootstrap,
                    allow_unverified,
                    follow,
                    poll,
                    proof_window,
                    backup_rpc,
                },
            )
            .await
        }
        Cmd::Get {
            address,
            slot,
            field,
            layout,
            block,
            rpc,
            prove,
            backup_rpc,
        } => {
            commands::get::run(
                &ctx,
                commands::get::Opts {
                    address,
                    slot,
                    field,
                    layout,
                    block,
                    rpc,
                    prove,
                    backup_rpc,
                },
            )
            .await
        }
        Cmd::History {
            address,
            slot,
            range,
        } => commands::history::run(&ctx, address, &slot, &range),
        Cmd::Diff {
            address,
            from,
            to,
            layout,
        } => commands::diff::run(&ctx, address, from, to, layout),
        Cmd::Verify {
            journal,
            show_matches,
        } => commands::verify::run(&ctx, &journal, show_matches),
        Cmd::Bench {
            rpc,
            live,
            blocks,
            top,
            samples,
            synthetic_blocks,
            synthetic_accounts,
            synthetic_slots,
            out,
        } => {
            commands::bench_cmd::run(
                &ctx,
                commands::bench_cmd::Opts {
                    rpc,
                    live,
                    blocks,
                    top,
                    samples,
                    synthetic_blocks,
                    synthetic_accounts,
                    synthetic_slots,
                    out,
                },
            )
            .await
        }
        Cmd::Typegen { layout, name } => commands::typegen::run(&layout, name),
        Cmd::Completions { shell } => {
            clap_complete::generate(shell, &mut Cli::command(), "balq", &mut std::io::stdout());
            Ok(())
        }
    }
}
