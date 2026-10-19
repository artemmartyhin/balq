//! Known-answer tests against consensus data: real `eth_getBlockAccessList`
//! responses whose headers carry the hash they must reproduce —
//! Platåberget (reth 2.5.0) block 114205, `0x8a6e…7241`, and Sepolia
//! (reth 2.7.0) block 11856337, the first Glamsterdam block there,
//! `0xe604…8a55`. If one fails, either the wire format changed or the JSON
//! mapping is wrong — in both cases nothing above the codec can be trusted
//! until it is fixed.
//!
//! Run with `cargo test -p bal-codec --features json`.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(feature = "json")]

use alloy_primitives::{address, b256};
use bal_codec::BlockAccessList;

const RESPONSE: &str = include_str!("fixtures/plataberget_114205.json");
const SEPOLIA_FORK: &str = include_str!("fixtures/sepolia_11856337.json");

fn parse(response: &str) -> BlockAccessList {
    let v: serde_json::Value = serde_json::from_str(response).unwrap();
    BlockAccessList::from_rpc_json(&v["result"]).expect("valid BAL")
}

fn fixture() -> BlockAccessList {
    parse(RESPONSE)
}

#[test]
fn json_roundtrips_to_header_hash() {
    let bal = fixture();
    let expected = b256!("8a6ec46d45d8867379e9edb896993be827c33dcbb2f4fc7ef780623a035f7241");
    assert_eq!(
        bal.hash(),
        expected,
        "keccak(rlp(bal)) must equal the header's blockAccessListHash"
    );
    assert!(bal.verify(expected).is_ok());
    assert_eq!(bal.accounts.len(), 144);
}

#[test]
fn sepolia_fork_block_matches_header() {
    let bal = parse(SEPOLIA_FORK);
    let expected = b256!("e6048b5d71a01b46691aa7d24238ba7880b789ae42173cab10a041b72e768a55");
    assert_eq!(bal.hash(), expected);
    assert!(bal.verify(expected).is_ok());
    assert_eq!(bal.accounts.len(), 280);
    let back = BlockAccessList::decode(&bal.encode_rlp()).unwrap();
    assert_eq!(back, bal);
}

#[test]
fn rlp_roundtrip_preserves_everything() {
    let bal = fixture();
    let bytes = bal.encode_rlp();
    let back = BlockAccessList::decode(&bytes).unwrap();
    assert_eq!(back, bal);
    assert_eq!(back.hash(), bal.hash());
}

#[test]
fn lookups_on_real_data() {
    let bal = fixture();
    // EIP-2935 history contract writes one slot per block.
    let hist = bal
        .account(&address!("0000f90827f1c53a10cb7a02335b175320002935"))
        .expect("history contract is touched every block");
    assert_eq!(hist.storage_changes.len(), 1);
    let slot = &hist.storage_changes[0];
    assert_eq!(slot.changes.len(), 1);
    assert_eq!(
        slot.changes[0].block_access_index, 0,
        "pre-execution system call"
    );
    assert!(hist.has_storage_changes());
    // Accounts stay sorted, so lookups are binary searches that must agree with a scan.
    for acc in &bal.accounts {
        assert_eq!(
            bal.account(&acc.address).map(|a| a.address),
            Some(acc.address)
        );
    }
}
