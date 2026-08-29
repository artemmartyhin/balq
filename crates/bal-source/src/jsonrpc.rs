//! JSON-RPC source. Day-0 findings on Platåberget (reth 2.5.0, 2026-08-29):
//!
//! - the block object carries `blockAccessListHash` but **not** the BAL body;
//! - the body comes from `eth_getBlockAccessList(blockNumberOrTagOrHash)`,
//!   decoded to JSON (execution-apis), available back to block 1;
//! - `debug_getRawBlockAccessList` exists in reth but public gateways block
//!   `debug_*`.
//!
//! Also hosts the day-0 probe.

use crate::{
    AccountProof, BalSource, Header, Result, SourceError, SourcedBlock, StateSource, StorageProof,
};
use alloy_primitives::{Address, Bytes, B256, U256};
use async_trait::async_trait;
use bal_codec::BlockAccessList;
use serde::Deserialize;
use serde_json::{json, Value};

/// Header field carrying `keccak(rlp(bal))` on the block object.
pub const BAL_HASH_FIELD: &str = "blockAccessListHash";
/// JSON-RPC method serving the BAL body (execution-apis).
pub const BAL_METHOD: &str = "eth_getBlockAccessList";
/// Largest JSON-RPC response body accepted, in bytes.
pub const MAX_BODY_BYTES: u64 = 64 * 1024 * 1024;

/// [`BalSource`] + [`StateSource`] over plain JSON-RPC. One request per call;
/// no batching, no retries — callers own that policy.
pub struct JsonRpcSource {
    url: String,
    client: reqwest::Client,
}

#[derive(Deserialize)]
struct RpcResponse {
    result: Option<Value>,
    error: Option<RpcError>,
}

#[derive(Deserialize)]
struct RpcError {
    code: i64,
    message: String,
}

impl JsonRpcSource {
    /// Talk to the JSON-RPC endpoint at `url`. Requests time out after 30 s
    /// so a stalled gateway cannot hang a sync forever; redirects are not
    /// followed, so a request never silently goes to a host you did not name.
    pub fn new(url: impl Into<String>) -> Self {
        // The builder only fails if the TLS backend cannot initialise; in that
        // case no client would work, so a default one is no worse — but it
        // must not silently drop the policies when they *can* be applied.
        let client = Self::hardened_client().unwrap_or_default();
        Self {
            url: url.into(),
            client,
        }
    }

    fn hardened_client() -> reqwest::Result<reqwest::Client> {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()
    }

    /// Like [`JsonRpcSource::new`] but surfaces a client-construction failure.
    pub fn try_new(url: impl Into<String>) -> Result<Self> {
        let client = Self::hardened_client()
            .map_err(|e| SourceError::Transport(format!("http client: {e}")))?;
        Ok(Self {
            url: url.into(),
            client,
        })
    }

