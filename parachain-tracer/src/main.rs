// Copyright 2023 Parity Technologies (UK) Ltd.
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
//! This is currently a work in progress, but there is a plan in place.
//! At this stage we can only use on-chain data to derive parachain metrics,
//! but later we can expand to use off-chain data as well like gossip.
//!
//! Features:
//! - backing and availability health metrics for all parachains
//! - TODO: backing group information - validator addresses
//! - TODO: parachain block times measured in relay chain blocks
//! - TODO: parachain XCM throughput
//! - TODO: parachain code size
//!
//! The CLI interface is useful for debugging/diagnosing issues with the parachain block pipeline.
//! Soon: CI integration also supported via Prometheus metrics exporting.

use clap::Parser;
use colored::Colorize;
use crossterm::style::Stylize;
use futures::{future, stream::FuturesUnordered, StreamExt};
use itertools::Itertools;
use log::{error, info, warn};
use polkadot_introspector_essentials::{
	api::subxt_wrapper::RequestExecutor,
	collector,
	collector::{Collector, CollectorOptions, CollectorStorageApi, CollectorUpdateEvent},
	consumer::{EventConsumerInit, EventStream},
	init,
	subxt_subscription::{SubxtEvent, SubxtSubscription},
	types::H256,
	utils::RetryOptions,
};
use polkadot_introspector_priority_channel::{channel_with_capacities, Receiver, Sender};
use prometheus::{Metrics, ParachainTracerPrometheusOptions};
use std::{collections::HashMap, default::Default, ops::DerefMut};
use tokio::{
	signal,
	sync::{broadcast, broadcast::Sender as BroadcastSender},
};
use tracker::{ParachainBlockTracker, SubxtTracker};

mod progress;
mod prometheus;
mod stats;
mod tracker;

#[derive(Clone, Debug, Parser, Default)]
#[clap(rename_all = "kebab-case")]
pub(crate) enum ParachainTracerMode {
	/// CLI chart mode.
	#[default]
	Cli,
	/// Prometheus endpoint mode.
	Prometheus(ParachainTracerPrometheusOptions),
}

#[derive(Clone, Debug, Parser)]
#[clap(author, version, about = "Observe parachain state")]
pub(crate) struct ParachainTracerOptions {
	/// Web-Socket URLs of a relay chain node.
	#[clap(name = "ws", long, value_delimiter = ',', default_value = "wss://rpc.polkadot.io:443")]
	pub node: String,
	/// Parachain id.
	#[clap(long, conflicts_with = "all")]
	para_id: Vec<u32>,
	#[clap(long, conflicts_with = "para_id", default_value = "false")]
	all: bool,
	/// Run for a number of blocks then stop.
	#[clap(name = "blocks", long)]
	block_count: Option<u32>,
	/// The number of last blocks with missing slots to display
	#[clap(long = "last-skipped-slot-blocks", default_value = "10")]
	pub last_skipped_slot_blocks: usize,
	/// Evict a stalled parachain after this amount of skipped blocks
	#[clap(long, default_value = "256")]
	max_parachain_stall: u32,
	/// Defines subscription mode
	#[clap(flatten)]
	collector_opts: CollectorOptions,
	/// Mode of running - CLI/Prometheus. Default or no subcommand means `CLI` mode.
	#[clap(subcommand)]
	mode: Option<ParachainTracerMode>,
	#[clap(flatten)]
	pub verbose: init::VerbosityOptions,
	#[clap(flatten)]
	pub retry: RetryOptions,
}

#[derive(Clone)]
pub(crate) struct ParachainTracer {
	opts: ParachainTracerOptions,
	retry: RetryOptions,
	node: String,
	metrics: Metrics,
}

impl ParachainTracer {
	pub(crate) fn new(mut opts: ParachainTracerOptions) -> color_eyre::Result<Self> {
		// This starts the both the storage and subxt APIs.
		let node = opts.node.clone();
		let retry = opts.retry.clone();
		opts.mode = opts.mode.or(Some(ParachainTracerMode::Cli));

		Ok(ParachainTracer { opts, node, metrics: Default::default(), retry })
	}

