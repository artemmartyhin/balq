# bal-layout

solc `storageLayout` → slot arithmetic and typed decoding. Give it a path
like `balances[0xabc…]`, `totals.index`, `items[2]` and it answers with a
slot, byte offset and size; give it a 32-byte word and it decodes the
value. Works the other way too: `describe_slot` names everything in a slot
except mapping entries (keccak is one-way).

```rust
use alloy_primitives::{B256, U256};
use bal_layout::{Layout, Value};

let layout = Layout::from_json(r#"{
  "storage": [
    {"label":"counter","slot":"0","offset":0,"type":"t_uint256"},
    {"label":"paused","slot":"1","offset":0,"type":"t_bool"},
    {"label":"owner","slot":"1","offset":1,"type":"t_address"},
    {"label":"balances","slot":"2","offset":0,"type":"t_mapping(t_address,t_uint256)"}
  ],
  "types": {
    "t_uint256": {"encoding":"inplace","label":"uint256","numberOfBytes":"32"},
    "t_bool":    {"encoding":"inplace","label":"bool","numberOfBytes":"1"},
    "t_address": {"encoding":"inplace","label":"address","numberOfBytes":"20"},
    "t_mapping(t_address,t_uint256)": {"encoding":"mapping","label":"mapping(address => uint256)",
        "numberOfBytes":"32","key":"t_address","value":"t_uint256"}
  }}"#)?;

// packed: `paused` is byte 0 of slot 1, `owner` bytes 1..21
let owner = layout.locate("owner")?;
assert_eq!((owner.offset, owner.size), (1, 20));

// mapping key → slot = keccak(pad32(key) ‖ slot)
let bal = layout.locate("balances[0x000000000000000000000000000000000000dEaD]")?;
assert_ne!(bal.slot, B256::ZERO);

let word = B256::from((U256::from(1u8) | (U256::from(0xdead_u64) << 8usize)).to_be_bytes::<32>());
assert_eq!(layout.decode(&layout.locate("paused")?, word), Value::Bool(true));
# Ok::<(), bal_layout::LayoutError>(())
```

The layout comes from your compiler — `forge inspect C storageLayout`, or
`extra_output = ["storageLayout"]` and the whole artifact — not from the
ABI. `typescript(name)` emits a TypeScript interface for the same shape;
`kind_of(path)` says whether a path is a value, struct, mapping or array.

Part of [balq](https://github.com/artemmartyhin/balq).
