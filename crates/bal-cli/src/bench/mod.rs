//! `balq bench`: numbers, not adjectives.
//!
//! Two modes share one harness:
//!
//! - **live** — take the last `blocks` blocks of a real network, pick the
//!   `top` addresses with the most storage writes in them, and run a catch-up
//!   sync on a fresh archive. Network time (fetching BALs) is measured
//!   separately from engine time (verify + apply) by fetching everything
//!   first into a cached source. Reads are then sampled at random and timed
//!   against `eth_getStorageAt` on the same endpoint for the same samples.
//! - **synthetic** — generate BALs in memory (`accounts` × `slots` per block)
//!   so the engine can be measured without a network at all.
//!
//! Results go to stdout as a Markdown table and, with `--out`, to
//! `results.json` plus SVG bar charts the README embeds.

use alloy_primitives::{keccak256, Address, B256, U256};
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use bal_archive::{Archive, ArchiveConfig};
use bal_codec::{AccountChanges, BlockAccessList, SlotChanges, StorageChange};
use bal_source::{BalSource, Header, JsonRpcSource, SourceError, SourcedBlock};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::Instant;

mod run;
pub use run::{live, synthetic_run};

/// One measured quantity.
#[derive(Debug, Clone, Serialize)]
pub struct Metric {
    /// Short name.
    pub name: String,
    /// Value in `unit`.
    pub value: f64,
    /// Unit label.
    pub unit: String,
    /// How it was obtained.
    pub note: String,
}

/// Everything one bench run produced.
#[derive(Debug, Clone, Serialize)]
pub struct BenchResult {
    /// "live" or "synthetic".
    pub mode: String,
    /// Endpoint, if live.
    pub rpc: Option<String>,
    /// Chain id, if live.
    pub chain_id: Option<u64>,
    /// Block range applied.
    pub blocks: (u64, u64),
    /// Watched addresses.
    pub addresses: usize,
    /// Metrics in display order.
    pub metrics: Vec<Metric>,
}

/// A source that serves blocks from memory, so applying can be timed
/// without the network.
struct Cached {
    blocks: BTreeMap<u64, SourcedBlock>,
}

#[async_trait]
impl BalSource for Cached {
    async fn head(&self) -> bal_source::Result<u64> {
        self.blocks
            .keys()
            .next_back()
            .copied()
            .ok_or(SourceError::BlockNotFound(0))
    }
    async fn finalized(&self) -> bal_source::Result<u64> {
        Ok(self.head().await?.saturating_sub(64))
    }
    async fn block(&self, number: u64) -> bal_source::Result<SourcedBlock> {
        self.blocks
            .get(&number)
            .cloned()
            .ok_or(SourceError::BlockNotFound(number))
    }
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

fn metric(name: &str, value: f64, unit: &str, note: &str) -> Metric {
    Metric {
        name: name.into(),
        value,
        unit: unit.into(),
        note: note.into(),
    }
}

/// Top-`k` addresses by number of storage writes across `blocks`.
fn most_active(blocks: &BTreeMap<u64, SourcedBlock>, k: usize) -> Vec<Address> {
    let mut writes: HashMap<Address, usize> = HashMap::new();
    for b in blocks.values() {
        for a in &b.bal.accounts {
            if !a.storage_changes.is_empty() {
                *writes.entry(a.address).or_default() += a.storage_changes.len();
            }
        }
    }
    let mut v: Vec<(Address, usize)> = writes.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    v.into_iter().take(k).map(|(a, _)| a).collect()
}

/// Markdown table.
pub fn markdown(r: &BenchResult) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "**{}** — blocks {}..={}, {} watched address(es){}\n\n",
        r.mode,
        r.blocks.0,
        r.blocks.1,
        r.addresses,
        r.rpc
            .as_deref()
            .map(|u| format!(", `{u}`"))
            .unwrap_or_default()
    ));
    s.push_str("| metric | value | note |\n|---|---:|---|\n");
    for m in &r.metrics {
        s.push_str(&format!(
            "| {} | {} {} | {} |\n",
            m.name,
            fmt_num(m.value),
            m.unit,
            m.note
        ));
    }
    s
}

fn fmt_num(v: f64) -> String {
    if v >= 1000.0 {
        format!("{v:.0}")
    } else if v >= 10.0 {
        format!("{v:.1}")
    } else {
        format!("{v:.2}")
    }
}

