use rofl_app_core::prelude::*;

/// Address where the oracle contract is deployed.
const ORACLE_CONTRACT_ADDRESS: &str = "0x5FbDB2315678afecb367f032d93F642f64180aa3"; // TODO: Replace with your contract address.

struct OracleApp;

#[async_trait]
impl App for OracleApp {
    /// Application version.
    const VERSION: Version = sdk::version_from_cargo!();

    async fn run(self: Arc<Self>, _env: Environment<Self>) {
        // We are running now!
        println!("Hello ROFL world!");
    }

    async fn on_runtime_block(self: Arc<Self>, env: Environment<Self>, _round: u64) {
        // This gets called for each runtime block. It will not be called again until the previous
        // invocation returns and if invocation takes multiple blocks to run, those blocks will be
        // skipped.
        if let Err(err) = self.run_oracle(env).await {
            println!("Failed to submit observation: {:?}", err);
        }
    }
}

impl OracleApp {
    /// Fetch stuff from remote service via HTTPS and publish it on chain.
    async fn run_oracle(self: Arc<Self>, env: Environment<Self>) -> Result<()> {
        let observation = tokio::task::spawn_blocking(move || -> Result<_> {
            // Request some data from Coingecko API.
            let rsp: serde_json::Value = rofl_utils::https::agent()
                .get("https://www.binance.com/api/v3/ticker/price?symbol=ROSEUSDT")
                .call()?
                .body_mut()
                .read_json()?;
            // Extract price and convert to integer.
            let price = rsp
                .pointer("/price")
                .ok_or(anyhow::anyhow!("price not available"))?
                .as_str()
                .unwrap()
                .parse::<f64>()?;
            let price = (price * 1_000_000.0) as u128;

            Ok(price)
        })
        .await??;

        // Prepare the oracle contract call.
        let mut tx = self.new_transaction(
            "evm.Call",
            module_evm::types::Call {
                address: ORACLE_CONTRACT_ADDRESS.parse().unwrap(),
                value: 0.into(),
                data: [
                    ethabi::short_signature("submitObservation", &[ethabi::ParamType::Uint(128)])
                        .to_vec(),
                    ethabi::encode(&[ethabi::Token::Uint(observation.into())]),
                ]
                .concat(),
            },
        );
        tx.set_fee_gas(200_000);

        // Submit observation on chain.
        env.client().sign_and_submit_tx(env.signer(), tx).await?;

        Ok(())
    }
}

fn main() {
    OracleApp.start();
}
