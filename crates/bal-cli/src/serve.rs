//! `balq index --serve`: answer reads while the process holds the archive.
//! Localhost, plain HTTP, no auth — it exposes exactly what the file does.
//!
//! **JSON-RPC 2.0** on `POST /` (single or batch), so any client that talks
//! to a node talks to balq:
//!
//! ```text
//!   eth_blockNumber                          → archive head
//!   eth_getStorageAt(addr, slot, block)      → 0x-word (verified)
//!   eth_call({to, data}, block)              → return data, when `data` is a
//!                                              compiler-generated getter of a
//!                                              variable in the address's layout
//!   balq_getField(addr, "path", block)       → { value, kind, slot, setAt, provenance }
//!   balq_status()                            → ArchiveStats
//! ```
//!
//! `block` is a hex number or `latest` / `safe` / `finalized` (the head);
//! `pending` is refused. A miss is a JSON-RPC error `-32000` whose `data.code`
//! is the `NotAvailable` code (`NotWatched`, `AfterHead`, `BeforeStart`,
//! `NeverRecorded`, `UnknownBefore`); a call that is not a getter is
//! `-32601` — clients fall back to a node on either.
//!
//! **REST** on `GET` for the CLI's own read commands:
//!
//! ```text
//!   /status  /watchlist  /head
//!   /storage/{addr}/{slot}/{block}
//!   /history/{addr}/{slot}?from=A&to=B
//!   /changed/{addr}/{block}
//! ```
//!
//! A sidecar file `<archive>.serve` holds the URL; `Ctx::open` reads it
//! when the file is locked.
//!
//! **Pass-through** (`--upstream`): everything above that balq cannot
//! answer — any other method, `latest` / `safe` / `finalized` reads, every
//! miss — is sent unchanged to the node and its answer returned, so the
//! server is a drop-in RPC URL for software that cannot take a custom
//! transport (Subsquid, Envio, scripts): historical reads of indexed
//! contracts come from the archive, the rest from the node. Each response
//! says which in an `X-Balq` header (`local`, `upstream`, `mixed`).

use crate::util::{na_code, na_json_full, prov, Layouts};
use alloy_primitives::{Address, B256, U256};
use bal_archive::{Archive, NotAvailable};
use bal_layout::{encode_return, Value as LValue};
use serde_json::{json, Value};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tiny_http::{Header, Method, Response, Server};

/// Largest request body accepted.
const MAX_REQUEST: usize = 1 << 20;
/// Largest upstream response relayed (`eth_getLogs` can be big).
const MAX_UPSTREAM: u64 = 64 << 20;
/// Threads answering requests: a slow pass-through must not stall reads.
const WORKERS: usize = 8;

/// JSON-RPC requests answered from the archive / relayed upstream since
/// start; `balq_status` reports them.
static ANSWERED_LOCAL: AtomicU64 = AtomicU64::new(0);
static RELAYED: AtomicU64 = AtomicU64::new(0);

/// Path of the sidecar that advertises a running server for `data`.
pub fn sidecar(data: &Path) -> PathBuf {
    let mut p = data.as_os_str().to_owned();
    p.push(".serve");
    PathBuf::from(p)
}

/// Removes the sidecar when dropped (normal exit); a killed process leaves
/// it behind, and the client treats a refused connection as stale.
pub struct Advertised(PathBuf);

