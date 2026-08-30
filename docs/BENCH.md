# Benchmarks

Reproduce: `cargo run --release -p balq -- bench --rpc <url> --blocks 300 --top 20 --out docs/bench`.
No network: `cargo run --release -p balq -- bench --synthetic-blocks 500`.

## Method

**Live.** Fetch the last `blocks` blocks (header + BAL) from the endpoint
into memory, timing the network separately. Pick the `top` addresses with
the most storage writes in that window — the hottest contracts on the
network, not a friendly sample. Open a fresh archive, `watch` them from the
first block, `sync` from the in-memory source (BALs only, no proofs). Then draw `samples` random
`(address, slot, block)` triples from the block index, read each at a
random later block so the seek-back path is exercised, and time it. The
first 30 samples are also fetched with `eth_getStorageAt` on the same
endpoint at the same block and compared bit for bit.

**Synthetic.** Generate `blocks` blocks touching `accounts` addresses ×
`slots` slots each, half of the slots hot (rewritten every block), half
spread over 100k keys. Same engine path, no network, no proofs.

Machine: Windows 11 laptop, release build, NVMe. Endpoint: the public
Platåberget gateway (reth 2.5.0) over HTTPS, so `fetch` includes TLS and a
shared gateway's latency.

## Results — 2026-08-29

### Live, Platåberget, blocks 115329..=115628, 20 addresses

| metric | value | note |
|---|---:|---|
| fetch | 187.8 ms/block | eth_getBlockByNumber + eth_getBlockAccessList over HTTPS |
| bal size | 88.7 KB/block | RLP-encoded BAL, average |
| accounts/block | 214.8 | touched accounts per BAL, average |
| verify | 205.1 µs/block | keccak(rlp(bal)) vs header, all blocks |
| apply | 10.2 ms/block | verify + write, 20 watched addresses |
| sync throughput | 98.2 blocks/s | engine only, BALs from memory |
| records written | 41 677 | one per changed slot per block (~139/block) |
| read p50 | 2.20 µs | storage_at, 5000 random samples |
| read p99 | 3.40 µs | seek + step back |
| db size | 17.4 MB | redb file after sync |
| bytes/record | 417 B | file size / slot records (incl. indexes) |
| rpc read p50 | 77.8 ms | eth_getStorageAt on the same endpoint, same samples |
| rpc/archive mismatches | 0 | 30 control samples |

### Synthetic, 500 blocks × 100 accounts × 20 slots

| metric | value | note |
|---|---:|---|
| verify | 152.6 µs/block | |
| apply | 100.4 ms/block | 2 000 records per block |
| sync throughput | 9.96 blocks/s | ≈ 20 000 records/s |
| records written | 1 000 000 | |
| read p50 / p99 | 3.4 µs / 4.8 µs | 5000 samples over 1M records |
| db size | 539.5 MB | |
| bytes/record | 540 B | |

## Reading the numbers

- **Reads are the point.** 2–5 µs for any slot at any block, flat from 40k
  to 1M records — one B-tree seek. The same read over RPC is ~78 ms on this
  gateway; an archive node on a LAN would be ~1–5 ms. Either way it is three
  to four orders of magnitude, and it does not involve trusting the answer.
- **Sync keeps up with a wide margin.** Mainnet produces one block per 12 s;
  the engine applies a block for 20 hot addresses in 10 ms. In `--follow`
  mode the cost is dominated by the HTTPS fetch (~190 ms here, ~5 ms on a
  local node).
- **Catch-up after downtime** is bounded by fetch, not by the engine: 300
  blocks took 56 s to fetch and 3 s to apply.
- **Per-record disk cost is high: 417–540 B** against a design estimate of
  ~90 B. Each changed slot costs a primary record (64 + 33 B), a block-index
  key (60 B), a bootstrap-state entry (52 + 9 B), and redb's B-tree and page
  overhead on top. A year of a contract with 100 changed slots per block
  would be ~130 GB at this rate. Options, in order of payoff: drop the
  per-slot bootstrap entry once `Done` (it is implied by the presence of a
  proof record), pack the block index, compact the file. None is done yet;
  the number is reported as measured.
- **Synthetic apply is slower per block** (100 ms) because it writes 2 000
  records per block, 14× the live case; per record it is the same ~50 µs.

## Caveats

- One machine, one day, one gateway. Numbers move with disk, CPU and the
  endpoint's load; the ratios are the stable part.
- Bootstrap cost (`eth_getProof`) is not measured: the public gateway
  serves proofs only at the head. On an own node it is one call per
  (address, block) with all first-seen slots batched.
- `rpc read p50` is a network round trip to a shared gateway; it is the
  honest comparison for someone who would otherwise use a provider, not a
  claim about node software.
