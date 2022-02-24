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
//! Provides subxt connection, data source, output interfaces and abstractions.
//! 
//! Implements two interfaces: event subscription and a subxt wrapper. Both of these
//! build on the simplifying assumption that all errors are hidden away from callers.
//! This trades off control of behavior of errors in favor of simplicity and readability.
//! 
//! TODO(ASAP): create issues for all below:
//! TODO: retry logic needs to be improved - exponential backoff, cli options
//! TODO: integration tests for polkadot/parachains.
//! TODO: move prometheus into a module.
//! TODO: expose storage via event/api. Build a new event source such that new tools
//! can be built by combining existing ones by listening to storage update events.
use color_eyre::eyre::WrapErr;

use async_trait::async_trait;
use futures::future;
use log::{debug, error, info, warn};
use sp_core::H256;
use sp_runtime::traits::Lazy;
use subxt::{ClientBuilder, DefaultConfig, DefaultExtra};

use tokio::sync::{
	mpsc::{channel, Receiver, Sender},
	oneshot,
};

use crate::polkadot;
use std::collections::hash_map::{Entry, HashMap};

const MAX_MSG_QUEUE_SIZE: usize = 1024;
const RETRY_COUNT: usize = 3;
const RETRY_DELAY_MS: u64 = 100;

/// Abstracts all types of events that are processed by the system.
#[async_trait]
pub trait Event {
	type EventSource;

	fn source(&self) -> Self::EventSource;
}

#[async_trait]
pub trait EventStream {
	type Event;

	fn create_consumer(&mut self) -> EventConsumerInit<Self::Event>;
	/// Run the main event loop.
	async fn run(self, tasks: Vec<tokio::task::JoinHandle<()>>) -> color_eyre::Result<()>;
}

#[derive(Debug)]
pub struct Request {
	pub url: String,
	pub request_type: RequestType,
	pub response_sender: oneshot::Sender<Result>,
}

pub struct RequestExecutor {
	to_api: Sender<Request>,
}

impl RequestExecutor {
	pub fn new(to_api: Sender<Request>) -> Self {
		RequestExecutor { to_api }
	}

	pub async fn get_block_timestamp(
		&self,
		url: String,
		hash: Option<<DefaultConfig as subxt::Config>::Hash>,
	) -> Result {
		let (sender, receiver) = oneshot::channel::<crate::core::Result>();
		let request = Request { url, request_type: RequestType::GetBlockTimestamp(hash), response_sender: sender };
		self.to_api.send(request).await.expect("Channel closed");

		receiver.await.expect("Failed to fetch timestamp.")
	}

	pub async fn get_block_head(&self, url: String, hash: Option<<DefaultConfig as subxt::Config>::Hash>) -> Result {
		let (sender, receiver) = oneshot::channel::<crate::core::Result>();
		let request = Request { url, request_type: RequestType::GetHead(hash), response_sender: sender };
		self.to_api.send(request).await.expect("Channel closed");

		receiver.await.expect("Failed to fetch timestamp.")
	}
}

#[derive(Clone, Debug)]
pub enum RequestType {
	GetBlockTimestamp(Option<<DefaultConfig as subxt::Config>::Hash>),
	GetHead(Option<<DefaultConfig as subxt::Config>::Hash>),
}

#[derive(Debug)]
pub enum Response {
	GetBlockTimestampResponse(u64),
	GetHeadResponse(Option<<DefaultConfig as subxt::Config>::Header>),
}

#[derive(Debug)]
pub enum Error {
	SubxtError(subxt::BasicError),
	InternalError,
}

pub type Result = std::result::Result<Response, Error>;

/// Implementing the above for `subxt` connectivity wrappers.
/// Also provides an message based interface for subxt APIs.
pub struct SubxtWrapper {
	urls: Vec<String>,
	/// One sender per consumer per url.
	consumers: Vec<Vec<Sender<SubxtEvent>>>,
	api: Vec<Receiver<Request>>,
}

#[derive(Clone, Debug)]
pub enum SubxtEvent {
	NewHead(<DefaultConfig as subxt::Config>::Header),
}

impl Event for SubxtEvent {
	type EventSource = &'static str;

	fn source(&self) -> Self::EventSource {
		"subxt"
	}
}

#[derive(Debug)]
pub struct EventConsumerInit<Event> {
	// One per ws connection.
	update_channels: Vec<Receiver<Event>>,
	to_api: Sender<Request>,
}

impl<Event> Into<(Vec<Receiver<Event>>, Sender<Request>)> for EventConsumerInit<Event> {
	fn into(self) -> (Vec<Receiver<Event>>, Sender<Request>) {
		(self.update_channels, self.to_api)
	}
}

#[async_trait]
impl EventStream for SubxtWrapper {
	type Event = SubxtEvent;

	/// Create a new consumer of events. Returns consumer initialization data.
	fn create_consumer(&mut self) -> EventConsumerInit<Self::Event> {
		// Create API channel.
		let (to_api, api_rx) = channel(MAX_MSG_QUEUE_SIZE);
		let mut update_channels = Vec::new();

		// Create per ws update channels.
		for _ in 0..self.urls.len() {
			update_channels.push(channel(MAX_MSG_QUEUE_SIZE));
		}

		let (update_tx, update_channels): (Vec<Sender<Self::Event>>, Vec<Receiver<Self::Event>>) =
			update_channels.into_iter().unzip();

		// Keep per url update senders for this consumer.
		self.consumers.push(update_tx);
		self.api.push(api_rx);

		EventConsumerInit { update_channels, to_api }
	}

