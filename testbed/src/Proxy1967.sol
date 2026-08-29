// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

/// Minimal EIP-1967 proxy: implementation at the standard slot, owner-only
/// upgrade. Storage of the logic contract lives here, at the proxy address —
/// the case bal-layout resolves by reading this slot's history from the archive.
contract Proxy1967 {
    // bytes32(uint256(keccak256("eip1967.proxy.implementation")) - 1)
    bytes32 internal constant IMPL_SLOT =
        0x360894a13ba1a3210667c828492db98dca3e2076cc3735a920a3ca505d382bbc;
    // bytes32(uint256(keccak256("eip1967.proxy.admin")) - 1)
    bytes32 internal constant ADMIN_SLOT =
        0xb53127684a568b3173ae13b9f8a6016e243e63b6e8ee1178d6a717850b5d6103;

    event Upgraded(address indexed implementation);

    constructor(address impl) {
        assembly {
            sstore(IMPL_SLOT, impl)
            sstore(ADMIN_SLOT, caller())
        }
        emit Upgraded(impl);
    }

    function upgradeTo(address impl) external {
        address admin;
        assembly {
            admin := sload(ADMIN_SLOT)
        }
        require(msg.sender == admin, "not admin");
        assembly {
            sstore(IMPL_SLOT, impl)
        }
        emit Upgraded(impl);
    }

    fallback() external payable {
        assembly {
            let impl := sload(IMPL_SLOT)
            calldatacopy(0, 0, calldatasize())
            let ok := delegatecall(gas(), impl, 0, calldatasize(), 0, 0)
            returndatacopy(0, 0, returndatasize())
            switch ok
            case 0 { revert(0, returndatasize()) }
            default { return(0, returndatasize()) }
        }
    }

    receive() external payable {}
}
