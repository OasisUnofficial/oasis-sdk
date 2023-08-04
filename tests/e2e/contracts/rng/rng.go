package rng

import (
	_ "embed"
	"encoding/hex"
	"fmt"
	"strings"

	ethABI "github.com/ethereum/go-ethereum/accounts/abi"
)

// CompiledHex is the compiled subcall contract in hex encoding.
//
//go:embed evm_rng.hex
var CompiledHex string

// Compiled is the compiled subcall contract.
var Compiled = func() []byte {
	contract, err := hex.DecodeString(strings.TrimSpace(CompiledHex))
	if err != nil {
		panic(fmt.Errorf("failed to decode contract: %w", err))
	}
	return contract
}()

//go:embed evm_rng.abi
var evmSubcallABIJson string

// ABI is the ABI of the subcall contract.
var ABI = func() ethABI.ABI {
	abi, err := ethABI.JSON(strings.NewReader(evmSubcallABIJson))
	if err != nil {
		panic(err)
	}
	return abi
}()
