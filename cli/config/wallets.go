package config

import (
	"fmt"
)

// Wallets contains the configuration of wallets.
type Wallets struct {
	// Default is the name of the default wallet.
	Default string `mapstructure:"default"`

	// All is a map of all configured wallets.
	All map[string]Wallet `mapstructure:",remain"`
}

// Validate performs config validation.
func (w *Wallets) Validate() error {
	// Make sure the default wallet actually exists.
	if _, exists := w.All[w.Default]; w.Default != "" && !exists {
		return fmt.Errorf("default wallet '%s' does not exist", w.Default)
	}

	// Make sure all wallets are valid.
	for name, wallet := range w.All {
		if name == "" {
			return fmt.Errorf("malformed wallet name '%s'", name)
		}

		if err := wallet.Validate(); err != nil {
			return fmt.Errorf("wallet '%s': %w", name, err)
		}
	}

	return nil
}

// WalletKind is the wallet kind.
type WalletKind string

const (
	WalletKindFile   WalletKind = "file"
	WalletKindLedger WalletKind = "ledger"
)

type Wallet struct {
	Description string     `mapstructure:"description"`
	Kind        WalletKind `mapstructure:"kind"`

	// Config contains kind-specific configuration for this wallet.
	Config map[string]interface{} `mapstructure:",remain"`
}

// Validate performs config validation.
func (w *Wallet) Validate() error {
	return nil
}
