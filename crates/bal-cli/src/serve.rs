//! `balq index --serve`: answer reads over HTTP while the process holds the
//! archive, so other `balq` invocations can read despite the single-process
//! file. Localhost, JSON, no auth — it exposes nothing the file does not.
//!
//! Routes (all `GET`):
//!
//! ```text
//!   /status                                  → ArchiveStats
//!   /watchlist                               → [{address, from, createdAt}]
//!   /head                                    → {number, hash} | null
//!   /storage/{addr}/{slot}/{block}           → StorageValue | 404 {error}
//!   /history/{addr}/{slot}?from=A&to=B       → [HistoryEntry] | 404 {error}
//!   /changed/{addr}/{block}                  → [slot] | 404 {error}
//! ```
//!
//! A sidecar file `<archive>.serve` holds the URL; `Ctx::open` reads it
//! when the file is locked.

use crate::util::{na_json_full, prov};
use alloy_primitives::{Address, B256, U256};
use bal_archive::Archive;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tiny_http::{Header, Response, Server};

/// Path of the sidecar that advertises a running server for `data`.
pub fn sidecar(data: &Path) -> PathBuf {
    let mut p = data.as_os_str().to_owned();
    p.push(".serve");
    PathBuf::from(p)
}

/// Removes the sidecar when dropped (normal exit); a killed process leaves
/// it behind, and the client treats a refused connection as stale.
pub struct Advertised(PathBuf);

impl Drop for Advertised {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Bind `listen`, write the sidecar, and answer on a background thread.
pub fn start(ar: Arc<Archive>, data: &Path, listen: &str) -> anyhow::Result<(String, Advertised)> {
    let server = Server::http(listen).map_err(|e| anyhow::anyhow!("serve on {listen}: {e}"))?;
    let url = format!("http://{}", server.server_addr());
    let side = sidecar(data);
    std::fs::write(&side, &url)?;
    std::thread::Builder::new()
        .name("balq-serve".into())
        .spawn(move || {
            for req in server.incoming_requests() {
                let (status, body) = answer(&ar, req.url());
                let mut resp = Response::from_string(body.to_string()).with_status_code(status);
                if let Ok(h) = Header::from_bytes("Content-Type", "application/json") {
                    resp = resp.with_header(h);
                }
                let _ = req.respond(resp);
            }
        })?;
    Ok((url, Advertised(side)))
}

fn parse_word(s: &str) -> Option<B256> {
    let u = match s.strip_prefix("0x") {
        Some(h) => U256::from_str_radix(h, 16).ok()?,
        None => s.parse::<U256>().ok()?,
    };
    Some(B256::from(u.to_be_bytes::<32>()))
}

fn answer(ar: &Archive, url: &str) -> (u16, Value) {
    let (path, query) = url.split_once('?').unwrap_or((url, ""));
    let q = |k: &str| -> Option<String> {
        query.split('&').find_map(|kv| {
            kv.split_once('=')
                .filter(|(a, _)| *a == k)
                .map(|(_, v)| v.to_string())
        })
    };
    let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
    let bad = |m: &str| {
        (
            400u16,
            json!({ "error": { "code": "BadRequest", "message": m } }),
        )
    };
    let internal = |e: &dyn std::fmt::Display| {
        (
            500u16,
            json!({ "error": { "code": "Internal", "message": e.to_string() } }),
        )
    };
    match parts.as_slice() {
        ["status"] => match ar.stats() {
            Ok(s) => (200, crate::util::stats_json(&s)),
            Err(e) => internal(&e),
        },
        ["watchlist"] => match ar.watchlist() {
            Ok(w) => (
                200,
                json!(w
                    .iter()
                    .map(|(a, f)| json!({ "address": a, "from": f, "createdAt": ar.created_at(*a).ok().flatten() }))
                    .collect::<Vec<_>>()),
            ),
            Err(e) => internal(&e),
        },
        ["head"] => match ar.head() {
            Ok(h) => (200, h.map(|(n, h)| json!({ "number": n, "hash": h })).unwrap_or(Value::Null)),
            Err(e) => internal(&e),
        },
        ["storage", a, s, b] => {
            let (Ok(a), Some(s), Ok(b)) = (a.parse::<Address>(), parse_word(s), b.parse::<u64>()) else {
                return bad("storage/{address}/{slot}/{block}");
            };
            match ar.storage_at(a, s, b) {
                Ok(v) => (
                    200,
                    json!({ "value": v.value, "provenance": prov(v.provenance), "setAt": v.set_at, "index": v.index }),
                ),
                Err(e) => (404, na_json_full(&e)),
            }
        }
        ["history", a, s] => {
            let (Ok(a), Some(s), Some(from), Some(to)) = (
                a.parse::<Address>(),
                parse_word(s),
                q("from").and_then(|v| v.parse::<u64>().ok()),
                q("to").and_then(|v| v.parse::<u64>().ok()),
            ) else {
                return bad("history/{address}/{slot}?from=A&to=B");
            };
            match ar.history(a, s, from..to) {
                Ok(h) => (
                    200,
                    json!(h
                        .iter()
                        .map(|e| json!({ "block": e.block, "index": e.index, "value": e.value, "provenance": prov(e.provenance) }))
                        .collect::<Vec<_>>()),
                ),
                Err(e) => (404, na_json_full(&e)),
            }
        }
        ["changed", a, b] => {
            let (Ok(a), Ok(b)) = (a.parse::<Address>(), b.parse::<u64>()) else {
                return bad("changed/{address}/{block}");
            };
            match ar.changed_slots(a, b) {
                Ok(s) => (200, json!(s)),
                Err(e) => (404, na_json_full(&e)),
            }
        }
        _ => (404, json!({ "error": { "code": "NoRoute", "message": "unknown route" } })),
    }
}