impl Drop for Advertised {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Bind `listen`, write the sidecar, and answer on background threads.
/// With `upstream`, whatever balq cannot answer is relayed to that node.
pub fn start(
    ar: Arc<Archive>,
    layouts: Arc<Layouts>,
    data: &Path,
    listen: &str,
    upstream: Option<String>,
) -> anyhow::Result<(String, Advertised)> {
    let server =
        Arc::new(Server::http(listen).map_err(|e| anyhow::anyhow!("serve on {listen}: {e}"))?);
    let url = format!("http://{}", server.server_addr());
    let side = sidecar(data);
    std::fs::write(&side, &url)?;
    let up = upstream.map(|u| Arc::new(Upstream::new(u)));
    for i in 0..WORKERS {
        let (server, ar, layouts, up) = (server.clone(), ar.clone(), layouts.clone(), up.clone());
        std::thread::Builder::new()
            .name(format!("balq-serve-{i}"))
            .spawn(move || {
                for req in server.incoming_requests() {
                    answer(req, &ar, &layouts, up.as_deref());
                }
            })?;
    }
    Ok((url, Advertised(side)))
}

fn answer(mut req: tiny_http::Request, ar: &Archive, layouts: &Layouts, up: Option<&Upstream>) {
    let (status, body, via) = match req.method() {
        Method::Post => {
            let mut buf = String::new();
            let read = req
                .as_reader()
                .take(MAX_REQUEST as u64 + 1)
                .read_to_string(&mut buf);
            if read.is_err() || buf.len() > MAX_REQUEST {
                (
                    413,
                    rpc_error(Value::Null, -32600, "request too large", None),
                    None,
                )
            } else {
                let relayed = match up {
                    Some(up) => relay_dispatch(ar, layouts, up, &buf),
                    None => Ok((rpc_dispatch(ar, layouts, &buf), Via::Local)),
                };
                match relayed {
                    Ok((body, via)) => (200, body, Some(via)),
                    Err(r) => return refuse(req, r),
                }
            }
        }
        _ => {
            let (s, b) = rest(ar, req.url());
            (s, b, None)
        }
    };
    let mut resp = Response::from_string(body.to_string()).with_status_code(status);
    if let Ok(h) = Header::from_bytes("Content-Type", "application/json") {
        resp = resp.with_header(h);
    }
    if let (Some(v), true) = (via, up.is_some()) {
        if let Ok(h) = Header::from_bytes("X-Balq", v.as_str()) {
            resp = resp.with_header(h);
        }
    }
    let _ = req.respond(resp);
}

/// Hand the node's refusal (429, 5xx, a dead connection) to the client as
/// the node gave it, so the client's own retry and backoff apply.
fn refuse(req: tiny_http::Request, r: Refused) {
    let mut resp = Response::from_string(r.body).with_status_code(r.status);
    for (k, v) in [
        Some(("X-Balq", "upstream".to_string())),
        r.retry_after.map(|v| ("Retry-After", v)),
    ]
    .into_iter()
    .flatten()
    {
        if let Ok(h) = Header::from_bytes(k, v.as_bytes()) {
            resp = resp.with_header(h);
        }
    }
    let _ = req.respond(resp);
}

// ---- JSON-RPC ---------------------------------------------------------------

fn rpc_error(id: Value, code: i64, message: &str, data: Option<Value>) -> Value {
    let mut err = json!({ "code": code, "message": message });
    if let Some(d) = data {
        err["data"] = d;
    }
    json!({ "jsonrpc": "2.0", "id": id, "error": err })
}

fn rpc_ok(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn rpc_dispatch(ar: &Archive, layouts: &Layouts, body: &str) -> Value {
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        return rpc_error(Value::Null, -32700, "parse error", None);
    };
    match v {
        Value::Array(reqs) => {
            Value::Array(reqs.into_iter().map(|r| rpc_one(ar, layouts, r)).collect())
        }
        other => rpc_one(ar, layouts, other),
    }
}

fn rpc_one(ar: &Archive, layouts: &Layouts, req: Value) -> Value {
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let Some(method) = req.get("method").and_then(|m| m.as_str()) else {
        return rpc_error(id, -32600, "invalid request: no method", None);
    };
    let params = req.get("params").cloned().unwrap_or(Value::Array(vec![]));
    let params = params.as_array().cloned().unwrap_or_default();
    match handle(ar, layouts, method, &params) {
        Ok(result) => rpc_ok(id, result),
        Err(RpcErr::Invalid(m)) => rpc_error(id, -32602, &m, None),
        Err(RpcErr::NoMethod(m)) => rpc_error(id, -32601, &m, None),
        Err(RpcErr::Miss(e)) => {
            let data = na_json_full(&e)["error"].clone();
            rpc_error(id, -32000, &e.to_string(), Some(data))
        }
        Err(RpcErr::Internal(m)) => rpc_error(id, -32603, &m, None),
    }
}

enum RpcErr {
    Invalid(String),
    NoMethod(String),
    Miss(NotAvailable),
    Internal(String),
}

impl From<NotAvailable> for RpcErr {
    fn from(e: NotAvailable) -> Self {
        RpcErr::Miss(e)
    }
}

impl From<bal_archive::ArchiveError> for RpcErr {
    fn from(e: bal_archive::ArchiveError) -> Self {
        RpcErr::Internal(e.to_string())
    }
}

fn hex_word(w: B256) -> String {
    format!("{w}")
}

fn parse_addr(v: Option<&Value>, what: &str) -> Result<Address, RpcErr> {
    v.and_then(|x| x.as_str())
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| RpcErr::Invalid(format!("{what}: expected an address")))
}

