//! Formatting shared by the commands. Text for humans, JSON for scripts —
//! never a `null` where the archive said "not available".

use alloy_primitives::{B256, U256};
use anyhow::{Context, Result};
use bal_archive::{NotAvailable, Provenance};
use bal_layout::Layout;
use serde_json::{json, Value};
use std::path::Path;

/// Slot or word: `0x`-hex of any length, or a decimal of any size. A bare
/// digit string is always decimal — never guessed as hex.
pub fn parse_slot(s: &str) -> Result<B256> {
    let s = s.trim();
    let u = match s.strip_prefix("0x") {
        Some(h) => U256::from_str_radix(h, 16).with_context(|| format!("bad hex slot {s}"))?,
        None => s
            .parse::<U256>()
            .with_context(|| format!("bad decimal slot {s} (prefix hex with 0x)"))?,
    };
    Ok(B256::from(u.to_be_bytes::<32>()))
}

/// Decimal when it fits comfortably, else the full word.
pub fn short(v: B256) -> String {
    let u = U256::from_be_bytes(v.0);
    if u < U256::from(u128::MAX) {
        format!("{u}")
    } else {
        format!("{v}")
    }
}

pub fn prov(p: Provenance) -> &'static str {
    match p {
        Provenance::Bal => "bal",
        Provenance::Proof => "proof",
        Provenance::Imported => "IMPORTED-UNVERIFIED",
        Provenance::Unverified => "UNVERIFIED",
    }
}

/// Stable machine-readable code for a miss.
pub fn na_code(e: &NotAvailable) -> &'static str {
    match e {
        NotAvailable::NotWatched(_) => "NotWatched",
        NotAvailable::BeforeStart { .. } => "BeforeStart",
        NotAvailable::AfterHead { .. } => "AfterHead",
        NotAvailable::NotSynced => "NotSynced",
        NotAvailable::InvalidRange { .. } => "InvalidRange",
        NotAvailable::NotBootstrapped => "NotBootstrapped",
        NotAvailable::BootstrapPending { .. } => "BootstrapPending",
        NotAvailable::BootstrapLost { .. } => "BootstrapLost",
        NotAvailable::Internal(_) => "Internal",
    }
}

/// Compact tag for a missing value in tabular output; `balq get` prints the full reason.
pub fn na_short(e: &NotAvailable) -> String {
    match e {
        NotAvailable::NotWatched(_) => "<not watched>".into(),
        NotAvailable::BeforeStart { .. } => "<before start>".into(),
        NotAvailable::AfterHead { .. } => "<after head>".into(),
        NotAvailable::NotSynced => "<not synced>".into(),
        NotAvailable::InvalidRange { start, end } => format!("<invalid range {start}..{end}>"),
        NotAvailable::NotBootstrapped => "<unknown: backfill>".into(),
        NotAvailable::BootstrapPending { first_seen }
        | NotAvailable::BootstrapLost { first_seen } => {
            format!("<unknown before @{first_seen}: backfill>")
        }
        NotAvailable::Internal(s) => format!("<internal: {s}>"),
    }
}

/// JSON form of a miss: `{ "error": { "code", "message" } }`.
pub fn na_json(e: &NotAvailable) -> Value {
    json!({ "error": { "code": na_code(e), "message": e.to_string() } })
}

pub fn load_layout(p: &Path) -> Result<Layout> {
    Layout::from_artifact(p).with_context(|| format!("loading layout {}", p.display()))
}

/// Every named field living in a raw slot, decoded from `word` if given.
pub fn named(layout: &Layout, slot: B256, word: Option<B256>) -> Vec<String> {
    layout
        .describe_slot(slot, 4096)
        .into_iter()
        .map(|(name, loc)| match word {
            Some(w) => format!("{name} = {}", layout.decode(&loc, w)),
            None => name,
        })
        .collect()
}

/// Print one JSON document.
pub fn emit(v: &Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
    );
}
