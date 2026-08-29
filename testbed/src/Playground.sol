// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

/// Test bed for balq. Every storage shape bal-layout must understand, in one
/// contract, plus a deterministic `poke` whose effects the caller can compute
/// off-chain — that is what makes `balq verify` possible without an archive
/// node: the sender knows the truth.
contract Playground {
    // slot 0
    uint256 public counter;
    // slot 1, packed: a @0 (16 bytes), b @16 (8 bytes), c @24 (1 byte)
    uint128 public a;
    uint64 public b;
    bool public c;
    // slot 2 (struct, packed into one word: lastTime @0, index @8)
    struct Totals {
        uint64 lastTime;
        uint192 index;
    }
    Totals public totals;
    // slot 3: keccak(key || 3)
    mapping(address => uint256) public balances;
    // slot 4: keccak(k2 || keccak(k1 || 4))
    mapping(address => mapping(uint256 => uint256)) public nested;
    // slot 5: length; data at keccak(5) + i
    uint256[] public items;
    // slot 6
    address public lastPoker;

    event Poked(uint256 seed, uint256 counter);

    /// Deterministic state transition from `seed`. Mirrors `expected()` in
    /// poke.mjs; change both together.
    function poke(uint256 seed) external {
        counter += 1;
        a = uint128(seed);
        b = uint64(seed >> 128);
        c = (seed & 1) == 1;
        totals.lastTime = uint64(block.timestamp);
        totals.index = uint192(counter * 1e18 + seed % 1000);
        balances[msg.sender] += seed % 10_000;
        nested[msg.sender][counter] = seed;
        if (items.length < 8) {
            items.push(seed);
        } else {
            items[counter % 8] = seed;
        }
        lastPoker = msg.sender;
        emit Poked(seed, counter);
    }

    /// No-op write: SSTORE with the unchanged value must land in
    /// storage_reads, not storage_changes.
    function touch() external {
        lastPoker = lastPoker;
    }
}