fn parse_word(v: Option<&Value>, what: &str) -> Result<B256, RpcErr> {
    let s = v
        .and_then(|x| x.as_str())
        .ok_or_else(|| RpcErr::Invalid(format!("{what}: expected a hex string")))?;
    let u = match s.strip_prefix("0x") {
        Some(h) => U256::from_str_radix(h, 16),
        None => s.parse::<U256>(),
    }
    .map_err(|_| RpcErr::Invalid(format!("{what}: bad number {s}")))?;
    Ok(B256::from(u.to_be_bytes::<32>()))
}

/// `latest` / `safe` / `finalized` → the archive head; hex or decimal → that
/// block; `pending` and `earliest` are refused (nothing sensible to answer).
fn parse_block(ar: &Archive, v: Option<&Value>) -> Result<u64, RpcErr> {
    let head = || -> Result<u64, RpcErr> {
        ar.head()?
            .map(|(h, _)| h)
            .ok_or(RpcErr::Miss(NotAvailable::NotSynced))
    };
    match v {
        None | Some(Value::Null) => head(),
        Some(Value::String(s)) => match s.as_str() {
            "latest" | "safe" | "finalized" => head(),
            "pending" | "earliest" => Err(RpcErr::Invalid(format!("block tag {s} is not served"))),
            h => {
                let n = match h.strip_prefix("0x") {
                    Some(x) => u64::from_str_radix(x, 16),
                    None => h.parse::<u64>(),
                }
                .map_err(|_| RpcErr::Invalid(format!("bad block {h}")))?;
                Ok(n)
            }
        },
        Some(Value::Number(n)) => n
            .as_u64()
            .ok_or_else(|| RpcErr::Invalid("bad block number".into())),
        Some(Value::Object(o)) => parse_block(ar, o.get("blockNumber")),
        Some(other) => Err(RpcErr::Invalid(format!("bad block {other}"))),
    }
}

