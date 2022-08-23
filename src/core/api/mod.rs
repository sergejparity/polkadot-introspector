// Copyright 2022 Parity Technologies (UK) Ltd.
// This file is part of polkadot-introspector.
//
// polkadot-introspector is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// polkadot-introspector is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with polkadot-introspector.  If not, see <http://www.gnu.org/licenses/>.
//
#![allow(dead_code)]
use crate::core::{RecordsStorageConfig, MAX_MSG_QUEUE_SIZE};
use tokio::sync::mpsc::{channel, Sender};

mod storage;
mod subxt_wrapper;

pub use subxt_wrapper::ValidatorIndex;

// Provides access to subxt and storage APIs, more to come.
#[derive(Clone)]
pub struct ApiService {
	subxt_tx: Sender<subxt_wrapper::Request>,
	storage_tx: Sender<storage::Request>,
}

impl ApiService {
	pub fn new_with_storage(storage_config: RecordsStorageConfig) -> ApiService {
		let (subxt_tx, subxt_rx) = channel(MAX_MSG_QUEUE_SIZE);
		let (storage_tx, storage_rx) = channel(MAX_MSG_QUEUE_SIZE);

		tokio::spawn(subxt_wrapper::api_handler_task(subxt_rx));
		tokio::spawn(storage::api_handler_task(storage_rx, storage_config));

		Self { subxt_tx, storage_tx }
	}

	pub fn storage(&self) -> storage::RequestExecutor {
		storage::RequestExecutor::new(self.storage_tx.clone())
	}

	pub fn subxt(&self) -> subxt_wrapper::RequestExecutor {
		subxt_wrapper::RequestExecutor::new(self.subxt_tx.clone())
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::core::*;
	use subxt::sp_runtime::traits::{BlakeTwo256, Hash};

	// Using this node as it allows for more parallel conenctions.
	const RPC_NODE_URL: &str = "wss://kusama-try-runtime-node.parity-chains.parity.io:443";

	#[tokio::test]
	async fn basic_storage_test() {
		let api = ApiService::new_with_storage(RecordsStorageConfig { max_blocks: 10 });
		let storage = api.storage();
		let key = BlakeTwo256::hash_of(&100);
		storage
			.storage_write(key, StorageEntry::new_onchain(1.into(), "some data"))
			.await;
		let value = storage.storage_read(key).await.unwrap();
		assert_eq!(value.into_inner::<String>(), "some data");
	}

	#[tokio::test]
	async fn basic_subxt_test() {
		let api = ApiService::new_with_storage(RecordsStorageConfig { max_blocks: 10 });
		let subxt = api.subxt();

		let head = subxt.get_block_head(RPC_NODE_URL.into(), None).await.unwrap();
		let timestamp = subxt.get_block_timestamp(RPC_NODE_URL.into(), Some(head.hash())).await;
		let _block = subxt.get_block(RPC_NODE_URL.into(), Some(head.hash())).await.unwrap();
		assert!(timestamp > 0);
	}

	#[tokio::test]
	async fn extract_parainherent_data() {
		let api = ApiService::new_with_storage(RecordsStorageConfig { max_blocks: 1 });
		let subxt = api.subxt();

		subxt
			.extract_parainherent_data(RPC_NODE_URL.into(), None)
			.await
			.expect("Inherent data must be present");
	}

	#[tokio::test]
	async fn get_scheduled_paras() {
		let api = ApiService::new_with_storage(RecordsStorageConfig { max_blocks: 1 });
		let subxt = api.subxt();

		let head = subxt.get_block_head(RPC_NODE_URL.into(), None).await.unwrap();

		assert!(!subxt.get_scheduled_paras(RPC_NODE_URL.into(), head.hash()).await.is_empty())
	}

	#[tokio::test]
	async fn get_occupied_cores() {
		let api = ApiService::new_with_storage(RecordsStorageConfig { max_blocks: 1 });
		let subxt = api.subxt();

		let head = subxt.get_block_head(RPC_NODE_URL.into(), None).await.unwrap();

		assert!(!subxt.get_occupied_cores(RPC_NODE_URL.into(), head.hash()).await.is_empty())
	}

	#[tokio::test]
	async fn get_backing_groups() {
		let api = ApiService::new_with_storage(RecordsStorageConfig { max_blocks: 1 });
		let subxt = api.subxt();

		let head = subxt.get_block_head(RPC_NODE_URL.into(), None).await.unwrap();

		assert!(!subxt.get_backing_groups(RPC_NODE_URL.into(), head.hash()).await.is_empty())
	}
}
