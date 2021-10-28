package config

// Default is the default config that should be used in case no configuration file exists.
var Default = Config{
	Networks: Networks{
		Default: "mainnet",
		All: map[string]Network{
			// Mainnet network parameters.
			// See https://docs.oasis.dev/general/oasis-network/network-parameters.
			"mainnet": {
				ChainContext: "53852332637bacb61b91b6411ab4095168ba02a50be4c3f82448438826f23898",
				RPC:          "https://grpc.oasis.dev:443",
			},
			// Oasis Protocol Foundation Testnet parameters.
			// See https://docs.oasis.dev/general/foundation/testnet.
			"testnet": {
				ChainContext: "5ba68bc5e01e06f755c4c044dd11ec508e4c17f1faf40c0e67874388437a9e55",
				RPC:          "https://testnet.grpc.oasis.dev:443",
			},
		},
	},
}
