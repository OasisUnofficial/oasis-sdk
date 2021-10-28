package cmd

import (
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"

	"github.com/adrg/xdg"
	"github.com/spf13/cobra"
	"github.com/spf13/viper"

	"github.com/oasisprotocol/oasis-sdk/cli/config"
)

var (
	cfgFile string

	rootCmd = &cobra.Command{
		Use:   "oasis",
		Short: "CLI for interacting with the Oasis network",
		// TODO: Long description.
	}
)

// Execute executes the root command.
func Execute() error {
	return rootCmd.Execute()
}

func initConfig() {
	if cfgFile != "" {
		// Use config file from the flag.
		viper.SetConfigFile(cfgFile)
	} else {
		const configFilename = "config.toml"
		configDir := filepath.Join(xdg.ConfigHome, "oasis")
		configPath := filepath.Join(configDir, configFilename)

		viper.AddConfigPath(configDir)
		viper.SetConfigType("toml")
		viper.SetConfigName(configFilename)

		// Ensure the configuration file exists.
		_ = os.MkdirAll(configDir, 0o700)
		if _, err := os.Stat(configPath); errors.Is(err, fs.ErrNotExist) {
			if _, err := os.Create(configPath); err != nil {
				cobra.CheckErr(fmt.Errorf("failed to create configuration file: %w", err))
			}

			// Populate the initial configuration file with defaults.
			config.ResetDefaults()
			_ = config.Save()
		}
	}

	_ = viper.ReadInConfig()

	// Load and validate global configuration.
	err := config.Load()
	cobra.CheckErr(err)
	err = config.Global().Validate()
	cobra.CheckErr(err)
}

func init() {
	cobra.OnInitialize(initConfig)

	rootCmd.PersistentFlags().StringVar(&cfgFile, "config", "", "config file to use")

	rootCmd.AddCommand(networkCmd)
}