fn handle(ar: &Archive, layouts: &Layouts, method: &str, p: &[Value]) -> Result<Value, RpcErr> {
    match method {
        "eth_blockNumber" => {
            let (h, _) = ar.head()?.ok_or(RpcErr::Miss(NotAvailable::NotSynced))?;
            Ok(json!(format!("{h:#x}")))
        }
        "eth_chainId" | "net_version" => Err(RpcErr::NoMethod(format!(
            "{method}: ask the node; balq serves storage only"
        ))),
        "eth_getStorageAt" => {
            let addr = parse_addr(p.first(), "address")?;
            let slot = parse_word(p.get(1), "slot")?;
            let block = parse_block(ar, p.get(2))?;
            let v = ar.storage_at(addr, slot, block)?;
            Ok(json!(hex_word(v.value)))
        }
        "eth_call" => {
            let call = p
                .first()
                .and_then(|v| v.as_object())
                .ok_or_else(|| RpcErr::Invalid("eth_call: expected a call object".into()))?;
            let to = parse_addr(call.get("to"), "to")?;
            let data = call
                .get("data")
                .or(call.get("input"))
                .and_then(|d| d.as_str())
                .ok_or_else(|| RpcErr::Invalid("eth_call: no data".into()))?;
            let data = alloy_primitives::hex::decode(data)
                .map_err(|e| RpcErr::Invalid(format!("eth_call: data: {e}")))?;
            let block = parse_block(ar, p.get(1))?;
            let Some(c) = layouts.contract(&to) else {
                return Err(RpcErr::NoMethod(format!("eth_call: no layout for {to}")));
            };
            let resolved = c
                .layout
                .resolve_call(&c.getters, &data)
                .map_err(|e| RpcErr::Invalid(format!("eth_call: {e}")))?;
            let Some(r) = resolved else {
                return Err(RpcErr::NoMethod(
                    "eth_call: not a getter of a variable in the layout".into(),
                ));
            };
            let mut values = Vec::with_capacity(r.reads.len());
            for loc in &r.reads {
                let word = ar.storage_at(to, loc.slot, block)?.value;
                let v = if c.layout.is_dynamic_bytes(loc) {
                    let mut chunks = Vec::new();
                    for s in c.layout.bytes_data_slots(loc, word) {
                        chunks.push(ar.storage_at(to, s, block)?.value);
                    }
                    c.layout.decode_bytes(loc, word, &chunks)
                } else {
                    c.layout.decode(loc, word)
                };
                values.push(v);
            }
            let out = encode_return(&values, &r.outputs)
                .map_err(|e| RpcErr::Internal(format!("encode: {e}")))?;
            Ok(json!(format!("0x{}", alloy_primitives::hex::encode(out))))
        }
        "balq_getField" => {
            let addr = parse_addr(p.first(), "address")?;
            let path = p
                .get(1)
                .and_then(|v| v.as_str())
                .ok_or_else(|| RpcErr::Invalid("path: expected a string".into()))?;
            let block = parse_block(ar, p.get(2))?;
            let Some(c) = layouts.contract(&addr) else {
                return Err(RpcErr::NoMethod(format!("no layout for {addr}")));
            };
            let loc = c
                .layout
                .locate(path)
                .map_err(|e| RpcErr::Invalid(e.to_string()))?;
            let v = ar.storage_at(addr, loc.slot, block)?;
            let (kind, value) = if c.layout.is_dynamic_bytes(&loc) {
                let mut chunks = Vec::new();
                for s in c.layout.bytes_data_slots(&loc, v.value) {
                    chunks.push(ar.storage_at(addr, s, block)?.value);
                }
                let dv = c.layout.decode_bytes(&loc, v.value, &chunks);
                (
                    match dv {
                        LValue::Str(_) => "string",
                        _ => "bytes",
                    },
                    dv.to_string(),
                )
            } else {
                let (k, dv) = c.layout.decode_typed(&loc, v.value);
                (
                    match k {
                        bal_layout::ValueKind::Uint => "uint",
                        bal_layout::ValueKind::Int => "int",
                        bal_layout::ValueKind::Bool => "bool",
                        bal_layout::ValueKind::Address => "address",
                        bal_layout::ValueKind::Bytes => "bytes",
                        bal_layout::ValueKind::DynBytes => "bytes",
                        bal_layout::ValueKind::String => "string",
                        bal_layout::ValueKind::Raw => "raw",
                    },
                    dv.to_string(),
                )
            };
            Ok(json!({
                "value": value, "kind": kind, "slot": hex_word(loc.slot), "word": hex_word(v.value),
                "setAt": v.set_at, "provenance": prov(v.provenance),
            }))
        }
        "balq_status" => {
            let mut v = crate::util::stats_json(&ar.stats()?);
            v["served"] = json!({
                "local": ANSWERED_LOCAL.load(Ordering::Relaxed),
                "relayed": RELAYED.load(Ordering::Relaxed),
            });
            Ok(v)
        }
        other => Err(RpcErr::NoMethod(format!("{other}: not served by balq"))),
    }
}

// ---- pass-through ------------------------------------------------------------

/// Who answered a request.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Via {
    Local,
    Upstream,
    Mixed,
}

impl Via {
    fn as_str(self) -> &'static str {
        match self {
            Via::Local => "local",
            Via::Upstream => "upstream",
            Via::Mixed => "mixed",
        }
    }
}

/// The node said no at the HTTP level (or could not be reached: 502).
struct Refused {
    status: u16,
    body: String,
    retry_after: Option<String>,
}

/// The node behind `--upstream`.
struct Upstream {
    url: String,
    agent: ureq::Agent,
}

impl Upstream {
    fn new(url: String) -> Self {
        let agent = ureq::config::Config::builder()
            .timeout_global(Some(std::time::Duration::from_secs(60)))
            .http_status_as_error(false)
            .build()
            .new_agent();
        Self { url, agent }
    }