/// Horizontal bar chart as a self-contained SVG. `bars`: (label, value, unit).
/// Log-scaled when the spread exceeds 100×, because "µs vs ms" is the point.
pub fn svg_bars(title: &str, bars: &[(String, f64, String)]) -> String {
    let w = 720.0;
    let row = 34.0;
    let top = 48.0;
    let left = 200.0;
    let right = 120.0;
    let h = top + row * bars.len() as f64 + 20.0;
    let max = bars.iter().map(|b| b.1).fold(0.0f64, f64::max).max(1e-9);
    let min = bars
        .iter()
        .map(|b| b.1)
        .filter(|v| *v > 0.0)
        .fold(f64::INFINITY, f64::min);
    let log = max / min.max(1e-9) > 100.0;
    let scale = |v: f64| -> f64 {
        let usable = w - left - right;
        if log {
            let lo = (min.max(1e-9)).log10() - 0.3;
            let hi = max.log10();
            ((v.max(1e-9).log10() - lo) / (hi - lo)).clamp(0.02, 1.0) * usable
        } else {
            (v / max).clamp(0.02, 1.0) * usable
        }
    };
    let mut s = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}\" height=\"{h}\" viewBox=\"0 0 {w} {h}\" font-family=\"ui-sans-serif, system-ui, sans-serif\" font-size=\"13\">\n\
         <rect width=\"{w}\" height=\"{h}\" fill=\"#ffffff\"/>\n\
         <text x=\"16\" y=\"28\" font-size=\"15\" font-weight=\"600\" fill=\"#111\">{}</text>\n\
         <text x=\"{}\" y=\"28\" font-size=\"11\" fill=\"#666\" text-anchor=\"end\">{}</text>\n",
        esc(title),
        w - 16.0,
        if log { "log scale" } else { "linear scale" }
    );
    for (i, (label, v, unit)) in bars.iter().enumerate() {
        let y = top + row * i as f64;
        let bw = scale(*v);
        let color = if i == 0 { "#2563eb" } else { "#9ca3af" };
        s.push_str(&format!(
            "<text x=\"{}\" y=\"{}\" text-anchor=\"end\" fill=\"#111\">{}</text>\n\
             <rect x=\"{left}\" y=\"{}\" width=\"{bw:.1}\" height=\"20\" rx=\"3\" fill=\"{color}\"/>\n\
             <text x=\"{:.1}\" y=\"{}\" fill=\"#111\">{} {}</text>\n",
            left - 10.0,
            y + 15.0,
            esc(label),
            y,
            left + bw + 8.0,
            y + 15.0,
            fmt_num(*v),
            esc(unit)
        ));
    }
    s.push_str("</svg>\n");
    s
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Write `results.json` and the charts into `out`.
pub fn write_outputs(
    out: &Path,
    live: Option<&BenchResult>,
    synth: Option<&BenchResult>,
) -> Result<Vec<PathBuf>> {
    std::fs::create_dir_all(out)?;
    let mut written = Vec::new();
    let all: Vec<&BenchResult> = live.into_iter().chain(synth).collect();
    let json = out.join("results.json");
    std::fs::write(&json, serde_json::to_string_pretty(&all)?)?;
    written.push(json);

    let get = |r: &BenchResult, name: &str| {
        r.metrics
            .iter()
            .find(|m| m.name == name)
            .map(|m| (m.value, m.unit.clone()))
    };

    if let Some(r) = live {
        // read latency: local vs RPC
        let mut bars = Vec::new();
        if let Some((v, u)) = get(r, "read p50") {
            bars.push(("balq storage_at p50".to_string(), v, u));
        }
        if let Some((v, u)) = get(r, "read p99") {
            bars.push(("balq storage_at p99".to_string(), v, u));
        }
        if let Some((v, _)) = get(r, "rpc read p50") {
            bars.push((
                "eth_getStorageAt over RPC p50".to_string(),
                v * 1000.0,
                "µs".into(),
            ));
        }
        if !bars.is_empty() {
            let p = out.join("read-latency.svg");
            std::fs::write(&p, svg_bars("Historical read: local archive vs RPC", &bars))?;
            written.push(p);
        }
        // per-block cost: fetch vs verify vs apply
        let mut bars = Vec::new();
        if let Some((v, _)) = get(r, "apply") {
            bars.push((
                "verify + apply (engine)".to_string(),
                v * 1000.0,
                "µs".into(),
            ));
        }
        if let Some((v, u)) = get(r, "verify") {
            bars.push(("verify only".to_string(), v, u));
        }
        if let Some((v, _)) = get(r, "fetch") {
            bars.push(("fetch BAL over HTTPS".to_string(), v * 1000.0, "µs".into()));
        }
        if !bars.is_empty() {
            let p = out.join("per-block.svg");
            std::fs::write(&p, svg_bars("Cost per block", &bars))?;
            written.push(p);
        }
    }
    if let Some(r) = synth {
        let mut bars = Vec::new();
        if let Some((v, u)) = get(r, "sync throughput") {
            bars.push((format!("engine throughput ({} addr)", r.addresses), v, u));
        }
        if let Some((v, u)) = get(r, "records written") {
            bars.push(("records written".to_string(), v, u));
        }
        if let Some((v, u)) = get(r, "bytes/record") {
            bars.push(("bytes per record on disk".to_string(), v, u));
        }
        if !bars.is_empty() {
            let p = out.join("synthetic.svg");
            std::fs::write(&p, svg_bars("Synthetic: engine only, no network", &bars))?;
            written.push(p);
        }
    }
    Ok(written)
}
