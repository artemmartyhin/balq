//! `--layout auto`: the storage layout of a verified contract, fetched from
//! Sourcify instead of your own build.
//!
//! Sourcify keeps solc's `storageLayout` among a verified contract's
//! compilation artifacts, together with the ABI and, for a proxy, the
//! implementation it points to. Etherscan keeps only the sources, so a
//! contract verified on Etherscan alone is imported into Sourcify first
//! (Sourcify recompiles it — that is what produces the layout) and then
//! read the same way. That import publishes the verification on Sourcify,
//! so it happens only when an Etherscan key is configured.
//!
//! The result is written as an ordinary artifact (`{ "abi", "storageLayout",
//! "balq": { where it came from } }`) to `<archive>.layouts/<chain>/<addr>.json`,
//! so every other command — and `typegen`, and `@balq/node`'s
//! `Layout.fromFile` — reads it like a forge artifact.

use alloy_primitives::Address;
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Public Sourcify server (`sourcify` in balq.toml overrides it).
pub const DEFAULT_SERVER: &str = "https://sourcify.dev/server";

/// How long to wait for an Etherscan import to finish compiling.
const IMPORT_WAIT: Duration = Duration::from_secs(120);

/// Is this `--layout` value (or `layout` in balq.toml) the word `auto`?
pub fn is_auto(p: impl AsRef<Path>) -> bool {
    p.as_ref() == Path::new("auto")
}

/// Where fetched layouts live: next to the archive, one file per chain and address.
pub fn cache_path(data: &Path, chain: u64, addr: Address) -> PathBuf {
    let mut dir = data.as_os_str().to_owned();
    dir.push(".layouts");
    PathBuf::from(dir)
        .join(chain.to_string())
        .join(format!("{addr:#x}.json"))
}

/// A cached layout for `addr` on any chain, if exactly one exists — what
/// `get` / `diff` use, so they need no RPC once `index` has fetched it.
pub fn cached_any_chain(data: &Path, addr: Address) -> Option<PathBuf> {
    let root = cache_path(data, 0, addr);
    let root = root.parent()?.parent()?;
    let name = format!("{addr:#x}.json");
    let mut hits = std::fs::read_dir(root)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path().join(&name))
        .filter(|p| p.is_file());
    let first = hits.next()?;
    hits.next().is_none().then_some(first)
}

/// Settings for a fetch.
pub struct Opts<'a> {
    /// Sourcify server base URL.
    pub server: &'a str,
    /// Etherscan API key, passed to Sourcify's importer (optional: Sourcify
    /// may import without one, rate-limited).
    pub etherscan_key: Option<&'a str>,
    /// Import from Etherscan when Sourcify does not know the contract
    /// (publishes the verification on Sourcify).
    pub import: bool,
}

/// What a fetch found.
#[derive(Debug)]
pub struct Fetched {
    /// The written artifact.
    pub path: PathBuf,
    /// Contract name (the implementation's, for a proxy).
    pub name: String,
    /// For a proxy: the implementation whose layout this is.
    pub implementation: Option<Address>,
    /// Imported from Etherscan into Sourcify during this fetch.
    pub imported: bool,
}

fn agent() -> ureq::Agent {
    ureq::config::Config::builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .http_status_as_error(false)
        .build()
        .new_agent()
}

/// `eth_chainId` of `rpc` — Sourcify is keyed by chain.
pub fn chain_id(rpc: &str) -> Result<u64> {
    let v: Value = agent()
        .post(rpc)
        .send_json(json!({ "jsonrpc": "2.0", "id": 1, "method": "eth_chainId", "params": [] }))
        .with_context(|| format!("eth_chainId from {rpc}"))?
        .body_mut()
        .read_json()
        .with_context(|| format!("eth_chainId from {rpc}"))?;
    let s = v["result"]
        .as_str()
        .with_context(|| format!("eth_chainId from {rpc}: {v}"))?;
    u64::from_str_radix(s.trim_start_matches("0x"), 16)
        .with_context(|| format!("eth_chainId from {rpc}: {s}"))
}

/// One contract as Sourcify has it, or `None` if it is not verified there.
fn lookup(server: &str, chain: u64, addr: Address) -> Result<Option<Value>> {
    let url = format!(
        "{server}/v2/contract/{chain}/{addr:#x}?fields=abi,storageLayout,proxyResolution,compilation.name"
    );
    let mut r = agent()
        .get(&url)
        .call()
        .with_context(|| format!("Sourcify {url}"))?;
    match r.status().as_u16() {
        200 => Ok(Some(r.body_mut().read_json().context("Sourcify response")?)),
        404 => Ok(None),
        s => bail!(
            "Sourcify {url}: HTTP {s}: {}",
            r.body_mut().read_to_string().unwrap_or_default()
        ),
    }
}

