//! Watch a contract from the next block, sync once, read a slot.
//!
//! ```
//! cargo run -p bal-archive --example watch_and_read -- \
//!     https://rpc.plataberget.ethpandaops.io 0x35825972e2ca90851b14576C531F13dA0B5d53ce
//! ```
//!
//! On a public gateway the first pass applies nothing (the watch starts
//! above the head); run it again a minute later to see records arrive.

use alloy_primitives::{Address, B256, U256};
use bal_archive::{Archive, NotAvailable};
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

    if archive.watchlist()?.iter().all(|(a, _)| *a != addr) {
        let head = node.head().await?;
        archive.watch(addr, head + 1)?;
        println!("watching {addr} from block {}", head + 1);
    }

    let report = archive.sync(&node, Some(&node)).await?;
    println!(
        "applied {} block(s), {} record(s), bootstrap {} proven / {} pending / {} lost",
        report.blocks_applied,
        report.slots_written,
        report.bootstrapped,
        report.bootstrap_pending,
        report.bootstrap_lost
    );

    if let Some((head, _)) = archive.head()? {
        let slot0 = B256::from(U256::ZERO.to_be_bytes::<32>());
        match archive.storage_at(addr, slot0, head) {
            Ok(v) => println!(
                "slot 0 @ {head}: {} (set at {}, {:?})",
                v.value, v.set_at, v.provenance
            ),
            Err(NotAvailable::NotBootstrapped) => {
                println!("slot 0 never changed since the watch start; proving it at the head…");
                archive.bootstrap_slot(&node, addr, slot0).await?;
                let v = archive.storage_at(addr, slot0, head)?;
                println!("slot 0 @ {head}: {} ({:?})", v.value, v.provenance);
            }
            Err(e) => println!("slot 0 @ {head}: not available — {e}"),
        }
    }
    Ok(())
}