	/// Spawn the UI and subxt tasks and return their futures.
	pub(crate) async fn run(
		mut self,
		shutdown_tx: &BroadcastSender<()>,
		consumer_config: EventConsumerInit<SubxtEvent>,
	) -> color_eyre::Result<Vec<tokio::task::JoinHandle<()>>> {
		let mut output_futures = vec![];

		if let Some(ParachainTracerMode::Prometheus(ref prometheus_opts)) = self.opts.mode {
			self.metrics = prometheus::run_prometheus_endpoint(prometheus_opts).await?;
		}

		let mut collector =
			Collector::new(self.opts.node.as_str(), self.opts.collector_opts.clone(), self.retry.clone());
		collector.spawn(shutdown_tx).await?;
		if let Err(e) = print_host_configuration(self.opts.node.as_str(), &mut collector.executor()).await {
			warn!("Cannot get host configuration");
			return Err(e)
		}

		println!(
			"{} will trace {} on {}\n{}",
			"Parachain Tracer".to_string().purple(),
			if self.opts.all {
				"all parachain(s)".to_string()
			} else {
				format!("parachain(s) {}", self.opts.para_id.iter().join(","))
			},
			&self.node,
			"-----------------------------------------------------------------------"
				.to_string()
				.bold()
		);

		if self.opts.all {
			let from_collector = collector.subscribe_broadcast_updates().await?;
			output_futures.push(tokio::spawn(ParachainTracer::watch_node_broadcast(
				self.clone(),
				from_collector,
				collector.api(),
			)));
		} else {
			for para_id in self.opts.para_id.iter() {
				let from_collector = collector.subscribe_parachain_updates(*para_id).await?;
				output_futures.push(ParachainTracer::watch_node_for_parachain(
					self.clone(),
					from_collector,
					*para_id,
					collector.api(),
				));
			}
		}

		let consumer_channels: Vec<Receiver<SubxtEvent>> = consumer_config.into();
		let _collector_fut = collector
			.run_with_consumer_channel(consumer_channels.into_iter().next().unwrap())
			.await;

		Ok(output_futures)
	}

	// This is the main loop for our subxt subscription.
	// Follows the stream of events and updates the application state.
	fn watch_node_for_parachain(
		self,
		from_collector: Receiver<CollectorUpdateEvent>,
		para_id: u32,
		api_service: CollectorStorageApi,
	) -> tokio::task::JoinHandle<()> {
		// The subxt API request executor.
		let executor = api_service.subxt();
		let mut tracker = tracker::SubxtTracker::new(
			para_id,
			self.node.as_str(),
			executor,
			api_service,
			self.opts.last_skipped_slot_blocks,
		);

		let metrics = self.metrics.clone();
		let is_cli = matches!(&self.opts.mode, Some(ParachainTracerMode::Cli));

		tokio::spawn(async move {
			loop {
				match from_collector.recv().await {
					Ok(update_event) => match update_event {
						CollectorUpdateEvent::NewHead(new_head) =>
							for relay_fork in &new_head.relay_parent_hashes {
								process_tracker_update(
									&mut tracker,
									*relay_fork,
									new_head.relay_parent_number,
									&metrics,
									is_cli,
								)
								.await;
							},
						CollectorUpdateEvent::NewSession(idx) => {
							tracker.new_session(idx).await;
						},
						CollectorUpdateEvent::Termination => {
							info!("collector is terminating");
							break
						},
					},
					Err(_) => {
						info!("Input channel has been closed");
						break
					},
				}
			}

			let stats = tracker.summary();
			if is_cli {
				print!("{}", stats);
			} else {
				info!("{}", stats);
			}
		})
	}

