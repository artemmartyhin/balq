# bal-source

Where BALs and proofs come from. Two traits — [`BalSource`](https://docs.rs/bal-source/latest/bal_source/trait.BalSource.html)
(head, headers, blocks with their BAL) and [`StateSource`](https://docs.rs/bal-source/latest/bal_source/trait.StateSource.html)
(`eth_getProof`) — plus a JSON-RPC implementation, Merkle-proof
verification against a header's `state_root`, and a primary/backup
combinator.

```rust,no_run
use bal_source::{BalSource, Fallback, JsonRpcSource};

# async fn demo() -> bal_source::Result<()> {
// Your full node decides what the chain is; an archive provider is asked
// only for BAL bodies it has pruned and proofs outside its state window.
// Both are verified the same way, so the backup adds reach, not trust.
let src = Fallback::new(
    JsonRpcSource::new("http://localhost:8545"),
    Some(JsonRpcSource::new("https://archive.example")),
);
let head = src.head().await?;
let block = src.block(head).await?;          // header + decoded, unverified BAL
block.bal.verify(block.header.block_access_list_hash.unwrap())?;
# Ok(()) }
```

`verify_account_proof` checks the account leaf against `state_root` and
every storage leaf against the account's storage root; a zero value is
proven by exclusion. `check_requested` refuses a proof that answers for
slots you did not ask about. The trie verifier runs under a panic guard,
so a crafted node cannot abort the process.

Transport: 30 s timeout, no redirects, 64 MiB response cap. The day-0
probe (`JsonRpcSource::probe`) reports whether an endpoint serves BALs,
for how old blocks, and how far back it serves proofs.

Part of [balq](https://github.com/artemmartyhin/balq).
