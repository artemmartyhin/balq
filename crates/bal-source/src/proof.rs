//! Merkle-proof verification for bootstrap values. This is what turns an
//! `eth_getProof` answer from "the RPC said so" into a value anchored to a
//! block header's `state_root` — promise #2 for the only records that do not
//! come from a BAL.

use crate::AccountProof;
use alloy_primitives::{keccak256, B256, U256};
use alloy_trie::{proof::verify_proof, Nibbles, TrieAccount, EMPTY_ROOT_HASH};

/// Why a proof was rejected. Any of these means the value must not be stored.
#[derive(Debug, thiserror::Error)]
pub enum ProofError {
    /// The account leaf does not hash up to the header's state root.
    #[error("account proof invalid against state_root {root}: {reason}")]
    Account {
        /// State root the proof was checked against.
        root: B256,
        /// Trie verifier's reason.
        reason: String,
    },
    /// A storage leaf does not hash up to the account's storage root.
    #[error("storage proof for slot {slot} invalid against storage_root {root}: {reason}")]
    Storage {
        /// Slot whose proof failed.
        slot: B256,
        /// Storage root the proof was checked against.
        root: B256,
        /// Trie verifier's reason.
        reason: String,
    },
    /// The response omitted a requested slot.
    #[error("proof did not include slot {0}")]
    MissingSlot(B256),
    /// The response carried a slot that was not requested.
    #[error("proof included unrequested slot {0}")]
    UnexpectedSlot(B256),
}

/// Check that a proof answers exactly the `requested` slots (any order, no
/// extras). A node that answers for other slots must not be able to plant
/// values under the wrong key.
pub fn check_requested(requested: &[B256], proof: &AccountProof) -> Result<(), ProofError> {
    let got: std::collections::HashSet<B256> = proof.storage_proofs.iter().map(|p| p.key).collect();
    for r in requested {
        if !got.contains(r) {
            return Err(ProofError::MissingSlot(*r));
        }
    }
    let want: std::collections::HashSet<B256> = requested.iter().copied().collect();
    for g in &got {
        if !want.contains(g) {
            return Err(ProofError::UnexpectedSlot(*g));
        }
    }
    Ok(())
}

/// Verify the account leaf against `state_root`, then every storage proof
/// against the account's `storage_hash`. Returns `(slot, value)` pairs in the
/// order they appear in the proof. A zero value is proven by *absence*
/// (exclusion proof), which is exactly the distinction promise #3 needs.
pub fn verify_account_proof(
    state_root: B256,
    proof: &AccountProof,
) -> Result<Vec<(B256, U256)>, ProofError> {
    let account = TrieAccount {
        nonce: proof.nonce,
        balance: proof.balance,
        storage_root: proof.storage_hash,
        code_hash: proof.code_hash,
    };
    // A non-existent account is proven by exclusion; its storage root is empty.
    let account_exists = !(proof.nonce == 0
        && proof.balance.is_zero()
        && proof.storage_hash == EMPTY_ROOT_HASH
        && proof.code_hash == alloy_primitives::KECCAK256_EMPTY);
    let expected_account = account_exists.then(|| alloy_rlp::encode(account));
    guarded_verify(
        state_root,
        Nibbles::unpack(keccak256(proof.address)),
        expected_account,
        &proof.account_proof,
    )
    .map_err(|reason| ProofError::Account {
        root: state_root,
        reason,
    })?;

    let mut out = Vec::with_capacity(proof.storage_proofs.len());
    for sp in &proof.storage_proofs {
        let expected = (!sp.value.is_zero()).then(|| alloy_rlp::encode(sp.value));
        guarded_verify(
            proof.storage_hash,
            Nibbles::unpack(keccak256(sp.key)),
            expected,
            &sp.proof,
        )
        .map_err(|reason| ProofError::Storage {
            slot: sp.key,
            root: proof.storage_hash,
            reason,
        })?;
        out.push((sp.key, sp.value));
    }
    Ok(out)
}

/// `verify_proof` with a panic guard. The trie verifier has at least one
/// `unreachable!()` reachable from a crafted node (an in-place extension
/// whose child is an in-place leaf); a node must not be able to abort the
/// process, so a panic is reported as an invalid proof instead.
fn guarded_verify(
    root: B256,
    key: Nibbles,
    expected: Option<Vec<u8>>,
    nodes: &[alloy_primitives::Bytes],
) -> Result<(), String> {
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        verify_proof(root, key, expected, nodes.iter())
    }));
    match r {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err("malformed proof node (verifier panicked)".into()),
    }
}
