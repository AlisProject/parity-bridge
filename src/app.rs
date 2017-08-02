use std::path::{Path, PathBuf};
use std::io;
use std::time::Duration;
use std::sync::Arc;
use futures::{future, Future};
use tokio_core::reactor::{Core, Handle};
use web3::{Web3, Transport};
use web3::transports::ipc::Ipc;
use web3::types::TransactionRequest;
use error::{Error, ErrorKind, ResultExt};
use config::Config;
use database::{Database, BlockchainState};
use api;

pub struct App<T> where T: Transport {
	event_loop: Core,
	config: Config,
	database_path: PathBuf,
	connections: Connections<T>,
}

pub struct Connections<T> where T: Transport {
	mainnet: T,
	testnet: T,
}

impl Connections<Ipc> {
	pub fn new_ipc<P: AsRef<Path>>(handle: &Handle, mainnet: P, testnet: P) -> Result<Self, Error> {
		let mainnet = Ipc::with_event_loop(mainnet, handle).chain_err(|| "Cannot connect to mainnet node ipc")?;
		let testnet = Ipc::with_event_loop(testnet, handle).chain_err(|| "Cannot connect to testnet node ipc")?;

		let result = Connections {
			mainnet,
			testnet,
		};
		Ok(result)
	}
}

impl App<Ipc> {
	pub fn new_ipc<P: AsRef<Path>>(config: Config, database_path: P) -> Result<Self, Error> {
		let event_loop = Core::new()?;
		let connections = Connections::new_ipc(&event_loop.handle(), &config.mainnet.ipc, &config.testnet.ipc)?;
		let result = App {
			event_loop,
			config,
			database_path: database_path.as_ref().to_path_buf(),
			connections,
		};
		Ok(result)
	}

	pub fn ensure_deployed<'a>(&'a self) -> Box<Future<Item = Database, Error = Error> + 'a> {
		let database_path = self.database_path.clone();
		match Database::load(&database_path).map_err(ErrorKind::from) {
			Ok(database) => future::result(Ok(database)).boxed(),
			Err(ErrorKind::Io(ref err)) if err.kind() == io::ErrorKind::NotFound => {
				let future = self.deploy().and_then(|database| {
					database.save(database_path)?;
					Ok(database)
				});
				Box::new(future)
			},
			Err(err) => future::result(Err(err.into())).boxed(),
		}
	}

	pub fn deploy<'a>(&'a self) -> Box<Future<Item = Database, Error = Error> + 'a> {
		let main_tx_request = TransactionRequest {
			from: self.config.mainnet.account.parse().expect("TODO: verify toml earlier"),
			to: None,
			gas: Some(self.config.mainnet.deploy_tx.gas.into()),
			gas_price: Some(self.config.mainnet.deploy_tx.gas_price.into()),
			value: Some(self.config.mainnet.deploy_tx.value.into()),
			data: Some(include_bytes!("../contracts/EthereumBridge.bin").to_vec().into()),
			nonce: None,
			condition: None,
		};

		let test_tx_request = TransactionRequest {
			from: self.config.testnet.account.parse().expect("TODO: verify toml earlier"),
			to: None,
			gas: Some(self.config.testnet.deploy_tx.gas.into()),
			gas_price: Some(self.config.testnet.deploy_tx.gas_price.into()),
			value: Some(self.config.testnet.deploy_tx.value.into()),
			data: Some(include_bytes!("../contracts/KovanBridge.bin").to_vec().into()),
			nonce: None,
			condition: None,
		};


		let main_future = api::send_transaction_with_confirmation(&self.connections.mainnet, main_tx_request, self.config.mainnet.poll_interval, self.config.mainnet.required_confirmations);
		let test_future = api::send_transaction_with_confirmation(&self.connections.testnet, test_tx_request, self.config.testnet.poll_interval, self.config.testnet.required_confirmations);

		let deploy = main_future.join(test_future)
			.map(|(main_receipt, test_receipt)| {
				Database {
					mainnet: BlockchainState {
						deploy_block_number: main_receipt.block_number.low_u64(),
						last_block_number: main_receipt.block_number.low_u64(),
						// TODO: fix to_string
						bridge_contract_address: main_receipt.contract_address.expect("contract creation receipt must have an address; qed").to_string(),
					},
					testnet: BlockchainState {
						deploy_block_number: test_receipt.block_number.low_u64(),
						last_block_number: test_receipt.block_number.low_u64(),
						// TODO: fix to_string
						bridge_contract_address: test_receipt.contract_address.expect("contract creation receipt must have an address; qed").to_string(),
					}
				}
			})
			.map_err(ErrorKind::Web3)
			.map_err(Error::from);

		Box::new(deploy)
	}
}