/// Ask Sourcify to import `addr` from Etherscan and wait for the job.
fn import_from_etherscan(o: &Opts, chain: u64, addr: Address) -> Result<()> {
    let url = format!("{}/v2/verify/etherscan/{chain}/{addr:#x}", o.server);
    let body = match o.etherscan_key {
        Some(k) => json!({ "apiKey": k }),
        None => json!({}),
    };
    let mut r = agent()
        .post(&url)
        .send_json(body)
        .with_context(|| format!("Sourcify {url}"))?;
    let status = r.status().as_u16();
    let v: Value = r.body_mut().read_json().unwrap_or(Value::Null);
    if status != 202 {
        let msg = v["message"].as_str().unwrap_or("no message");
        bail!("Sourcify could not import {addr} from Etherscan: {msg} (HTTP {status})");
    }
    let id = v["verificationId"]
        .as_str()
        .context("Sourcify import: no verificationId")?;
    let poll = format!("{}/v2/verify/{id}", o.server);
    let deadline = std::time::Instant::now() + IMPORT_WAIT;
    loop {
        std::thread::sleep(Duration::from_secs(2));
        let v: Value = agent()
            .get(&poll)
            .call()
            .with_context(|| format!("Sourcify {poll}"))?
            .body_mut()
            .read_json()
            .context("Sourcify job status")?;
        if v["isJobCompleted"].as_bool() == Some(true) {
            if let Some(e) = v.get("error").filter(|e| !e.is_null()) {
                let msg = e["message"].as_str().unwrap_or("verification failed");
                bail!("Sourcify import of {addr} from Etherscan failed: {msg}");
            }
            return Ok(());
        }
        if std::time::Instant::now() > deadline {
            bail!("Sourcify import of {addr} still running after {IMPORT_WAIT:?}; try again later ({poll})");
        }
    }
}

/// `lookup`, importing from Etherscan on a miss when allowed.
fn lookup_or_import(o: &Opts, chain: u64, addr: Address) -> Result<(Value, bool)> {
    if let Some(v) = lookup(o.server, chain, addr)? {
        return Ok((v, false));
    }
    if !o.import {
        bail!(
            "{addr} is not verified on Sourcify (chain {chain}); if it is verified on Etherscan, set \
             ETHERSCAN_API_KEY (or `etherscan_api_key` in balq.toml) and balq imports it into Sourcify"
        );
    }
    import_from_etherscan(o, chain, addr)
        .with_context(|| format!("{addr} is not verified on Sourcify (chain {chain})"))?;
    match lookup(o.server, chain, addr)? {
        Some(v) => Ok((v, true)),
        None => bail!("{addr}: Sourcify import finished but the contract is still not found"),
    }
}

fn name_of(v: &Value) -> String {
    v["compilation"]["name"].as_str().unwrap_or("?").to_string()
}

/// Fetch the layout of `addr` on `chain` (its implementation's, for a
/// proxy) and write it to `dest`.
pub fn fetch(o: &Opts, chain: u64, addr: Address, dest: &Path) -> Result<Fetched> {
    let (top, mut imported) = lookup_or_import(o, chain, addr)?;
    let proxy = &top["proxyResolution"];
    let (src, implementation) = if proxy["isProxy"].as_bool() == Some(true) {
        let impls = proxy["implementations"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        match impls.as_slice() {
            [one] => {
                let i: Address = one["address"]
                    .as_str()
                    .and_then(|s| s.parse().ok())
                    .context("Sourcify: proxy implementation without an address")?;
                let (v, imp) = lookup_or_import(o, chain, i)
                    .with_context(|| format!("{addr} is a proxy; its implementation {i}"))?;
                imported |= imp;
                (v, Some(i))
            }
            [] => bail!(
                "{addr} is a {} proxy but Sourcify knows no implementation for it; pass --layout {addr:#x}=<artifact>",
                proxy["proxyType"].as_str().unwrap_or("?")
            ),
            many => bail!(
                "{addr} is a {} proxy with {} implementations (a diamond?); one layout cannot name them — use a manifest (see bal-layout)",
                proxy["proxyType"].as_str().unwrap_or("?"),
                many.len()
            ),
        }
    } else {
        (top.clone(), None)
    };
    let name = name_of(&src);
    let layout = &src["storageLayout"];
    if layout.is_null() {
        bail!(
            "Sourcify has {name} ({}) verified but without a storageLayout — solc before 0.5.13 does not emit one",
            implementation.unwrap_or(addr)
        );
    }
    let artifact = json!({
        "abi": src["abi"],
        "storageLayout": layout,
        "balq": {
            "source": "sourcify",
            "server": o.server,
            "chainId": chain,
            "address": addr,
            "contract": name,
            "implementation": implementation,
            "proxyType": proxy["proxyType"],
            "match": src["match"],
            "fetchedAt": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        },
    });
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let text = serde_json::to_string_pretty(&artifact)?;
    std::fs::write(dest, text).with_context(|| format!("writing {}", dest.display()))?;
    Ok(Fetched {
        path: dest.to_path_buf(),
        name,
        implementation,
        imported,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_and_cache_paths() {
        assert!(is_auto("auto") && !is_auto("auto.json") && !is_auto("out/auto"));
        let a: Address = "0x7b79995e5f793A07Bc00c21412e50Ecae098E7f9"
            .parse()
            .unwrap_or_default();
        let p = cache_path(Path::new("/var/balq.redb"), 11155111, a);
        assert_eq!(
            p,
            Path::new(
                "/var/balq.redb.layouts/11155111/0x7b79995e5f793a07bc00c21412e50ecae098e7f9.json"
            )
        );
    }

    #[test]
    fn cached_any_chain_needs_exactly_one() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let data = dir.path().join("balq.redb");
        let a = Address::repeat_byte(0x11);
        assert!(cached_any_chain(&data, a).is_none());
        for chain in [1u64, 2] {
            let p = cache_path(&data, chain, a);
            std::fs::create_dir_all(p.parent().unwrap_or(dir.path())).ok();
            std::fs::write(&p, "{}").ok();
            let found = cached_any_chain(&data, a);
            if chain == 1 {
                assert_eq!(found, Some(p));
            } else {
                assert!(found.is_none(), "two chains: ambiguous, refetch instead");
            }
        }
    }
}
