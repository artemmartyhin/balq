# bal-archive

Local, verified history of contract storage. Accumulates every change from
EIP-7928 BALs into an embedded [redb](https://crates.io/crates/redb) file,
handles reorgs and bootstrap, and answers "what was in slot X at block N"
with one ordered seek. Knows blocks and slots; knows nothing about
Solidity — see `bal-layout` for names.

```rust,no_run
use alloy_primitives::{address, B256, U256};
use bal_archive::{Archive, NotAvailable};
use bal_source::JsonRpcSource;

# async fn demo() -> Result<(), Box<dyn std::error::Error>> {
let archive = Archive::open("./balq.redb")?;
let proxy = address!("35825972e2ca90851b14576C531F13dA0B5d53ce");
archive.watch(proxy, 114_563)?;               // must be above the current head

// Forward: fetch → verify keccak(rlp(bal)) against the header → apply.
let node = JsonRpcSource::new("http://localhost:8545");
let report = archive.sync(&node, None).await?;
println!("applied {} blocks", report.blocks_applied);

// Backward: older blocks' BALs down to the contract's creation, each block
// chained by parent_hash to the one above. No proofs, no archive node.
let back = archive.backfill(&node, proxy, bal_archive::BackfillOpts::default()).await?;
println!("history now starts at {} ({:?})", back.to, back.stopped);

let slot = B256::from(U256::from(0).to_be_bytes::<32>());
match archive.storage_at(proxy, slot, 114_591) {
    Ok(v) => println!("{} set at block {} ({:?})", v.value, v.set_at, v.provenance),
    Err(NotAvailable::BeforeStart { start, .. }) => println!("history starts at {start}"),
    Err(e) => println!("no value: {e}"),     // never a silent zero
}
# Ok(()) }
```

Every stored word carries its `Provenance` (`Bal`, `Proof`, or opt-in
`Unverified`/`Imported`). Every miss is a typed `NotAvailable`. All
methods take `&self`: share the archive in an `Arc` and read while `sync`
runs.

`sync(&node, Some(&state))` additionally proves the earlier value of newly
seen slots with `eth_getProof` while the node's state window allows — an
optional shortcut for what `backfill` reads from blocks.

Key layout, backfill and creation rules, reorg handling and the trust model are
documented in the [repository](https://github.com/artemmartyhin/balq)
(`docs/DECISIONS.md`, `docs/SECURITY-AUDIT.md`).