    /// POST `body` as is; the node's JSON back, or its refusal as is.
    fn send(&self, body: &Value) -> Result<Value, Refused> {
        let gone = |e: String| Refused {
            status: 502,
            body: format!("upstream {}: {e}", self.url),
            retry_after: None,
        };
        let mut resp = self
            .agent
            .post(&self.url)
            .send_json(body)
            .map_err(|e| gone(e.to_string()))?;
        let status = resp.status().as_u16();
        let retry_after = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .map(String::from);
        let text = resp
            .body_mut()
            .with_config()
            .limit(MAX_UPSTREAM)
            .read_to_string()
            .map_err(|e| gone(e.to_string()))?;
        if !(200..300).contains(&status) {
            return Err(Refused {
                status,
                body: text,
                retry_after,
            });
        }
        serde_json::from_str(&text).map_err(|e| gone(format!("not JSON ({e})")))
    }
}

/// May balq answer this request itself? Only storage reads at a definite
/// block: `latest` and friends are the node's — it is at the head, the
/// archive may be a poll behind.
fn local_candidate(req: &Value) -> bool {
    let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let params = req.get("params").and_then(|p| p.as_array());
    let block = match method {
        "eth_getStorageAt" | "balq_getField" => params.and_then(|p| p.get(2)),
        "eth_call" => params.and_then(|p| p.get(1)),
        "balq_status" => return true,
        _ => return false,
    };
    match block {
        None | Some(Value::Null) => false,
        Some(Value::String(t)) => !matches!(
            t.as_str(),
            "latest" | "safe" | "finalized" | "pending" | "earliest"
        ),
        Some(Value::Object(o)) => o.get("blockNumber").is_some(),
        Some(_) => true,
    }
}

/// Answer what the archive can, relay the rest (a batch's relayed part as
/// one upstream batch), keep the client's order and ids.
fn relay_dispatch(
    ar: &Archive,
    layouts: &Layouts,
    up: &Upstream,
    body: &str,
) -> Result<(Value, Via), Refused> {
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        return Ok((
            rpc_error(Value::Null, -32700, "parse error", None),
            Via::Local,
        ));
    };
    let one = |r: &Value| -> Option<Value> {
        if !local_candidate(r) {
            return None;
        }
        let out = rpc_one(ar, layouts, r.clone());
        out.get("error").is_none().then_some(out)
    };
    match v {
        Value::Array(reqs) => {
            let mut out: Vec<Option<Value>> = reqs.iter().map(one).collect();
            let relay: Vec<usize> = (0..reqs.len()).filter(|i| out[*i].is_none()).collect();
            ANSWERED_LOCAL.fetch_add((reqs.len() - relay.len()) as u64, Ordering::Relaxed);
            RELAYED.fetch_add(relay.len() as u64, Ordering::Relaxed);
            let via = match (relay.is_empty(), relay.len() == reqs.len()) {
                (true, _) => Via::Local,
                (false, true) => Via::Upstream,
                (false, false) => Via::Mixed,
            };
            if !relay.is_empty() {
                let batch = Value::Array(relay.iter().map(|i| reqs[*i].clone()).collect());
                let answers = match up.send(&batch)? {
                    Value::Array(a) => a,
                    single => vec![single; relay.len()],
                };
                // Match by id where the node gave them, else by position.
                for (k, i) in relay.iter().enumerate() {
                    let id = reqs[*i].get("id");
                    if id.is_none() {
                        continue; // a notification gets no answer
                    }
                    let hit = answers
                        .iter()
                        .find(|a| id.is_some() && a.get("id") == id)
                        .or_else(|| answers.get(k))
                        .cloned();
                    out[*i] = Some(hit.unwrap_or_else(|| {
                        rpc_error(
                            id.cloned().unwrap_or(Value::Null),
                            -32603,
                            "upstream: no answer",
                            None,
                        )
                    }));
                }
            }
            Ok((Value::Array(out.into_iter().flatten().collect()), via))
        }
        r => match one(&r) {
            Some(local) => {
                ANSWERED_LOCAL.fetch_add(1, Ordering::Relaxed);
                Ok((local, Via::Local))
            }
            None => {
                RELAYED.fetch_add(1, Ordering::Relaxed);
                Ok((up.send(&r)?, Via::Upstream))
            }
        },
    }
}

// ---- REST (the CLI's own read path) -----------------------------------------

fn parse_word_str(s: &str) -> Option<B256> {
    let u = match s.strip_prefix("0x") {
        Some(h) => U256::from_str_radix(h, 16).ok()?,
        None => s.parse::<U256>().ok()?,
    };
    Some(B256::from(u.to_be_bytes::<32>()))
}