	async fn run(self, tasks: Vec<tokio::task::JoinHandle<()>>) -> color_eyre::Result<()> {
		let futures = self
			.consumers
			.into_iter()
			.map(|update_channels| Self::run_per_consumer(update_channels, self.urls.clone()))
			.collect::<Vec<_>>();

		let mut flat_futures = futures.into_iter().flat_map(|e| e).collect::<Vec<_>>();
		flat_futures.push(tokio::spawn(Self::setup_api_handler(self.api)));
		flat_futures.extend(tasks);
		future::try_join_all(flat_futures).await?;

		Ok(())
	}
}

async fn subxt_get_head(
	api: &polkadot::RuntimeApi<DefaultConfig, DefaultExtra<DefaultConfig>>,
	maybe_hash: Option<H256>,
) -> Result {
	Ok(Response::GetHeadResponse(api.client.rpc().header(maybe_hash).await.map_err(Error::SubxtError)?))
}

async fn subxt_get_block_ts(
	api: &polkadot::RuntimeApi<DefaultConfig, DefaultExtra<DefaultConfig>>,
	maybe_hash: Option<H256>,
) -> Result {
	Ok(Response::GetBlockTimestampResponse(api.storage().timestamp().now(maybe_hash).await.map_err(Error::SubxtError)?))
}

impl SubxtWrapper {
	pub fn new(urls: Vec<String>) -> SubxtWrapper {
		SubxtWrapper { urls, consumers: Vec::new(), api: Vec::new() }
	}

	// Spawn API handler tasks.
	async fn setup_api_handler(apis: Vec<Receiver<Request>>) {
		apis.into_iter().for_each(|api| {
			tokio::spawn(Self::api_handler_task(api));
		});
	}

	// Attempts to connect to websocket and returns an RuntimeApi instance if successful.
	async fn new_client_fn(url: String) -> Option<polkadot::RuntimeApi<DefaultConfig, DefaultExtra<DefaultConfig>>> {
		for _ in 0..RETRY_COUNT {
			match ClientBuilder::new()
				.set_url(url.clone())
				.build()
				.await
				.context("Error connecting to substrate node")
			{
				Ok(api) =>
					return Some(
						api.to_runtime_api::<polkadot::RuntimeApi<DefaultConfig, DefaultExtra<DefaultConfig>>>(),
					),
				Err(err) => {
					error!("[{}] Client error: {:?}", url, err);
					tokio::time::sleep(std::time::Duration::from_millis(RETRY_DELAY_MS)).await;
					continue
				},
			};
		}
		None
	}

	// Per consumer API thread.
	async fn api_handler_task(mut api: Receiver<Request>) {
		let mut connection_pool = HashMap::new();

		loop {
			if let Some(request) = api.recv().await {
				match connection_pool.entry(request.url.clone()) {
					Entry::Occupied(_) => (),
					Entry::Vacant(entry) => {
						let maybe_api = Self::new_client_fn(request.url.clone()).await;
						if let Some(api) = maybe_api {
							entry.insert(api);
						}
					},
				};

				let api = connection_pool.get(&request.url.clone());

				let response = if let Some(api) = api {
					match request.request_type {
						RequestType::GetBlockTimestamp(maybe_hash) => subxt_get_block_ts(api, maybe_hash).await,
						RequestType::GetHead(maybe_hash) => subxt_get_head(api, maybe_hash).await,
					}
				} else {
					Err(Error::InternalError)
				};

				let _ = request.response_sender.send(response);
			} else {
				// channel closed, exit loop.
				break
			}
		}
	}

	// Per consumer
	async fn run_per_node(update_channel: Sender<SubxtEvent>, url: String) {
		loop {
			match ClientBuilder::new()
				.set_url(url.clone())
				.build()
				.await
				.context("Error connecting to substrate node")
			{
				Ok(api) => {
					let api = api.to_runtime_api::<polkadot::RuntimeApi<DefaultConfig, DefaultExtra<DefaultConfig>>>();
					info!("[{}] Connected", url);
					match api.client.rpc().subscribe_blocks().await {
						Ok(mut sub) =>
							while let Some(ev_ctx) = sub.next().await {
								let header = ev_ctx.unwrap();
								info!("[{}] Block #{} imported ({:?})", url, header.number, header.hash());

								update_channel.send(SubxtEvent::NewHead(header.clone())).await.unwrap();
							},
						Err(err) => {
							error!("[{}] Disconnected ({:?}) ", url, err);
							// TODO (sometime): Add exponential backoff.
							tokio::time::sleep(std::time::Duration::from_millis(500)).await;
							info!("[{}] retrying connection ... ", url);
						},
					}
				},
				Err(err) => {
					error!("[{}] Disconnected ({:?}) ", url, err);
					// TODO (sometime): Add exponential backoff.
					tokio::time::sleep(std::time::Duration::from_millis(500)).await;
					info!("[{}] retrying connection ... ", url);
				},
			}
		}
	}

	// Sets up per websocket tasks to handle updates and reconnects on errors.
	fn run_per_consumer(
		update_channels: Vec<Sender<SubxtEvent>>,
		urls: Vec<String>,
	) -> Vec<tokio::task::JoinHandle<()>> {
		update_channels
			.into_iter()
			.zip(urls.into_iter())
			.map(|(update_channel, url)| tokio::spawn(Self::run_per_node(update_channel, url)))
			.collect()
	}
}
