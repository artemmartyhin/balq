//! Client for `balq index --serve`: the read half of [`bal_archive::Archive`]
//! over HTTP, with the same result types, so commands do not care which one
//! they got.

use alloy_primitives::{Address, B256};
use bal_archive::{
    ArchiveError, ArchiveStats, HistoryEntry, NotAvailable, Provenance, StorageValue,
};
use serde_json::Value;

pub struct RemoteArchive {
    url: String,
}

fn na_from(v: &Value) -> NotAvailable {
    let e = &v["error"];
    let n = |k: &str| e[k].as_u64().unwrap_or(0);
    match e["code"].as_str().unwrap_or("Internal") {
        "NotWatched" => NotAvailable::NotWatched(
            e["address"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or(Address::ZERO),
        ),
        "BeforeStart" => NotAvailable::BeforeStart {
            requested: n("requested"),
            start: n("start"),
        },
        "AfterHead" => NotAvailable::AfterHead {
            requested: n("requested"),
            head: n("head"),
        },
        "NotSynced" => NotAvailable::NotSynced,
        "InvalidRange" => NotAvailable::InvalidRange {
            start: n("start"),
            end: n("end"),
        },
        "NeverRecorded" => NotAvailable::NeverRecorded,
        "UnknownBefore" => NotAvailable::UnknownBefore {
            first_seen: n("first_seen"),
        },
        _ => NotAvailable::Internal(e["message"].as_str().unwrap_or("remote error").to_string()),
    }
}

fn prov_from(s: &str) -> Provenance {
    match s {
        "proof" => Provenance::Proof,
        "imported" | "IMPORTED-UNVERIFIED" => Provenance::Imported,
        "unverified" | "UNVERIFIED" => Provenance::Unverified,
        _ => Provenance::Bal,
    }
}

fn word(v: &Value) -> B256 {
    v.as_str()
        .and_then(|s| s.parse().ok())
        .unwrap_or(B256::ZERO)
}

impl RemoteArchive {
    pub fn new(url: impl Into<String>) -> Self {
        Self { url: url.into() }
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// `Ok(Ok(json))` for 2xx, `Ok(Err(json))` for a 404 miss, `Err` for
    /// anything else (server gone, 500).
    fn get(
        &self,
        path: &str,
    ) -> std::result::Result<std::result::Result<Value, Value>, ArchiveError> {
        let full = format!("{}/{}", self.url.trim_end_matches('/'), path);
        let fail = |m: String| ArchiveError::Source(bal_source::SourceError::Transport(m));
        match ureq::get(&full).call() {
            Ok(mut r) => {
                let v: Value = r
                    .body_mut()
                    .read_json()
                    .map_err(|e| fail(format!("serve {full}: {e}")))?;
                Ok(Ok(v))
            }
            Err(ureq::Error::StatusCode(404)) => {
                // ureq 3 discards the body of an error status by default;
                // re-request with the body kept.
                let v: Value = ureq::config::Config::builder()
                    .http_status_as_error(false)
                    .build()
                    .new_agent()
                    .get(&full)
                    .call()
                    .map_err(|e| fail(format!("serve {full}: {e}")))?
                    .body_mut()
                    .read_json()
                    .map_err(|e| fail(format!("serve {full}: {e}")))?;
                Ok(Err(v))
            }
            Err(e) => Err(fail(format!("serve {full}: {e}"))),
        }
    }

    pub fn storage_at(
        &self,
        addr: Address,
        slot: B256,
        block: u64,
    ) -> std::result::Result<StorageValue, NotAvailable> {
        match self.get(&format!("storage/{addr}/{slot}/{block}"))? {
            Ok(v) => Ok(StorageValue {
                value: word(&v["value"]),
                provenance: prov_from(v["provenance"].as_str().unwrap_or("bal")),
                set_at: v["setAt"].as_u64().unwrap_or(0),
                index: v["index"].as_u64().unwrap_or(0) as u32,
            }),
            Err(e) => Err(na_from(&e)),
        }
    }

    pub fn history(
        &self,
        addr: Address,
        slot: B256,
        range: std::ops::Range<u64>,
    ) -> std::result::Result<Vec<HistoryEntry>, NotAvailable> {
        match self.get(&format!(
            "history/{addr}/{slot}?from={}&to={}",
            range.start, range.end
        ))? {
            Ok(v) => Ok(v
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|e| HistoryEntry {
                            block: e["block"].as_u64().unwrap_or(0),
                            index: e["index"].as_u64().unwrap_or(0) as u32,
                            value: word(&e["value"]),
                            provenance: prov_from(e["provenance"].as_str().unwrap_or("bal")),
                        })
                        .collect()
                })
                .unwrap_or_default()),
            Err(e) => Err(na_from(&e)),
        }
    }

    pub fn changed_slots(
        &self,
        addr: Address,
        block: u64,
    ) -> std::result::Result<Vec<B256>, NotAvailable> {
        match self.get(&format!("changed/{addr}/{block}"))? {
            Ok(v) => Ok(v
                .as_array()
                .map(|a| a.iter().map(word).collect())
                .unwrap_or_default()),
            Err(e) => Err(na_from(&e)),
        }
    }

    pub fn stats(&self) -> std::result::Result<ArchiveStats, ArchiveError> {
        let v = self
            .get("status")?
            .map_err(|e| ArchiveError::Corrupt(Box::leak(e.to_string().into_boxed_str())))?;
        let n = |k: &str| v[k].as_u64().unwrap_or(0);
        let pairs = |k: &str| -> Vec<(Address, u64)> {
            v[k].as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|w| {
                            Some((
                                w["address"].as_str()?.parse().ok()?,
                                w["from"].as_u64().or(w["block"].as_u64())?,
                            ))
                        })
                        .collect()
                })
                .unwrap_or_default()
        };
        Ok(ArchiveStats {
            head: v["head"]
                .as_object()
                .map(|h| (h["number"].as_u64().unwrap_or(0), word(&h["hash"]))),
            watches: pairs("watch"),
            created: pairs("created"),
            slot_records: n("slotRecords"),
            slots_done: v["bootstrap"]["done"].as_u64().unwrap_or(0),
            slots_pending: v["bootstrap"]["pending"].as_u64().unwrap_or(0),
            slots_lost: v["bootstrap"]["lost"].as_u64().unwrap_or(0),
            retained_headers: n("retainedHeaders"),
            file_bytes: n("fileBytes"),
        })
    }

    pub fn head(&self) -> std::result::Result<Option<(u64, B256)>, ArchiveError> {
        let v = self
            .get("head")?
            .map_err(|_| ArchiveError::Corrupt("head"))?;
        Ok(v.as_object()
            .map(|h| (h["number"].as_u64().unwrap_or(0), word(&h["hash"]))))
    }
}
