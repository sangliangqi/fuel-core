//! Utilities and helper methods for writing tests

use anyhow::{
    anyhow,
    Context,
};
use fuel_core_chain_config::ContractConfig;
use fuel_core_client::client::{
    pagination::{
        PageDirection,
        PaginationRequest,
    },
    types::{
        CoinType,
        TransactionStatus,
    },
    FuelClient,
};
use fuel_core_types::{
    fuel_crypto::PublicKey,
    fuel_tx::{
        ConsensusParameters,
        Contract,
        Finalizable,
        Input,
        Output,
        StorageSlot,
        Transaction,
        TransactionBuilder,
        TxId,
        UniqueIdentifier,
        UtxoId,
    },
    fuel_types::{
        bytes::SizedBytes,
        Address,
        AssetId,
    },
    fuel_vm::SecretKey,
};

use crate::config::{
    ClientConfig,
    SuiteConfig,
};

// The base amount needed to cover the cost of a simple transaction
pub const BASE_AMOUNT: u64 = 10_000;

pub struct TestContext {
    pub alice: Wallet,
    pub bob: Wallet,
    pub config: SuiteConfig,
}

impl TestContext {
    pub async fn new(config: SuiteConfig) -> Self {
        let alice_client = Self::new_client(config.endpoint.clone(), &config.wallet_a);
        let bob_client = Self::new_client(config.endpoint.clone(), &config.wallet_b);
        Self {
            alice: Wallet::new(config.wallet_a.secret, alice_client).await,
            bob: Wallet::new(config.wallet_b.secret, bob_client).await,
            config,
        }
    }

    fn new_client(default_endpoint: String, wallet: &ClientConfig) -> FuelClient {
        FuelClient::new(wallet.endpoint.clone().unwrap_or(default_endpoint)).unwrap()
    }
}

#[derive(Debug, Clone)]
pub struct Wallet {
    pub secret: SecretKey,
    pub address: Address,
    pub client: FuelClient,
    pub consensus_params: ConsensusParameters,
}

impl Wallet {
    pub async fn new(secret: SecretKey, client: FuelClient) -> Self {
        let public_key: PublicKey = (&secret).into();
        let address = Input::owner(&public_key);
        // get consensus params
        let consensus_params = client
            .chain_info()
            .await
            .expect("failed to get chain info")
            .consensus_parameters
            .into();

        Self {
            secret,
            address,
            client,
            consensus_params,
        }
    }

    /// returns the balance associated with a wallet
    pub async fn balance(&self, asset_id: Option<AssetId>) -> anyhow::Result<u64> {
        self.client
            .balance(&self.address, Some(&asset_id.unwrap_or_default()))
            .await
            .context("failed to retrieve balance")
    }

    /// Checks if wallet has a coin (regardless of spent status)
    pub async fn owns_coin(&self, utxo_id: UtxoId) -> anyhow::Result<bool> {
        let mut first_page = true;
        let mut results = vec![];

        while first_page || !results.is_empty() {
            first_page = false;
            results = self
                .client
                .coins(
                    &self.address,
                    None,
                    PaginationRequest {
                        cursor: None,
                        results: 100,
                        direction: PageDirection::Forward,
                    },
                )
                .await?
                .results;
            // check if page has the utxos we're looking for
            if results.iter().any(|coin| coin.utxo_id == utxo_id) {
                return Ok(true)
            }
        }

        Ok(false)
    }

    /// Creates the transfer transaction.
    pub async fn transfer_tx(
        &self,
        destination: Address,
        transfer_amount: u64,
        asset_id: Option<AssetId>,
    ) -> anyhow::Result<Transaction> {
        let asset_id = asset_id.unwrap_or_default();
        let total_amount = transfer_amount + BASE_AMOUNT;
        // select coins
        let coins = &self
            .client
            .coins_to_spend(&self.address, vec![(asset_id, total_amount, None)], None)
            .await?[0];

        // build transaction
        let mut tx = TransactionBuilder::script(Default::default(), Default::default());
        tx.gas_price(1);
        tx.gas_limit(BASE_AMOUNT);

        for coin in coins {
            if let CoinType::Coin(coin) = coin {
                tx.add_unsigned_coin_input(
                    self.secret,
                    coin.utxo_id,
                    coin.amount,
                    coin.asset_id,
                    Default::default(),
                    coin.maturity.into(),
                );
            }
        }
        tx.add_output(Output::Coin {
            to: destination,
            amount: transfer_amount,
            asset_id,
        });
        tx.add_output(Output::Change {
            to: self.address,
            amount: 0,
            asset_id,
        });
        tx.with_params(self.consensus_params.clone());

        Ok(tx.finalize_as_transaction())
    }

    /// Transfers coins from this wallet to another
    pub async fn transfer(
        &self,
        destination: Address,
        transfer_amount: u64,
        asset_id: Option<AssetId>,
    ) -> anyhow::Result<TransferResult> {
        let tx = self
            .transfer_tx(destination, transfer_amount, asset_id)
            .await?;
        let tx_id = tx.id(&self.consensus_params.chain_id);
        let status = self.client.submit_and_await_commit(&tx).await?;

        // we know the transferred coin should be output 0 from above
        let transferred_utxo = UtxoId::new(tx_id, 0);

        // get status and return the utxo id of transferred coin
        Ok(TransferResult {
            tx_id,
            transferred_utxo,
            success: matches!(status, TransactionStatus::Success { .. }),
            status,
        })
    }

    pub async fn deploy_contract(&self, config: ContractConfig) -> anyhow::Result<()> {
        let asset_id = AssetId::zeroed();
        let total_amount = BASE_AMOUNT;
        // select coins
        let coins = &self
            .client
            .coins_to_spend(&self.address, vec![(asset_id, total_amount, None)], None)
            .await?[0];

        let ContractConfig {
            contract_id,
            code: bytes,
            salt,
            state,
            ..
        } = config;
        let slots = state
            .unwrap_or_default()
            .into_iter()
            .map(|(key, value)| StorageSlot::new(key, value))
            .collect::<Vec<_>>();
        let state_root = Contract::initial_state_root(slots.iter());
        let mut tx = TransactionBuilder::create(bytes.into(), salt, slots);
        tx.gas_price(1);
        tx.gas_limit(BASE_AMOUNT);

        for coin in coins {
            if let CoinType::Coin(coin) = coin {
                tx.add_unsigned_coin_input(
                    self.secret,
                    coin.utxo_id,
                    coin.amount,
                    coin.asset_id,
                    Default::default(),
                    coin.maturity.into(),
                );
            }
        }
        tx.add_output(Output::ContractCreated {
            contract_id,
            state_root,
        });
        tx.add_output(Output::Change {
            to: self.address,
            amount: 0,
            asset_id,
        });

        let tx = tx.finalize();
        println!("The size of the transaction is {}", tx.serialized_size());

        let status = self
            .client
            .submit_and_await_commit(&tx.clone().into())
            .await?;

        // check status of contract deployment
        if let TransactionStatus::Failure { .. } | TransactionStatus::SqueezedOut { .. } =
            &status
        {
            return Err(anyhow!(format!("unexpected transaction status {status:?}")))
        }

        Ok(())
    }
}

pub struct TransferResult {
    pub tx_id: TxId,
    pub transferred_utxo: UtxoId,
    pub success: bool,
    pub status: TransactionStatus,
}
