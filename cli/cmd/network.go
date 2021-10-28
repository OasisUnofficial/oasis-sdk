package cmd

import (
	"sort"

	"github.com/spf13/cobra"

	"github.com/oasisprotocol/oasis-sdk/cli/config"
	"github.com/oasisprotocol/oasis-sdk/cli/table"
)

var (
	networkCmd = &cobra.Command{
		Use:   "network",
		Short: "Manage network endpoints",
	}

	networkListCmd = &cobra.Command{
		Use:   "list",
		Short: "List configured networks",
		Run: func(cmd *cobra.Command, args []string) {
			cfg := config.Global()
			table := table.New()
			table.SetHeader([]string{"Name", "Chain Context", "RPC"})

			var output [][]string
			for name, net := range cfg.Networks.All {
				output = append(output, []string{
					name,
					net.ChainContext,
					net.RPC,
				})
			}

			// Sort output by name.
			sort.Slice(output, func(i, j int) bool {
				return output[i][0] < output[j][0]
			})

			table.AppendBulk(output)
			table.Render()
		},
	}

	networkAddCmd = &cobra.Command{
		Use:   "add [name] [chain-context] [rpc-endpoint]",
		Short: "Add a new network",
		Args:  cobra.ExactArgs(3),
		Run: func(cmd *cobra.Command, args []string) {
			cfg := config.Global()
			name, chainContext, rpc := args[0], args[1], args[2]

			err := cfg.Networks.Add(name, config.Network{
				ChainContext: chainContext,
				RPC:          rpc,
			})
			cobra.CheckErr(err)

			err = config.Save()
			cobra.CheckErr(err)
		},
	}

	networkRmCmd = &cobra.Command{
		Use:   "rm [name]",
		Short: "Remove an existing network",
		Args:  cobra.ExactArgs(1),
		Run: func(cmd *cobra.Command, args []string) {
			cfg := config.Global()
			name := args[0]

			err := cfg.Networks.Remove(name)
			cobra.CheckErr(err)

			err = config.Save()
			cobra.CheckErr(err)
		},
	}

	networkSetDefaultCmd = &cobra.Command{
		Use:   "set-default [name]",
		Short: "Sets the given network as the default network",
		Args:  cobra.ExactArgs(1),
		Run: func(cmd *cobra.Command, args []string) {
			cfg := config.Global()
			name := args[0]

			err := cfg.Networks.SetDefault(name)
			cobra.CheckErr(err)

			err = config.Save()
			cobra.CheckErr(err)
		},
	}
)

func init() {
	networkCmd.AddCommand(networkListCmd)
	networkCmd.AddCommand(networkAddCmd)
	networkCmd.AddCommand(networkRmCmd)
	networkCmd.AddCommand(networkSetDefaultCmd)
}
