//! Known-answer test against consensus data: a real `eth_getBlockAccessList`
//! response from Platåberget (reth 2.5.0), block 114205, whose header carries
//! `blockAccessListHash = 0x8a6e…7241`. If this test fails, either the wire
//! format changed or the JSON mapping is wrong — in both cases nothing above
//! the codec can be trusted until it is fixed.
//!
//! Run with `cargo test -p bal-codec --features json`.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(feature = "json")]

use alloy_primitives::{address, b256};
use bal_codec::BlockAccessList;

const RESPONSE: &str = include_str!("fixtures/plataberget_114205.json");

fn fixture() -> BlockAccessList {
    let v: serde_json::Value = serde_json::from_str(RESPONSE).unwrap();
    BlockAccessList::from_rpc_json(&v["result"]).expect("valid BAL")
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
