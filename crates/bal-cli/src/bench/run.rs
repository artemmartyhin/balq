//! The two runs: a synthetic chain in memory, and a live catch-up on a real
//! network — both through the same `engine`.

use super::*;

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
    // Free pages are part of the file until compaction; report both.
    drop(ar);
    let _ = Archive::compact_file(&path);
    let size_compact = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(size);
    let ar = Archive::open_with(&path, ArchiveConfig::default())?;

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
            "db size compacted",
            size_compact as f64 / 1e6,
            "MB",
            "same file after compact_file()",
        ),
        metric(
            "bytes/record compacted",
            size_compact as f64 / records,
            "B",
            "compacted size / slot records",
        ),
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
