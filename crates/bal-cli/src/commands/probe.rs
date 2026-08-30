use super::Ctx;
use crate::util::emit;
use anyhow::Result;
use bal_source::{BalProbe, JsonRpcSource, BAL_HASH_FIELD, BAL_METHOD};
use serde_json::json;

fn probe_json(p: &BalProbe) -> serde_json::Value {
    match p {
        BalProbe::Verified {
            block,
            accounts,
            hash,
        } => json!({ "block": block, "status": "verified", "accounts": accounts, "hash": hash }),
        BalProbe::NoHashInHeader { block, accounts } => {
            json!({ "block": block, "status": "no_hash_in_header", "accounts": accounts })
        }
        BalProbe::Mismatch {
            block,
            computed,
            expected,
        } => {
            json!({ "block": block, "status": "mismatch", "computed": computed, "expected": expected })
        }
        BalProbe::Missing(b) => json!({ "block": b, "status": "missing" }),
        BalProbe::Error(e) => json!({ "status": "error", "message": e }),
    }
}

fn probe_line(p: &BalProbe) -> String {
    match p {
        BalProbe::Verified {
            block,
            accounts,
            hash,
        } => format!(
            "block {block}: VERIFIED — {accounts} accounts, keccak(rlp(bal)) == header ({hash})"
        ),
        BalProbe::NoHashInHeader { block, accounts } => format!(
            "block {block}: served ({accounts} accounts) but header has no BAL hash — cannot verify"
        ),
        BalProbe::Mismatch {
            block,
            computed,
            expected,
        } => format!(
            "block {block}: HASH MISMATCH — computed {computed}, header {expected}. Codec/spec drift; do not build on this."
        ),
        BalProbe::Missing(b) => format!("block {b}: {BAL_METHOD} returned null"),
        BalProbe::Error(e) => format!("error: {e}"),
    }
}

pub async fn run(ctx: &Ctx, rpc: Option<String>, age: u64) -> Result<()> {
    let rpc = ctx.cfg.rpc(rpc)?;
    let src = JsonRpcSource::new(&rpc);
    let r = src.probe(age).await?;
    let hash_field = r.head_fields.iter().any(|f| f == BAL_HASH_FIELD);

    if ctx.json {
        emit(&json!({
            "rpc": rpc,
            "client": r.client_version,
            "chainId": r.chain_id,
            "head": r.head,
            "headerHasBalHash": hash_field,
            "balMethod": BAL_METHOD,
            "q1Head": probe_json(&r.head_probe),
            "q2Old": probe_json(&r.old_probe),
            "q2Block1": probe_json(&r.earliest_probe),
            "proofWindow": match &r.proof_window {
                Ok(w) => json!(w),
                Err(e) => json!({ "error": e }),
            },
        }));
        return Ok(());
    }

    println!(
        "client:        {}",
        r.client_version.as_deref().unwrap_or("?")
    );
    println!(
        "chain id:      {}",
        r.chain_id.map(|c| c.to_string()).unwrap_or("?".into())
    );
    println!("head:          {}", r.head);
    println!(
        "header field:  {}",
        if hash_field {
            format!("`{BAL_HASH_FIELD}` present")
        } else {
            format!(
                "`{BAL_HASH_FIELD}` ABSENT — fields: {}",
                r.head_fields.join(", ")
            )
        }
    );
    println!("method:        {BAL_METHOD}");
    println!();
    println!("{:<16}{}", "Q1 head", probe_line(&r.head_probe));
    println!("{:<16}{}", "Q2 old", probe_line(&r.old_probe));
    println!("{:<16}{}", "Q2 block 1", probe_line(&r.earliest_probe));
    match &r.proof_window {
        Ok(0) => {
            println!("eth_getProof    window 0 — proofs only at head.");
            println!("                Early bootstrap (pre-value of a slot at its first change) is IMPOSSIBLE here:");
            println!("                such slots become BootstrapLost; history is complete from each slot's first change.");
            println!("                Own reth node: run with --rpc.eth-proof-window 128 (or more), or pass --backup-rpc.");
        }
        Ok(w) => println!("eth_getProof    window {w} blocks — pass `sync --proof-window {w}`"),
        Err(e) => println!("eth_getProof    NOT served — no bootstrap at all: {e}"),
    }
    Ok(())
}
