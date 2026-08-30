//! One module per command. Each takes a [`Ctx`] (archive path, output
//! mode, config) and returns `anyhow::Result`; exit codes for "not
//! available" (2) and "mismatch" (1) are set by the command itself.

pub mod backfill;
pub mod bench_cmd;
pub mod compact;
pub mod diff;
pub mod get;
pub mod history;
pub mod index;
pub mod probe;
pub mod sync;
pub mod typegen;
pub mod verify;
pub mod watch;

use crate::remote::RemoteArchive;
use alloy_primitives::{Address, B256};
use bal_archive::{Archive, ArchiveError, ArchiveStats, HistoryEntry, NotAvailable, StorageValue};
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

/// The archive for a read command: the file itself, or — when another
/// `balq index --serve` holds it — that process over HTTP. Same methods,
/// same result types.
pub enum Backend {
    Local(Archive),
    Remote(RemoteArchive),
}

macro_rules! either {
    ($self:ident, $a:ident => $e:expr) => {
        match $self {
            Backend::Local($a) => $e,
            Backend::Remote($a) => $e,
        }
    };
}

impl Backend {
    pub fn storage_at(
        &self,
        addr: Address,
        slot: B256,
        block: u64,
    ) -> Result<StorageValue, NotAvailable> {
        either!(self, a => a.storage_at(addr, slot, block))
    }
    pub fn history(
        &self,
        addr: Address,
        slot: B256,
        range: std::ops::Range<u64>,
    ) -> Result<Vec<HistoryEntry>, NotAvailable> {
        either!(self, a => a.history(addr, slot, range))
    }
    pub fn changed_slots(&self, addr: Address, block: u64) -> Result<Vec<B256>, NotAvailable> {
        either!(self, a => a.changed_slots(addr, block))
    }
    pub fn stats(&self) -> Result<ArchiveStats, ArchiveError> {
        either!(self, a => a.stats())
    }
    /// The file, when this process holds it.
    pub fn local(&self) -> Option<&Archive> {
        match self {
            Backend::Local(a) => Some(a),
            Backend::Remote(_) => None,
        }
    }
    /// Where reads are served from, for messages.
    pub fn via(&self) -> Option<&str> {
        match self {
            Backend::Local(_) => None,
            Backend::Remote(r) => Some(r.url()),
        }
    }
}

impl Ctx {
    /// The archive file for writing (watch, backfill, index …).
    pub fn open_local(&self) -> anyhow::Result<Archive> {
        Ok(Archive::open(&self.data)?)
    }

    /// The archive for reading: the file, or the running `index --serve`
    /// that holds it (its sidecar `<data>.serve` names the URL). A sidecar
    /// whose server no longer answers is stale and removed.
    pub fn open(&self) -> anyhow::Result<Backend> {
        match Archive::open(&self.data) {
            Ok(a) => Ok(Backend::Local(a)),
            Err(e) if e.to_string().contains("already open") => {
                let side = crate::serve::sidecar(&self.data);
                let Ok(url) = std::fs::read_to_string(&side) else {
                    anyhow::bail!(
                        "{e}\n  another process holds {}; run it with `index --serve` to read from here meanwhile",
                        self.data.display()
                    );
                };
                let remote = RemoteArchive::new(url.trim());
                match remote.head() {
                    Ok(_) => Ok(Backend::Remote(remote)),
                    Err(_) => {
                        let _ = std::fs::remove_file(&side);
                        anyhow::bail!(
                            "{e}\n  {} pointed at a server that no longer answers; removed it",
                            side.display()
                        )
                    }
                }
            }
            Err(e) => Err(e.into()),
        }
    }
}
