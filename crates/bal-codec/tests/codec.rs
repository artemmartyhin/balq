//! Codec unit tests: hashing, ordering rules, known-answer encoding.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use alloy_primitives::{address, Address, B256, U256};
use bal_codec::*;

fn acc(addr: Address, slots: &[(u64, &[(u32, u64)])]) -> AccountChanges {
    AccountChanges {
        address: addr,
        storage_changes: slots
            .iter()
            .map(|(slot, ch)| SlotChanges {
                slot: U256::from(*slot),
                changes: ch
                    .iter()
                    .map(|(i, v)| StorageChange {
                        block_access_index: *i,
                        value: U256::from(*v),
                    })
                    .collect(),
            })
            .collect(),
        storage_reads: vec![],
        balance_changes: vec![],
        nonce_changes: vec![],
        code_changes: vec![],
    }
}

const A1: Address = address!("0000000000000000000000000000000000000001");
const A2: Address = address!("0000000000000000000000000000000000000002");

#[test]
fn empty_bal_hash_matches_eip() {
    let bal = BlockAccessList::default();
    assert_eq!(bal.encode_rlp(), vec![0xc0]);
    assert_eq!(bal.hash(), EMPTY_BAL_HASH);
    assert!(bal.verify(EMPTY_BAL_HASH).is_ok());
    assert!(BlockAccessList::decode(&[0xc0]).unwrap().is_empty());
}

#[test]
fn roundtrip_and_lookup() {
    let bal = BlockAccessList {
        accounts: vec![
            acc(A1, &[(0x0a, &[(1, 500), (3, 515)]), (0x0b, &[(2, 1)])]),
            acc(A2, &[]),
        ],
    };
    let bytes = bal.encode_rlp();
    let back = BlockAccessList::decode(&bytes).unwrap();
    assert_eq!(back, bal);
    assert!(back.verify(bal.hash()).is_ok());

    let a = back.account(&A1).unwrap();
    let slot = a
        .slot(&B256::from(U256::from(0x0a).to_be_bytes::<32>()))
        .unwrap();
    assert_eq!(slot.final_change().value, U256::from(515));
    assert!(back.account(&A2).unwrap().storage_changes.is_empty());
    assert!(!back.account(&A2).unwrap().has_storage_changes());
    assert!(back
        .account(&address!("0000000000000000000000000000000000000003"))
        .is_none());
}

#[test]
fn rejects_unsorted_accounts() {
    let bal = BlockAccessList {
        accounts: vec![acc(A2, &[]), acc(A1, &[])],
    };
    let err = BlockAccessList::decode(&bal.encode_rlp()).unwrap_err();
    assert!(matches!(err, CodecError::Ordering(_)), "{err}");
}

#[test]
fn rejects_duplicate_account() {
    let bal = BlockAccessList {
        accounts: vec![acc(A1, &[]), acc(A1, &[])],
    };
    let err = BlockAccessList::decode(&bal.encode_rlp()).unwrap_err();
    assert!(
        matches!(
            err,
            CodecError::Duplicate {
                what: "accounts by address",
                ..
            }
        ),
        "{err}"
    );
}

#[test]
fn rejects_unsorted_slot_changes() {
    let bal = BlockAccessList {
        accounts: vec![acc(A1, &[(0x0a, &[(3, 1), (1, 2)])])],
    };
    let err = BlockAccessList::decode(&bal.encode_rlp()).unwrap_err();
    assert!(matches!(err, CodecError::Ordering(_)), "{err}");
}

#[test]
fn rejects_key_in_both_changes_and_reads() {
    let mut a = acc(A1, &[(0x0a, &[(1, 1)])]);
    a.storage_reads = vec![U256::from(0x0a)];
    let bal = BlockAccessList { accounts: vec![a] };
    let err = BlockAccessList::decode(&bal.encode_rlp()).unwrap_err();
    assert!(matches!(err, CodecError::KeyInChangesAndReads(_)), "{err}");
}

#[test]
fn rejects_empty_slot_change_list() {
    let bal = BlockAccessList {
        accounts: vec![acc(A1, &[(0x0a, &[])])],
    };
    let err = BlockAccessList::decode(&bal.encode_rlp()).unwrap_err();
    assert!(matches!(err, CodecError::EmptySlotChanges { .. }), "{err}");
}

#[test]
fn rejects_trailing_bytes() {
    let err = BlockAccessList::decode(&[0xc0, 0x00]).unwrap_err();
    assert!(matches!(err, CodecError::Trailing(1)), "{err}");
}

#[test]
fn hash_mismatch_is_typed() {
    let bal = BlockAccessList::default();
    let err = bal.verify(B256::ZERO).unwrap_err();
    assert!(matches!(err, CodecError::HashMismatch { .. }));
}

/// Known-answer test: hand-assembled RLP for one account, one slot, one change.
/// [[addr, [[0x0a, [[1, 0x1f4]]]], [], [], [], []]]
#[test]
fn known_encoding() {
    let bal = BlockAccessList {
        accounts: vec![acc(A1, &[(0x0a, &[(1, 0x1f4)])])],
    };
    let expected = concat!(
        "e3",                                         // outer list, 35 bytes payload
        "e2",                                         //  account list, 34 bytes payload
        "940000000000000000000000000000000000000001", //   address
        "c8c70ac5c4018201f4",                         //   [[0x0a, [[1, 0x1f4]]]]
        "c0c0c0c0"                                    //   reads, balance, nonce, code
    );
    assert_eq!(hex::encode(bal.encode_rlp()), expected);
}
