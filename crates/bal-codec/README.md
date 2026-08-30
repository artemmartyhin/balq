# bal-codec

EIP-7928 Block-Level Access List: RLP decoding, ordering/uniqueness
validation, `keccak256(rlp(bal))`, and verification against the header's
`blockAccessListHash`. Knows nothing about Solidity, storage, or nodes —
this is the one crate a spec change touches.

```rust
use bal_codec::{BlockAccessList, EMPTY_BAL_HASH};

// The empty BAL is `rlp([])` = 0xc0; its hash is fixed by the EIP.
let bal = BlockAccessList::decode(&[0xc0])?;
assert!(bal.is_empty());
assert_eq!(bal.hash(), EMPTY_BAL_HASH);
bal.verify(EMPTY_BAL_HASH)?;
# Ok::<(), bal_codec::CodecError>(())
```

With the `json` feature, the execution-apis JSON form served by
`eth_getBlockAccessList` decodes through the same validation:

```rust,ignore
let v: serde_json::Value = rpc_response["result"].clone();
let bal = BlockAccessList::from_rpc_json(&v)?;
assert_eq!(bal.hash(), header.block_access_list_hash);
```

Lookups use the sorted order the EIP mandates: `bal.account(&addr)` and
`account.slot(&key)` are binary searches. A BAL that violates ordering or
uniqueness is rejected, never softly accepted.

Part of [balq](https://github.com/artemmartyhin/balq).
