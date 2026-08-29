//! Ordering and uniqueness rules from the EIP. Violations are hard errors.

use crate::{AccountChanges, BlockAccessList, CodecError};
use std::cmp::Ordering;

fn strictly_ascending<T: Ord>(
    items: impl Iterator<Item = T>,
    what: &'static str,
) -> Result<(), CodecError> {
    let mut prev: Option<T> = None;
    for (pos, cur) in items.enumerate() {
        if let Some(p) = &prev {
            match p.cmp(&cur) {
                Ordering::Less => {}
                Ordering::Equal => return Err(CodecError::Duplicate { what, pos }),
                Ordering::Greater => return Err(CodecError::Ordering(what)),
            }
        }
        prev = Some(cur);
    }
    Ok(())
}

pub(crate) fn validate(bal: &BlockAccessList) -> Result<(), CodecError> {
    strictly_ascending(
        bal.accounts.iter().map(|a| a.address),
        "accounts by address",
    )?;
    for acc in &bal.accounts {
        validate_account(acc)?;
    }
    Ok(())
}

fn validate_account(acc: &AccountChanges) -> Result<(), CodecError> {
    strictly_ascending(
        acc.storage_changes.iter().map(|s| s.slot),
        "storage_changes by slot",
    )?;
    strictly_ascending(acc.storage_reads.iter().copied(), "storage_reads by slot")?;
    for s in &acc.storage_changes {
        if s.changes.is_empty() {
            return Err(CodecError::EmptySlotChanges {
                address: acc.address,
                slot: s.slot_b256(),
            });
        }
        strictly_ascending(
            s.changes.iter().map(|c| c.block_access_index),
            "storage changes by block_access_index",
        )?;
    }
    strictly_ascending(
        acc.balance_changes.iter().map(|c| c.block_access_index),
        "balance_changes by block_access_index",
    )?;
    strictly_ascending(
        acc.nonce_changes.iter().map(|c| c.block_access_index),
        "nonce_changes by block_access_index",
    )?;
    strictly_ascending(
        acc.code_changes.iter().map(|c| c.block_access_index),
        "code_changes by block_access_index",
    )?;
    // Both lists are sorted: linear merge detects any shared key.
    let (mut i, mut j) = (0, 0);
    while i < acc.storage_changes.len() && j < acc.storage_reads.len() {
        match acc.storage_changes[i].slot.cmp(&acc.storage_reads[j]) {
            Ordering::Less => i += 1,
            Ordering::Greater => j += 1,
            Ordering::Equal => return Err(CodecError::KeyInChangesAndReads(acc.address)),
        }
    }
    Ok(())
}