    /// Raw JSON-RPC call; returns the `result` member or the error object as [`SourceError::Rpc`].
    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        let http = self
            .client
            .post(&self.url)
            .json(&body)
            .send()
            .await
            .map_err(|e| SourceError::Transport(e.to_string()))?;
        // Gateways answer 5xx with HTML; say so instead of "invalid JSON".
        let status = http.status();
        if !status.is_success() {
            return Err(SourceError::Transport(format!(
                "HTTP {} from {} for {method}",
                status.as_u16(),
                self.url
            )));
        }
        // Read the body with a hard cap: a node must not be able to make us
        // allocate without bound. The EIP caps a BAL at 8 MiB of RLP; its
        // JSON form is a few times larger. 64 MiB is generous.
        if let Some(len) = http.content_length() {
            if len > MAX_BODY_BYTES {
                return Err(SourceError::Transport(format!(
                    "{method}: response of {len} bytes exceeds the {MAX_BODY_BYTES}-byte limit"
                )));
            }
        }
        let mut http = http;
        let mut body: Vec<u8> = Vec::new();
        while let Some(chunk) = http
            .chunk()
            .await
            .map_err(|e| SourceError::Transport(format!("{method}: {e}")))?
        {
            if body.len() + chunk.len() > MAX_BODY_BYTES as usize {
                return Err(SourceError::Transport(format!(
                    "{method}: response exceeds the {MAX_BODY_BYTES}-byte limit"
                )));
            }
            body.extend_from_slice(&chunk);
        }
        let resp: RpcResponse = serde_json::from_slice(&body)
            .map_err(|e| SourceError::Malformed(format!("{method}: {e}")))?;
        if let Some(e) = resp.error {
            return Err(SourceError::Rpc {
                code: e.code,
                message: e.message,
            });
        }
        Ok(resp.result.unwrap_or(Value::Null))
    }

    async fn raw_block(&self, tag: Value) -> Result<Value> {
        let v = self
            .call("eth_getBlockByNumber", json!([tag, false]))
            .await?;
        if v.is_null() {
            return Err(SourceError::BlockNotFound(match &tag {
                Value::String(s) => parse_hex_u64(s).unwrap_or(u64::MAX),
                _ => u64::MAX,
            }));
        }
        Ok(v)
    }

    /// Fetch and decode the BAL of `number` via `eth_getBlockAccessList`.
    pub async fn bal(&self, number: u64) -> Result<BlockAccessList> {
        let v = self
            .call(BAL_METHOD, json!([format!("{number:#x}")]))
            .await?;
        if v.is_null() {
            return Err(SourceError::NoBal(number));
        }
        Ok(BlockAccessList::from_rpc_json(&v)?)
    }

    /// Fetch header + BAL and check the BAL against the header. Used by the
    /// probe; `sync` does the same check itself.
    async fn probe_block(&self, tag: Value) -> BalProbe {
        let v = match self.raw_block(tag).await {
            Ok(v) => v,
            Err(e) => return BalProbe::Error(e.to_string()),
        };
        let header = match parse_header(&v) {
            Ok(h) => h,
            Err(e) => return BalProbe::Error(e.to_string()),
        };
        let bal = match self.bal(header.number).await {
            Ok(b) => b,
            Err(SourceError::NoBal(n)) => return BalProbe::Missing(n),
            Err(e) => return BalProbe::Error(e.to_string()),
        };
        let computed = bal.hash();
        match header.block_access_list_hash {
            None => BalProbe::NoHashInHeader {
                block: header.number,
                accounts: bal.accounts.len(),
            },
            Some(expected) if expected == computed => BalProbe::Verified {
                block: header.number,
                accounts: bal.accounts.len(),
                hash: computed,
            },
            Some(expected) => BalProbe::Mismatch {
                block: header.number,
                computed,
                expected,
            },
        }
    }

    /// Day-0 probe: Q1 (is the BAL served?), Q2 (for old blocks too?), and
    /// does our codec reproduce the header hash on real data.
    pub async fn probe(&self, old_block_age: u64) -> Result<ProbeReport> {
        let head_v = self.raw_block(json!("latest")).await?;
        let head = parse_header(&head_v)?;
        let old_number = head.number.saturating_sub(old_block_age).max(1);
        let chain_id = self
            .call("eth_chainId", json!([]))
            .await
            .ok()
            .and_then(|v| v.as_str().and_then(|s| parse_hex_u64(s).ok()));
        let client_version = self
            .call("web3_clientVersion", json!([]))
            .await
            .ok()
            .and_then(|v| v.as_str().map(String::from));
        let head_fields = head_v
            .as_object()
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default();
        let head_probe = self.probe_block(json!(format!("{:#x}", head.number))).await;
        let old_probe = self.probe_block(json!(format!("{old_number:#x}"))).await;
        let earliest_probe = self.probe_block(json!("0x1")).await;
        let proof_window = self.measure_proof_window(head.number).await;

        Ok(ProbeReport {
            client_version,
            chain_id,
            head: head.number,
            head_fields,
            head_probe,
            old_probe,
            earliest_probe,
            proof_window,
        })
    }

    /// Largest distance behind head at which `eth_getProof` still answers.
    /// reth: `--rpc.eth-proof-window` (default 0 = head only). `Err` if even
    /// the head is refused (no bootstrap possible at all).
    pub async fn measure_proof_window(&self, head: u64) -> std::result::Result<u64, String> {
        let mut ok: Option<u64> = None;
        for k in [0u64, 1, 2, 4, 8, 16, 32, 64, 128, 256, 1024, 4096] {
            if k > head {
                break;
            }
            let r = self
                .call(
                    "eth_getProof",
                    json!([
                        Address::ZERO,
                        Vec::<B256>::new(),
                        format!("{:#x}", head - k)
                    ]),
                )
                .await;
            match r {
                Ok(_) => ok = Some(k),
                Err(e) => {
                    if ok.is_none() {
                        return Err(e.to_string());
                    }
                    break;
                }
            }
        }
        Ok(ok.unwrap_or(0))
    }
}

/// Outcome of fetching one block's BAL and checking it against its header.
#[derive(Debug, Clone)]
pub enum BalProbe {
    /// BAL served and `keccak(rlp(bal)) == header.blockAccessListHash`.
    Verified {
        /// Block number.
        block: u64,
        /// Accounts in the BAL.
        accounts: usize,
        /// The matching hash.
        hash: B256,
    },
    /// BAL served but the header has no hash field (pre-fork block or client gap).
    NoHashInHeader {
        /// Block number.
        block: u64,
        /// Accounts in the BAL.
        accounts: usize,
    },
    /// BAL served, hash differs: codec/spec drift. Nothing should be built on this.
    Mismatch {
        /// Block number.
        block: u64,
        /// What this codec computed.
        computed: B256,
        /// What the header says.
        expected: B256,
    },
    /// `eth_getBlockAccessList` returned null.
    Missing(u64),
    /// Transport or decoding failure.
    Error(String),
}

