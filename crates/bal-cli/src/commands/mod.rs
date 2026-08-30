//! One module per command. Each takes a [`Ctx`] (archive path, output
//! mode, config) and returns `anyhow::Result`; exit codes for "not
//! available" (2) and "mismatch" (1) are set by the command itself.

pub mod backfill;
pub mod bench_cmd;
pub mod diff;
pub mod get;
pub mod history;
pub mod probe;
pub mod sync;
pub mod typegen;
pub mod verify;
pub mod watch;

use std::path::PathBuf;

/// What every command needs.
pub struct Ctx {
    /// Archive file.
    pub data: PathBuf,
    /// `--json`.
    pub json: bool,
    /// `balq.toml`.
    pub cfg: crate::config::Config,
}

impl Ctx {
    pub fn open(&self) -> anyhow::Result<bal_archive::Archive> {
        Ok(bal_archive::Archive::open(&self.data)?)
    }
}
