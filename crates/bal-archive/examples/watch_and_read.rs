//! Watch a contract, sync to the head, backfill to its deploy, read a slot.
//!
//! ```
//! cargo run -p bal-archive --example watch_and_read -- \
//!     https://rpc.plataberget.ethpandaops.io 0x35825972e2ca90851b14576C531F13dA0B5d53ce
//! ```

use alloy_primitives::{Address, B256, U256};
use bal_archive::{Archive, BackfillOpts, BackfillStop, NotAvailable};
use bal_source::{BalSource, JsonRpcSource};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let rpc = args
        .next()
        .unwrap_or_else(|| "http://localhost:8545".into());
    let addr: Address = args
        .next()
        .unwrap_or_else(|| "0x35825972e2ca90851b14576C531F13dA0B5d53ce".into())
        .parse()?;

    let node = JsonRpcSource::new(&rpc);
    let archive = Archive::open("example.redb")?;

    // New address: start at the node's head, a block that already exists.
    if archive.watchlist()?.iter().all(|(a, _)| *a != addr) {
        let head = node.head().await?;
        let from = archive.head()?.map(|(h, _)| h + 1).unwrap_or(head);
        archive.watch(addr, from)?;
        println!("watching {addr} from block {from}");
    }

    // Forward: every new block's BAL, verified against its header.
    let report = archive.sync(&node, None).await?;
    println!(
        "sync: {} block(s), {} record(s)",
        report.blocks_applied, report.slots_written
    );

    // Backward: older blocks' BALs, chained by parent_hash, down to the deploy.
    // No eth_getProof, no archive node.
    let back = archive
        .backfill(&node, addr, BackfillOpts::default())
        .await?;
    match back.stopped {
        BackfillStop::Creation(c) => {
            println!("backfill: created at {c} — history complete ({} blocks)", back.blocks_scanned)
        }
        other => println!("backfill: history from {} ({other:?})", back.to),
    }

    if let Some((head, _)) = archive.head()? {
        let slot0 = B256::from(U256::ZERO.to_be_bytes::<32>());
        match archive.storage_at(addr, slot0, head) {
            Ok(v) => println!(
                "slot 0 @ {head}: {} (set at {}, {:?})",
                v.value, v.set_at, v.provenance
            ),
            Err(NotAvailable::BeforeStart { start, .. }) => println!("history starts at {start}"),
            Err(e) => println!("slot 0 @ {head}: not available — {e}"),
        }
    }
    Ok(())
}
