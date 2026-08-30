//! `balq.toml`: defaults for the things you type on every invocation.
//!
//! ```toml
//! rpc = "http://127.0.0.1:8545"
//! backup_rpc = "https://archive.example"   # optional
//! proof_window = 128                        # see `balq probe`
//! data = "/var/lib/balq/balq.redb"
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

    /// The endpoint to use: the flag, else the file, else an error that
    /// says where to put it.
    pub fn rpc(&self, flag: Option<String>) -> Result<String> {
        flag.or_else(|| self.rpc.clone())
            .context("no RPC endpoint: pass --rpc <url> or set `rpc` in balq.toml")
    }
}
