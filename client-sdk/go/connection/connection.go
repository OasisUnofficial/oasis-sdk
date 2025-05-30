package connection

import (
	"context"
	"crypto/tls"
	"fmt"

	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials"
	"google.golang.org/grpc/credentials/insecure"

	"github.com/oasisprotocol/oasis-core/go/common"
	cmnGrpc "github.com/oasisprotocol/oasis-core/go/common/grpc"
	consensus "github.com/oasisprotocol/oasis-core/go/consensus/api"
	control "github.com/oasisprotocol/oasis-core/go/control/api"

	"github.com/oasisprotocol/oasis-sdk/client-sdk/go/client"
	"github.com/oasisprotocol/oasis-sdk/client-sdk/go/config"
	"github.com/oasisprotocol/oasis-sdk/client-sdk/go/modules/accounts"
	"github.com/oasisprotocol/oasis-sdk/client-sdk/go/modules/consensusaccounts"
	"github.com/oasisprotocol/oasis-sdk/client-sdk/go/modules/contracts"
	"github.com/oasisprotocol/oasis-sdk/client-sdk/go/modules/core"
	"github.com/oasisprotocol/oasis-sdk/client-sdk/go/modules/evm"
	"github.com/oasisprotocol/oasis-sdk/client-sdk/go/modules/rewards"
	"github.com/oasisprotocol/oasis-sdk/client-sdk/go/modules/rofl"
	"github.com/oasisprotocol/oasis-sdk/client-sdk/go/modules/roflmarket"
)

// RuntimeClient is a client.RuntimeClient augmented with commonly used modules.
type RuntimeClient struct {
	client.RuntimeClient

	Core              core.V1
	Accounts          accounts.V1
	Rewards           rewards.V1
	ConsensusAccounts consensusaccounts.V1
	Contracts         contracts.V1
	Evm               evm.V1
	ROFL              rofl.V1
	ROFLMarket        roflmarket.V1
}

// Connection is the general node connection interface.
type Connection interface {
	// Consensus returns an interface to the consensus layer.
	Consensus() consensus.Services

	// Control returns an interface to the node control layer.
	Control() control.NodeController

	// Runtime returns an interface to the given runtime.
	Runtime(pt *config.ParaTime) RuntimeClient
}

type connection struct {
	conn *grpc.ClientConn
}

func (c *connection) Consensus() consensus.Services {
	return consensus.NewServicesClient(c.conn)
}

func (c *connection) Control() control.NodeController {
	return control.NewNodeControllerClient(c.conn)
}

func (c *connection) Runtime(pt *config.ParaTime) RuntimeClient {
	var runtimeID common.Namespace
	if err := runtimeID.UnmarshalHex(pt.ID); err != nil {
		panic(err)
	}
	cli := client.New(c.conn, runtimeID)
	return RuntimeClient{
		RuntimeClient:     cli,
		Core:              core.NewV1(cli),
		Accounts:          accounts.NewV1(cli),
		Rewards:           rewards.NewV1(cli),
		ConsensusAccounts: consensusaccounts.NewV1(cli),
		Contracts:         contracts.NewV1(cli),
		Evm:               evm.NewV1(cli),
		ROFL:              rofl.NewV1(cli),
		ROFLMarket:        roflmarket.NewV1(cli),
	}
}

// Connect establishes a connection with the target network.
func Connect(ctx context.Context, net *config.Network) (Connection, error) {
	conn, err := ConnectNoVerify(ctx, net)
	if err != nil {
		return nil, err
	}

	// Request the chain domain separation context from the node and compare with local
	// configuration to reject mismatches early.
	chainContext, err := conn.Consensus().Core().GetChainContext(ctx)
	if err != nil {
		return nil, fmt.Errorf("failed to retrieve remote node's chain context: %s", err)
	}
	if chainContext != net.ChainContext {
		return nil, fmt.Errorf("remote node's chain context mismatch (expected: %s got: %s)", net.ChainContext, chainContext)
	}

	return conn, nil
}

// ConnectNoVerify establishes a connection with the target network,
// omitting the chain context check.
func ConnectNoVerify(_ context.Context, net *config.Network) (Connection, error) {
	var dialOpts []grpc.DialOption
	switch cmnGrpc.IsLocalAddress(net.RPC) {
	case true:
		// No TLS needed for local nodes.
		dialOpts = append(dialOpts, grpc.WithTransportCredentials(insecure.NewCredentials()))
	case false:
		// Configure TLS for non-local nodes.
		creds := credentials.NewTLS(&tls.Config{MinVersion: tls.VersionTLS12})
		dialOpts = append(dialOpts, grpc.WithTransportCredentials(creds))
	}

	conn, err := cmnGrpc.Dial(net.RPC, dialOpts...)
	if err != nil {
		return nil, err
	}

	return &connection{
		conn: conn,
	}, nil
}
