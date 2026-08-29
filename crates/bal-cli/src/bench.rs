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

/// Deterministic synthetic chain: every block touches `accounts` addresses,
/// `slots` slots each, with values derived from the block number.
fn synthetic(
    blocks: u64,
    accounts: usize,
    slots: usize,
) -> (BTreeMap<u64, SourcedBlock>, Vec<Address>) {
    let addrs: Vec<Address> = (0..accounts)
        .map(|i| Address::from_slice(&keccak256((i as u64).to_be_bytes())[..20]))
        .collect();
    let mut sorted = addrs.clone();
    sorted.sort();
    let mut out = BTreeMap::new();
    let mut parent = B256::ZERO;
    for n in 1..=blocks {
        let accounts = sorted
            .iter()
            .map(|a| AccountChanges {
                address: *a,
                storage_changes: (0..slots)
                    .map(|s| SlotChanges {
                        // spread slots so both hot (0..slots) and per-block keys appear
                        slot: U256::from(if s % 2 == 0 {
                            s as u64
                        } else {
                            (n * 7919 + s as u64) % 100_000
                        }),
                        changes: vec![StorageChange {
                            block_access_index: 1,
                            value: U256::from(n * 1000 + s as u64),
                        }],
                    })
                    .collect::<Vec<_>>(),
                storage_reads: vec![],
                balance_changes: vec![],
                nonce_changes: vec![],
                code_changes: vec![],
            })
            .map(|mut a| {
                a.storage_changes.sort_by_key(|x| x.slot);
                a.storage_changes.dedup_by(|x, y| x.slot == y.slot);
                a
            })
            .collect();
        let bal = BlockAccessList { accounts };
        let hash = keccak256([parent.as_slice(), &n.to_be_bytes()].concat());
        out.insert(
            n,
            SourcedBlock {
                header: Header {
                    number: n,
                    hash,
                    parent_hash: parent,
                    state_root: B256::ZERO,
                    timestamp: n * 12,
                    block_access_list_hash: Some(bal.hash()),
                },
                bal,
            },
        );
        parent = hash;
    }
    (out, addrs)
}

/// Run the engine part: open a fresh archive, watch, sync from `cached`,
/// then sample reads. Returns metrics and the archive for further use.
async fn engine(
    data_dir: &Path,
    cached: &Cached,
    watched: &[Address],
    first: u64,
    samples: usize,
) -> Result<(Vec<Metric>, Archive, Vec<(Address, B256, u64, B256)>)> {
    let path = data_dir.join("bench.redb");
    let _ = std::fs::remove_file(&path);
    let ar = Archive::open_with(&path, ArchiveConfig::default())?;
    for a in watched {
        ar.watch(*a, first)?;
    }

    // verify only
    let t = Instant::now();
    let mut verified = 0u64;
    for b in cached.blocks.values() {
        if let Some(h) = b.header.block_access_list_hash {
            b.bal.verify(h).map_err(|e| anyhow!("{e}"))?;
            verified += 1;
        }
    }
    let verify_us = t.elapsed().as_secs_f64() * 1e6 / verified.max(1) as f64;

    // verify + apply (sync without bootstrap)
    let t = Instant::now();
    let rep = ar.sync(cached, None).await?;
    let sync_s = t.elapsed().as_secs_f64();
    let per_block_ms = sync_s * 1e3 / rep.blocks_applied.max(1) as f64;

    // collect samples: (addr, slot, block, expected) from the block index
    let mut samples_v = Vec::new();
    let (head, _) = ar.head()?.ok_or_else(|| anyhow!("nothing applied"))?;
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let mut rnd = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let span = head - first + 1;
    let mut tries = 0;
    while samples_v.len() < samples && tries < samples * 20 {
        tries += 1;
        let a = watched[(rnd() as usize) % watched.len()];
        let b = first + rnd() % span;
        let Ok(slots) = ar.changed_slots(a, b) else {
            continue;
        };
        if slots.is_empty() {
            continue;
        }
        let s = slots[(rnd() as usize) % slots.len()];
        // read at a later block so seek-back is exercised
        let at = b + rnd() % (head - b + 1);
        let v = ar.storage_at(a, s, at).map_err(|e| anyhow!("{e}"))?;
        samples_v.push((a, s, at, v.value));
    }

    // timed reads
    let mut lat = Vec::with_capacity(samples_v.len());
    for (a, s, at, _) in &samples_v {
        let t = Instant::now();
        let _ = ar.storage_at(*a, *s, *at);
        lat.push(t.elapsed().as_secs_f64() * 1e6);
    }
    lat.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    let records = rep.slots_written.max(1) as f64;

    let mut m = vec![
        metric(
            "verify",
            verify_us,
            "µs/block",
            "keccak(rlp(bal)) vs header, all blocks",
        ),
        metric(
            "apply",
            per_block_ms,
            "ms/block",
            &format!("verify + write, {} watched addresses", watched.len()),
        ),
        metric(
            "sync throughput",
            rep.blocks_applied as f64 / sync_s.max(1e-9),
            "blocks/s",
            "engine only, BALs from memory",
        ),
        metric(
            "records written",
            rep.slots_written as f64,
            "records",
            "one per changed slot per block",
        ),
        metric(
            "read p50",
            percentile(&lat, 0.5),
            "µs",
            &format!("storage_at, {} random samples", lat.len()),
        ),
        metric("read p99", percentile(&lat, 0.99), "µs", "seek + step back"),
        metric("db size", size as f64 / 1e6, "MB", "redb file after sync"),
        metric(
            "bytes/record",
            size as f64 / records,
            "B",
            "file size / slot records (incl. indexes)",
        ),
    ];
    if rep.blocks_applied == 0 {
        m.clear();
    }
    Ok((m, ar, samples_v))
}

