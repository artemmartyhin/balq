//! `balq.toml`: defaults for the things you type on every invocation.
//!
//! ```toml
//! rpc = "http://127.0.0.1:8545"
//! backup_rpc = "https://old-blocks.example"   # optional: blocks the primary no longer serves
//! data = "/var/lib/balq/balq.redb"
//! watch = ["0x3582…53ce", "0xabcd…"]          # what `balq index` indexes
//! layout = "out/Playground.sol/Playground.json"   # default layout for field names; "auto" = from Sourcify
//! proof_window = 128                          # only with `sync --prove`
//! sourcify = "https://sourcify.dev/server"    # for `auto`; a self-hosted Sourcify works too
//! etherscan_api_key = "…"                     # for `auto` on contracts verified only on Etherscan
//!                                             #   (or ETHERSCAN_API_KEY in the environment)
//!
//! [layouts]                                   # per-address layouts (a protocol of several contracts)
//! "0xabcd…" = "out/Vault.sol/Vault.json"
//! "0x1c7d…" = "auto"
//! ```
//!
//! Flags always win over the file. Looked up at `--config`, else
//! `./balq.toml` if it exists.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Contents of `balq.toml`; every field optional.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Primary JSON-RPC endpoint.
    pub rpc: Option<String>,
    /// Archive endpoint for BAL bodies and proofs the primary cannot serve.
    pub backup_rpc: Option<String>,
    /// Blocks behind head the primary still serves `eth_getProof`.
    pub proof_window: Option<u64>,
    /// Archive file.
    pub data: Option<PathBuf>,
    /// Addresses `balq index` indexes when none are given on the command line.
    #[serde(default)]
    pub watch: Vec<alloy_primitives::Address>,
    /// Storage layout used to name fields in `index` / `sync --follow` output
    /// for every address that has no entry in `layouts`.
    pub layout: Option<PathBuf>,
    /// Per-address layouts: `[layouts]` then `"0x…" = "path.json"` (or `"auto"`).
    #[serde(default)]
    pub layouts: std::collections::HashMap<alloy_primitives::Address, PathBuf>,
    /// Sourcify server for `auto` layouts (default the public one).
    pub sourcify: Option<String>,
    /// Etherscan API key handed to Sourcify's importer for `auto` layouts of
    /// contracts verified only on Etherscan.
    pub etherscan_api_key: Option<String>,
}

impl Config {
    /// Load `path`, or `./balq.toml` if present, or defaults.
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let path = match path {
            Some(p) => p.to_path_buf(),
            None => {
                let p = PathBuf::from("balq.toml");
                if !p.exists() {
                    return Ok(Self::default());
                }
                p
            }
        };
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    /// Etherscan key: the file, else `ETHERSCAN_API_KEY`.
    pub fn etherscan_key(&self) -> Option<String> {
        self.etherscan_api_key
            .clone()
            .or_else(|| std::env::var("ETHERSCAN_API_KEY").ok())
            .filter(|k| !k.is_empty())
    }

    /// The endpoint to use: the flag, else the file, else an error that
    /// says where to put it.
    pub fn rpc(&self, flag: Option<String>) -> Result<String> {
        flag.or_else(|| self.rpc.clone())
            .context("no RPC endpoint: pass --rpc <url> or set `rpc` in balq.toml")
    }
}