/// Day-0 findings for one endpoint. Printed by `balq probe`.
#[derive(Debug, Clone)]
pub struct ProbeReport {
    /// `web3_clientVersion`, if served.
    pub client_version: Option<String>,
    /// `eth_chainId`, if served.
    pub chain_id: Option<u64>,
    /// Head block number at probe time.
    pub head: u64,
    /// Field names on the head block object (to spot renamed BAL fields).
    pub head_fields: Vec<String>,
    /// Q1: the head block.
    pub head_probe: BalProbe,
    /// Q2: a block `age` blocks back.
    pub old_probe: BalProbe,
    /// Q2: block 1.
    pub earliest_probe: BalProbe,
    /// Measured `eth_getProof` window (blocks behind head still served), or
    /// the error if proofs are not served at all.
    pub proof_window: std::result::Result<u64, String>,
}

/// A node that answers block N with a header numbered M would otherwise get
/// N's records filed under M. Refuse.
fn expect_number(h: Header, requested: u64) -> Result<Header> {
    if h.number != requested {
        return Err(SourceError::Malformed(format!(
            "asked for block {requested}, node answered with block {}",
            h.number
        )));
    }
    Ok(h)
}

fn parse_hex_u64(s: &str) -> Result<u64> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    u64::from_str_radix(s, 16).map_err(|e| SourceError::Malformed(format!("u64 {s}: {e}")))
}

fn parse_b256(v: &Value, name: &str) -> Result<B256> {
    let s = v
        .as_str()
        .ok_or_else(|| SourceError::Malformed(format!("{name}: not a string")))?;
    s.parse::<B256>()
        .map_err(|e| SourceError::Malformed(format!("{name}: {e}")))
}

fn field<'a>(v: &'a Value, name: &str) -> Result<&'a Value> {
    v.get(name)
        .ok_or_else(|| SourceError::Malformed(format!("missing field {name}")))
}

fn parse_header(v: &Value) -> Result<Header> {
    let num = field(v, "number")?
        .as_str()
        .ok_or_else(|| SourceError::Malformed("number".into()))?;
    let ts = field(v, "timestamp")?
        .as_str()
        .ok_or_else(|| SourceError::Malformed("timestamp".into()))?;
    let bal_hash = v
        .get(BAL_HASH_FIELD)
        .filter(|x| !x.is_null())
        .map(|x| parse_b256(x, BAL_HASH_FIELD))
        .transpose()?;
    Ok(Header {
        number: parse_hex_u64(num)?,
        hash: parse_b256(field(v, "hash")?, "hash")?,
        parent_hash: parse_b256(field(v, "parentHash")?, "parentHash")?,
        state_root: parse_b256(field(v, "stateRoot")?, "stateRoot")?,
        timestamp: parse_hex_u64(ts)?,
        block_access_list_hash: bal_hash,
    })
}

#[async_trait]
impl BalSource for JsonRpcSource {
    async fn header(&self, number: u64) -> Result<Header> {
        let v = self.raw_block(json!(format!("{number:#x}"))).await?;
        expect_number(parse_header(&v)?, number)
    }

    async fn head(&self) -> Result<u64> {
        let v = self.call("eth_blockNumber", json!([])).await?;
        parse_hex_u64(
            v.as_str()
                .ok_or_else(|| SourceError::Malformed("blockNumber".into()))?,
        )
    }

    async fn finalized(&self) -> Result<u64> {
        let v = self.raw_block(json!("finalized")).await?;
        Ok(parse_header(&v)?.number)
    }

    async fn block(&self, number: u64) -> Result<SourcedBlock> {
        let v = self.raw_block(json!(format!("{number:#x}"))).await?;
        let header = expect_number(parse_header(&v)?, number)?;
        let bal = JsonRpcSource::bal(self, number).await?;
        Ok(SourcedBlock { header, bal })
    }

    async fn bal(&self, number: u64) -> Result<BlockAccessList> {
        JsonRpcSource::bal(self, number).await
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProofResp {
    balance: U256,
    nonce: U256,
    code_hash: B256,
    storage_hash: B256,
    account_proof: Vec<Bytes>,
    storage_proof: Vec<StorageProofResp>,
}

#[derive(Deserialize)]
struct StorageProofResp {
    key: U256,
    value: U256,
    proof: Vec<Bytes>,
}

#[async_trait]
impl StateSource for JsonRpcSource {
    async fn proof(&self, addr: Address, slots: &[B256], block: u64) -> Result<AccountProof> {
        let v = self
            .call("eth_getProof", json!([addr, slots, format!("{block:#x}")]))
            .await?;
        let p: ProofResp =
            serde_json::from_value(v).map_err(|e| SourceError::Malformed(format!("proof: {e}")))?;
        let nonce: u64 = p
            .nonce
            .try_into()
            .map_err(|_| SourceError::Malformed(format!("proof nonce {} exceeds u64", p.nonce)))?;
        Ok(AccountProof {
            address: addr,
            balance: p.balance,
            nonce,
            code_hash: p.code_hash,
            storage_hash: p.storage_hash,
            account_proof: p.account_proof,
            storage_proofs: p
                .storage_proof
                .into_iter()
                .map(|s| StorageProof {
                    key: B256::from(s.key.to_be_bytes::<32>()),
                    value: s.value,
                    proof: s.proof,
                })
                .collect(),
        })
    }
}