	async fn watch_node_broadcast(
		self,
		mut from_collector: Receiver<CollectorUpdateEvent>,
		api_service: CollectorStorageApi,
	) {
		let mut trackers = HashMap::new();
		// Used to track last block seen in parachain to evict stalled parachains
		// Another approach would be a BtreeMap indexed by a block number, but
		// for the practical reasons we are fine to do a hash map scan on each head.
		let mut last_blocks: HashMap<u32, u32> = HashMap::new();
		let mut best_known_block: u32 = 0;
		let max_stall = self.opts.max_parachain_stall;
		let mut futures = FuturesUnordered::new();

		loop {
			tokio::select! {
				message = from_collector.next() => {
					match message {
						Some(update_event) => match update_event {
							CollectorUpdateEvent::NewHead(new_head) => {
								let para_id = new_head.para_id;
								let last_known_block = new_head.relay_parent_number;

								let to_tracker = trackers.entry(para_id).or_insert_with(|| {
									let (tx, rx) = channel_with_capacities(collector::COLLECTOR_NORMAL_CHANNEL_CAPACITY, 1);
									futures.push(ParachainTracer::watch_node_for_parachain(self.clone(), rx, para_id, api_service.clone()));
									info!("Added tracker for parachain {}", para_id);

									tx
								});
								to_tracker.send(CollectorUpdateEvent::NewHead(new_head.clone())).await.unwrap();
								// Update last block number
								let _ = std::mem::replace(
									last_blocks.entry(para_id).or_insert(last_known_block).deref_mut(),
									last_known_block,
								);

								if last_known_block > best_known_block {
									best_known_block = last_known_block;
									evict_stalled(&mut trackers, &mut last_blocks, max_stall);
								}
							},
							CollectorUpdateEvent::NewSession(idx) =>
								for to_tracker in trackers.values_mut() {
									to_tracker.send(CollectorUpdateEvent::NewSession(idx)).await.unwrap();
								},
							CollectorUpdateEvent::Termination => {
								info!("Received termination event");
								break;
							},
						},
						None => {
							info!("Input channel has been closed");
							break;
						},
					};
				},
				Some(_) = futures.next() => {},
				else => break,
			}
		}

		// Drop all trackers channels to initiate their termination
		trackers.clear();
		future::try_join_all(futures).await.unwrap();
	}
}

async fn process_tracker_update(
	tracker: &mut SubxtTracker,
	relay_hash: H256,
	relay_parent_number: u32,
	metrics: &Metrics,
	is_cli: bool,
) {
	match tracker.inject_block(relay_hash, relay_parent_number).await {
		Ok(_) => {
			if let Some(progress) = tracker.progress(metrics) {
				if is_cli {
					println!("{}", progress);
				}
			}
			tracker.maybe_reset_state();
		},
		Err(e) => {
			error!("error occurred when processing block {}: {:?}", relay_hash, e)
		},
	}
}

fn evict_stalled(
	trackers: &mut HashMap<u32, Sender<CollectorUpdateEvent>>,
	last_blocks: &mut HashMap<u32, u32>,
	max_stall: u32,
) {
	let max_block = *last_blocks.values().max().unwrap_or(&0_u32);
	let to_evict: Vec<u32> = last_blocks
		.iter()
		.filter(|(_, last_block)| max_block - *last_block > max_stall)
		.map(|(para_id, _)| *para_id)
		.collect();
	for para_id in to_evict {
		let last_seen = last_blocks.remove(&para_id).expect("checked previously, qed");
		info!("evicting tracker for parachain {}, stalled for {} blocks", para_id, max_block - last_seen);
		trackers.remove(&para_id);
	}
}

async fn print_host_configuration(url: &str, executor: &mut RequestExecutor) -> color_eyre::Result<()> {
	let conf = executor.get_host_configuration(url).await?;
	println!("Host configuration for {}:", url.to_owned().bold());
	println!(
		"\t👀 Max validators: {} / {} per core",
		format!("{}", conf.max_validators.unwrap_or(0)).bold(),
		format!("{}", conf.max_validators_per_core.unwrap_or(0)).bright_magenta(),
	);
	println!("\t👍 Needed approvals: {}", format!("{}", conf.needed_approvals).bold(),);
	println!("\t🥔 No show slots: {}", format!("{}", conf.no_show_slots).bold(),);
	println!("\t⏳ Delay tranches: {}", format!("{}", conf.n_delay_tranches).bold(),);
	Ok(())
}

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
	let opts = ParachainTracerOptions::parse();
	init::init_cli(&opts.verbose)?;

	let mut core = SubxtSubscription::new(vec![opts.node.clone()], opts.retry.clone());
	let consumer_init = core.create_consumer();
	let (shutdown_tx, _) = broadcast::channel(1);

	match ParachainTracer::new(opts)?.run(&shutdown_tx, consumer_init).await {
		Ok(mut futures) => {
			let shutdown_tx_cpy = shutdown_tx.clone();
			futures.push(tokio::spawn(async move {
				signal::ctrl_c().await.unwrap();
				let _ = shutdown_tx_cpy.send(());
			}));
			core.run(futures, shutdown_tx.clone()).await?
		},
		Err(err) => error!("FATAL: cannot start parachain tracer: {}", err),
	}

	Ok(())
}