/// Live benchmark against a JSON-RPC endpoint.
pub async fn live(
    rpc: &str,
    blocks: u64,
    top: usize,
    samples: usize,
    data_dir: &Path,
) -> Result<BenchResult> {
    let src = JsonRpcSource::new(rpc);
    let head = src.head().await?;
    let first = head.saturating_sub(blocks) + 1;
    let chain_id = src
        .call("eth_chainId", serde_json::json!([]))
        .await
        .ok()
        .and_then(|v| {
            v.as_str()
                .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        });

    // fetch
    let t = Instant::now();
    let mut cached = BTreeMap::new();
    let mut bytes = 0usize;
    for n in first..=head {
        let b = src
            .block(n)
            .await
            .with_context(|| format!("fetching block {n}"))?;
        bytes += b.bal.encode_rlp().len();
        cached.insert(n, b);
    }
    let fetch_s = t.elapsed().as_secs_f64();
    let cached = Cached { blocks: cached };
    let accounts_avg = cached
        .blocks
        .values()
        .map(|b| b.bal.accounts.len())
        .sum::<usize>() as f64
        / cached.blocks.len().max(1) as f64;

    let watched = most_active(&cached.blocks, top);
    if watched.is_empty() {
        return Err(anyhow!("no storage writes in the last {blocks} blocks"));
    }

    let (mut metrics, _ar, samples_v) = engine(data_dir, &cached, &watched, first, samples).await?;

    // RPC baseline for the same reads: eth_getStorageAt at the sampled block
    let mut rpc_lat = Vec::new();
    let mut mismatches = 0usize;
    for (a, s, at, expected) in samples_v.iter().take(30) {
        let t = Instant::now();
        let r = src
            .call(
                "eth_getStorageAt",
                serde_json::json!([a, s, format!("{at:#x}")]),
            )
            .await;
        rpc_lat.push(t.elapsed().as_secs_f64() * 1e3);
        if let Ok(v) = r {
            let got: Option<B256> = v.as_str().and_then(|h| h.parse().ok());
            if got != Some(*expected) {
                mismatches += 1;
            }
        }
    }
    rpc_lat.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let mut front = vec![
        metric(
            "fetch",
            fetch_s * 1e3 / blocks as f64,
            "ms/block",
            "eth_getBlockByNumber + eth_getBlockAccessList over HTTPS",
        ),
        metric(
            "bal size",
            bytes as f64 / blocks as f64 / 1e3,
            "KB/block",
            "RLP-encoded BAL, average",
        ),
        metric(
            "accounts/block",
            accounts_avg,
            "accounts",
            "touched accounts per BAL, average",
        ),
    ];
    front.append(&mut metrics);
    if !rpc_lat.is_empty() {
        front.push(metric(
            "rpc read p50",
            percentile(&rpc_lat, 0.5),
            "ms",
            "eth_getStorageAt on the same endpoint, same samples",
        ));
        front.push(metric(
            "rpc/archive mismatches",
            mismatches as f64,
            "count",
            "eth_getStorageAt vs archive (window-limited nodes answer only near head)",
        ));
    }
    Ok(BenchResult {
        mode: "live".into(),
        rpc: Some(rpc.to_string()),
        chain_id,
        blocks: (first, head),
        addresses: watched.len(),
        metrics: front,
    })
}

/// Synthetic benchmark, no network.
pub async fn synthetic_run(
    blocks: u64,
    accounts: usize,
    slots: usize,
    samples: usize,
    data_dir: &Path,
) -> Result<BenchResult> {
    let (chain, addrs) = synthetic(blocks, accounts, slots);
    let cached = Cached { blocks: chain };
    let (metrics, _ar, _) = engine(data_dir, &cached, &addrs, 1, samples).await?;
    Ok(BenchResult {
        mode: "synthetic".into(),
        rpc: None,
        chain_id: None,
        blocks: (1, blocks),
        addresses: addrs.len(),
        metrics,
    })
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
