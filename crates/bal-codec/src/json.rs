//! JSON form of a BAL as returned by `eth_getBlockAccessList`
//! (execution-apis). Field names observed on Platåberget (reth 2.5.0),
//! 2026-08-29:
//!
//! ```json
//! [{ "address": "0x…",
//!    "storageChanges": [{ "key": "0x1e29", "changes": [{ "index": "0x0", "value": "0x…" }] }],
//!    "storageReads":   ["0x…"],
//!    "balanceChanges": [{ "index": "0x2a", "value": "0x…" }],
//!    "nonceChanges":   [{ "index": "0x16", "value": "0x1809" }],
//!    "codeChanges":    [{ "index": "0x3e", "code": "0x…" }] }]
//! ```
//!
//! Quantities are minimal hex. The result is validated exactly like the RLP
//! path, and its hash is expected to equal the header's — that equality is
//! the proof that this mapping is right.

use crate::{
    AccountChanges, BalanceChange, BlockAccessList, CodeChange, CodecError, NonceChange,
    SlotChanges, StorageChange,
};
use alloy_primitives::{Address, Bytes, U256};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RpcAccount {
    address: Address,
    #[serde(default)]
    storage_changes: Vec<RpcSlot>,
    #[serde(default)]
    storage_reads: Vec<U256>,
    #[serde(default)]
    balance_changes: Vec<RpcIndexed>,
    #[serde(default)]
    nonce_changes: Vec<RpcIndexed>,
    #[serde(default)]
    code_changes: Vec<RpcCode>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RpcSlot {
    key: U256,
    changes: Vec<RpcIndexed>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RpcIndexed {
    index: U256,
    value: U256,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RpcCode {
    index: U256,
    code: Bytes,
}

fn idx(u: U256) -> Result<u32, CodecError> {
    u.try_into()
        .map_err(|_| CodecError::Json(format!("block access index {u} exceeds u32")))
}

impl BlockAccessList {
    /// Decode the JSON result of `eth_getBlockAccessList` and validate it
    /// like RLP input. `null` (block not found) is an error here; callers
    /// handle "not found" before reaching the codec.
    pub fn from_rpc_json(v: &serde_json::Value) -> Result<Self, CodecError> {
        // `&Value` is itself a Deserializer: no clone of the (possibly tens
        // of MB) tree.
        let accounts: Vec<RpcAccount> = serde::Deserialize::deserialize(v)
            .map_err(|e: serde_json::Error| CodecError::Json(e.to_string()))?;
        let mut out = Vec::with_capacity(accounts.len());
        for a in accounts {
            out.push(AccountChanges {
                address: a.address,
                storage_changes: a
                    .storage_changes
                    .into_iter()
                    .map(|s| {
                        Ok(SlotChanges {
                            slot: s.key,
                            changes: s
                                .changes
                                .into_iter()
                                .map(|c| {
                                    Ok(StorageChange {
                                        block_access_index: idx(c.index)?,
                                        value: c.value,
                                    })
                                })
                                .collect::<Result<_, CodecError>>()?,
                        })
                    })
                    .collect::<Result<_, CodecError>>()?,
                storage_reads: a.storage_reads,
                balance_changes: a
                    .balance_changes
                    .into_iter()
                    .map(|c| {
                        Ok(BalanceChange {
                            block_access_index: idx(c.index)?,
                            post_balance: c.value,
                        })
                    })
                    .collect::<Result<_, CodecError>>()?,
                nonce_changes: a
                    .nonce_changes
                    .into_iter()
                    .map(|c| {
                        Ok(NonceChange {
                            block_access_index: idx(c.index)?,
                            new_nonce: c.value.try_into().map_err(|_| {
                                CodecError::Json(format!("nonce {} exceeds u64", c.value))
                            })?,
                        })
                    })
                    .collect::<Result<_, CodecError>>()?,
                code_changes: a
                    .code_changes
                    .into_iter()
                    .map(|c| {
                        Ok(CodeChange {
                            block_access_index: idx(c.index)?,
                            new_code: c.code,
                        })
                    })
                    .collect::<Result<_, CodecError>>()?,
            });
        }
        let bal = BlockAccessList { accounts: out };
        crate::validate::validate(&bal)?;
        Ok(bal)
    }
}
