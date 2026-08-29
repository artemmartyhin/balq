# Security

## Threat model in one paragraph

balq trusts its node for exactly one thing: which block is canonical.
Everything else is checked locally — `keccak(rlp(bal))` against the header,
Merkle proofs against `state_root`, proof responses against the slots that
were requested. A node that lies about BAL contents or proofs is detected;
a node that lies about the canonical chain is not (and cannot be, without a
consensus client).

Things that are **not** verified today and should be read as such:

- the block header itself (`keccak(rlp(header)) == blockHash`);
- anything stored with `Provenance::Imported` or `Provenance::Unverified`.

See `docs/SECURITY-AUDIT.md` for the threat model, the surfaces, and what was found and fixed.

## Reporting

Report vulnerabilities by e-mail to artemmartyhin@gmail.com. Please include a
reproduction (a journal row that the archive answers wrongly is ideal).
Expect an acknowledgement within a few days; fixes are published with a
CHANGELOG entry and a bumped version.