fn rest(ar: &Archive, url: &str) -> (u16, Value) {
    let (path, query) = url.split_once('?').unwrap_or((url, ""));
    let q = |k: &str| -> Option<String> {
        query.split('&').find_map(|kv| {
            kv.split_once('=')
                .filter(|(a, _)| *a == k)
                .map(|(_, v)| v.to_string())
        })
    };
    let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
    let bad = |m: &str| {
        (
            400u16,
            json!({ "error": { "code": "BadRequest", "message": m } }),
        )
    };
    let internal = |e: &dyn std::fmt::Display| {
        (
            500u16,
            json!({ "error": { "code": "Internal", "message": e.to_string() } }),
        )
    };
    match parts.as_slice() {
        ["status"] => match ar.stats() {
            Ok(s) => (200, crate::util::stats_json(&s)),
            Err(e) => internal(&e),
        },
        ["watchlist"] => match ar.watchlist() {
            Ok(w) => (
                200,
                json!(w
                    .iter()
                    .map(|(a, f)| json!({ "address": a, "from": f, "createdAt": ar.created_at(*a).ok().flatten() }))
                    .collect::<Vec<_>>()),
            ),
            Err(e) => internal(&e),
        },
        ["head"] => match ar.head() {
            Ok(h) => (
                200,
                h.map(|(n, h)| json!({ "number": n, "hash": h }))
                    .unwrap_or(Value::Null),
            ),
            Err(e) => internal(&e),
        },
        ["storage", a, s, b] => {
            let (Ok(a), Some(s), Ok(b)) = (a.parse::<Address>(), parse_word_str(s), b.parse::<u64>())
            else {
                return bad("storage/{address}/{slot}/{block}");
            };
            match ar.storage_at(a, s, b) {
                Ok(v) => (
                    200,
                    json!({ "value": v.value, "provenance": prov(v.provenance), "setAt": v.set_at, "index": v.index }),
                ),
                Err(e) => (404, na_json_full(&e)),
            }
        }
        ["history", a, s] => {
            let (Ok(a), Some(s), Some(from), Some(to)) = (
                a.parse::<Address>(),
                parse_word_str(s),
                q("from").and_then(|v| v.parse::<u64>().ok()),
                q("to").and_then(|v| v.parse::<u64>().ok()),
            ) else {
                return bad("history/{address}/{slot}?from=A&to=B");
            };
            match ar.history(a, s, from..to) {
                Ok(h) => (
                    200,
                    json!(h
                        .iter()
                        .map(|e| json!({ "block": e.block, "index": e.index, "value": e.value, "provenance": prov(e.provenance) }))
                        .collect::<Vec<_>>()),
                ),
                Err(e) => (404, na_json_full(&e)),
            }
        }
        ["changed", a, b] => {
            let (Ok(a), Ok(b)) = (a.parse::<Address>(), b.parse::<u64>()) else {
                return bad("changed/{address}/{block}");
            };
            match ar.changed_slots(a, b) {
                Ok(s) => (200, json!(s)),
                Err(e) => (404, na_json_full(&e)),
            }
        }
        _ => (
            404,
            json!({ "error": { "code": "NoRoute", "message": format!("unknown route; JSON-RPC is POST / (codes: {})", na_code(&NotAvailable::NotSynced)) } }),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A node that answers every request with `"up:<method>"`, and 429 to
    /// any body mentioning `ratelimit`.
    fn fake_node() -> String {
        let server = Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}", server.server_addr());
        std::thread::spawn(move || {
            for mut req in server.incoming_requests() {
                let mut body = String::new();
                req.as_reader().read_to_string(&mut body).unwrap();
                if body.contains("ratelimit") {
                    let resp = Response::from_string("slow down")
                        .with_status_code(429)
                        .with_header(Header::from_bytes("Retry-After", "3").unwrap());
                    let _ = req.respond(resp);
                    continue;
                }
                let answer = |r: &Value| {
                    rpc_ok(
                        r["id"].clone(),
                        json!(format!("up:{}", r["method"].as_str().unwrap())),
                    )
                };
                let v: Value = serde_json::from_str(&body).unwrap();
                let out = match &v {
                    Value::Array(a) => Value::Array(a.iter().map(answer).collect()),
                    r => answer(r),
                };
                let _ = req.respond(Response::from_string(out.to_string()));
            }
        });
        url
    }

    fn rpc(id: u64, method: &str, params: Value) -> Value {
        json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
    }

    #[test]
    fn only_definite_block_reads_are_local_candidates() {
        let a = "0x35825972e2ca90851b14576C531F13dA0B5d53ce";
        let call = json!({ "to": a, "data": "0x06fdde03" });
        for (req, local) in [
            (rpc(1, "eth_getStorageAt", json!([a, "0x0", "0x10"])), true),
            (
                rpc(1, "eth_getStorageAt", json!([a, "0x0", "latest"])),
                false,
            ),
            (rpc(1, "eth_getStorageAt", json!([a, "0x0"])), false),
            (rpc(1, "eth_call", json!([call, "0x10"])), true),
            (
                rpc(1, "eth_call", json!([call, { "blockNumber": "0x10" }])),
                true,
            ),
            (
                rpc(1, "eth_call", json!([call, { "blockHash": "0xab" }])),
                false,
            ),
            (rpc(1, "eth_call", json!([call, "finalized"])), false),
            (rpc(1, "eth_call", json!([call])), false),
            (rpc(1, "balq_getField", json!([a, "counter", "0x10"])), true),
            (rpc(1, "eth_blockNumber", json!([])), false),
            (rpc(1, "eth_getLogs", json!([{}])), false),
            (rpc(1, "balq_status", json!([])), true),
        ] {
            assert_eq!(local_candidate(&req), local, "{req}");
        }
    }

    #[test]
    fn relay_answers_locally_relays_misses_and_keeps_batch_order() {
        let dir = tempfile::tempdir().unwrap();
        let ar = Archive::open(dir.path().join("a.redb")).unwrap();
        let layouts = Layouts::default();
        let up = Upstream::new(fake_node());
        let a = "0x35825972e2ca90851b14576C531F13dA0B5d53ce";

        // Not watched: a miss, so the node answers — never an error.
        let one = rpc(7, "eth_getStorageAt", json!([a, "0x0", "0x10"])).to_string();
        let Ok((v, via)) = relay_dispatch(&ar, &layouts, &up, &one) else {
            panic!("relayed")
        };
        assert_eq!(
            (v["id"].clone(), v["result"].clone(), via),
            (json!(7), json!("up:eth_getStorageAt"), Via::Upstream)
        );

        // A batch: local, relayed, a notification (no answer), relayed.
        let mut note = rpc(0, "eth_chainId", json!([]));
        note.as_object_mut().unwrap().remove("id");
        let batch = json!([
            rpc(1, "balq_status", json!([])),
            rpc(2, "eth_chainId", json!([])),
            note,
            rpc(3, "eth_getLogs", json!([{}])),
        ])
        .to_string();
        let Ok((v, via)) = relay_dispatch(&ar, &layouts, &up, &batch) else {
            panic!("relayed")
        };
        assert_eq!(via, Via::Mixed);
        let ids: Vec<_> = v
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].clone())
            .collect();
        assert_eq!(ids, vec![json!(1), json!(2), json!(3)]);
        assert!(v[0]["result"]["fileBytes"].is_u64(), "balq_status is local");
        assert_eq!(v[1]["result"], "up:eth_chainId");
        assert_eq!(v[2]["result"], "up:eth_getLogs");
    }

    #[test]
    fn relay_hands_back_the_nodes_refusal() {
        let dir = tempfile::tempdir().unwrap();
        let ar = Archive::open(dir.path().join("a.redb")).unwrap();
        let up = Upstream::new(fake_node());
        let req = rpc(1, "eth_getLogs", json!([{ "ratelimit": true }])).to_string();
        let Err(r) = relay_dispatch(&ar, &Layouts::default(), &up, &req) else {
            panic!("refused")
        };
        assert_eq!(
            (r.status, r.body.as_str(), r.retry_after.as_deref()),
            (429, "slow down", Some("3"))
        );

        let dead = Upstream::new("http://127.0.0.1:9".into());
        let Err(r) = relay_dispatch(&ar, &Layouts::default(), &dead, &req) else {
            panic!("refused")
        };
        assert_eq!(r.status, 502);
    }
}
