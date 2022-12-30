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

use crate::pc::tracker::DisputesOutcome;
use clap::Parser;
use color_eyre::Result;
use prometheus_endpoint::{
	prometheus::{HistogramOpts, HistogramVec, IntCounterVec, IntGaugeVec, Opts},
	Registry,
};
use std::net::ToSocketAddrs;

#[derive(Clone, Debug, Parser, Default)]
#[clap(rename_all = "kebab-case")]
pub struct ParachainCommanderPrometheusOptions {
	/// Address to bind Prometheus listener
	#[clap(short = 'a', long = "address", default_value = "0.0.0.0")]
	address: String,
	/// Port to bind Prometheus listener
	#[clap(short = 'p', long = "port", default_value = "65432")]
	port: u16,
}

#[derive(Clone)]
struct DisputesMetrics {
	/// Number of candidates disputed.
	disputed_count: IntCounterVec,
	concluded_valid: IntCounterVec,
	concluded_invalid: IntCounterVec,
	/// Average count of validators that voted against supermajority
	/// Average resolution time in blocks
	resolution_time: HistogramVec,
}

#[derive(Clone)]
struct MetricsInner {
	/// Number of backed candidates.
	backed_count: IntCounterVec,
	/// Number of skipped slots, where no candidate was backed and availability core
	/// was free.
	skipped_slots: IntCounterVec,
	/// Number of candidates included.
	included_count: IntCounterVec,
	/// Disputes stats
	disputes_stats: DisputesMetrics,
	/// Block time measurements for relay parent blocks
	block_times: HistogramVec,
	/// Number of slow availability events.
	slow_avail_count: IntCounterVec,
	/// Number of low bitfield propagation events.
	low_bitfields_count: IntCounterVec,
	/// Number of bitfields being set
	bitfields: IntGaugeVec,
	/// Average included time in relay parent blocks
	included_times: HistogramVec,
}

/// Parachain commander prometheus metrics
#[derive(Default, Clone)]
pub struct Metrics(Option<MetricsInner>);

const HISTOGRAM_TIME_BUCKETS: &[f64] =
	&[0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 15.0, 25.0, 35.0, 50.0];

impl Metrics {
	pub(crate) fn on_backed(&self, para_id: u32) {
		if let Some(metrics) = &self.0 {
			metrics.backed_count.with_label_values(&[&para_id.to_string()[..]]).inc();
		}
	}

	pub(crate) fn on_block(&self, time: f64, para_id: u32) {
		if let Some(metrics) = &self.0 {
			metrics.block_times.with_label_values(&[&para_id.to_string()[..]]).observe(time);
		}
	}

	pub(crate) fn on_slow_availability(&self, para_id: u32) {
		if let Some(metrics) = &self.0 {
			metrics.slow_avail_count.with_label_values(&[&para_id.to_string()[..]]).inc();
		}
	}

	pub(crate) fn on_bitfields(&self, nbitfields: u32, is_low: bool, para_id: u32) {
		if let Some(metrics) = &self.0 {
			metrics
				.bitfields
				.with_label_values(&[&para_id.to_string()[..]])
				.set(nbitfields as i64);

			if is_low {
				metrics.low_bitfields_count.with_label_values(&[&para_id.to_string()[..]]).inc();
			}
		}
	}

	pub(crate) fn on_skipped_slot(&self, para_id: u32) {
		if let Some(metrics) = &self.0 {
			metrics.skipped_slots.with_label_values(&[&para_id.to_string()[..]]).inc();
		}
	}

	pub(crate) fn on_disputed(&self, dispute_outcome: &DisputesOutcome, para_id: u32) {
		if let Some(metrics) = &self.0 {
			let para_str: String = para_id.to_string();
			metrics.disputes_stats.disputed_count.with_label_values(&[&para_str[..]]).inc();

			if dispute_outcome.voted_for > dispute_outcome.voted_against {
				metrics.disputes_stats.concluded_valid.with_label_values(&[&para_str[..]]).inc();
			} else {
				metrics
					.disputes_stats
					.concluded_invalid
					.with_label_values(&[&para_str[..]])
					.inc();
			}

			if let Some(diff) = dispute_outcome.resolve_time {
				metrics
					.disputes_stats
					.resolution_time
					.with_label_values(&[&para_str[..]])
					.observe(diff as f64);
			}
		}
	}

	pub(crate) fn on_included(&self, relay_parent_number: u32, previous_included: Option<u32>, para_id: u32) {
		if let Some(metrics) = &self.0 {
			let para_str: String = para_id.to_string();
			metrics.included_count.with_label_values(&[&para_str[..]]).inc();

			if let Some(previous_block_number) = previous_included {
				metrics
					.included_times
					.with_label_values(&[&para_str[..]])
					.observe(relay_parent_number.saturating_sub(previous_block_number) as f64);
			}
		}
	}
}

pub async fn run_prometheus_endpoint(
	prometheus_opts: &ParachainCommanderPrometheusOptions,
) -> Result<(Metrics, Vec<tokio::task::JoinHandle<()>>)> {
	let prometheus_registry = Registry::new_custom(Some("introspector".into()), None)?;
	let metrics = register_metrics(&prometheus_registry)?;
	let socket_addr_str = format!("{}:{}", prometheus_opts.address, prometheus_opts.port);
	let mut futures: Vec<tokio::task::JoinHandle<()>> = vec![];
	for addr in socket_addr_str.to_socket_addrs()? {
		let prometheus_registry = prometheus_registry.clone();
		futures.push(tokio::spawn(async move {
			prometheus_endpoint::init_prometheus(addr, prometheus_registry).await.unwrap()
		}));
	}

	Ok((metrics, futures))
}

fn register_metrics(registry: &Registry) -> Result<Metrics> {
	let disputes_stats = DisputesMetrics {
		disputed_count: prometheus_endpoint::register(
			IntCounterVec::new(Opts::new("pc_disputed_count", "Number of disputed candidates"), &["parachain_id"])?,
			registry,
		)?,
		concluded_valid: prometheus_endpoint::register(
			IntCounterVec::new(
				Opts::new("pc_disputed_valid_count", "Number of disputed candidates concluded valid"),
				&["parachain_id"],
			)?,
			registry,
		)?,
		concluded_invalid: prometheus_endpoint::register(
			IntCounterVec::new(
				Opts::new("pc_disputed_invalid_count", "Number of disputed candidates concluded invalid"),
				&["parachain_id"],
			)?,
			registry,
		)?,
		resolution_time: prometheus_endpoint::register(
			HistogramVec::new(
				HistogramOpts::new("pc_block_time", "Block time for parachain measurements for relay parent blocks")
					.buckets(HISTOGRAM_TIME_BUCKETS.into()),
				&["parachain_id"],
			)?,
			registry,
		)?,
	};
	Ok(Metrics(Some(MetricsInner {
		backed_count: prometheus_endpoint::register(
			IntCounterVec::new(Opts::new("pc_backed_count", "Number of backed candidates"), &["parachain_id"])?,
			registry,
		)?,
		skipped_slots: prometheus_endpoint::register(
			IntCounterVec::new(
				Opts::new(
					"pc_skipped_slots",
					"Number of skipped slots, where no candidate was backed and availability core was free",
				),
				&["parachain_id"],
			)?,
			registry,
		)?,
		included_count: prometheus_endpoint::register(
			IntCounterVec::new(Opts::new("pc_included_count", "Number of candidates included"), &["parachain_id"])?,
			registry,
		)?,
		disputes_stats,
		block_times: prometheus_endpoint::register(
			HistogramVec::new(
				HistogramOpts::new("pc_block_time", "Block time for parachain measurements for relay parent blocks")
					.buckets(HISTOGRAM_TIME_BUCKETS.into()),
				&["parachain_id"],
			)?,
			registry,
		)?,
		slow_avail_count: prometheus_endpoint::register(
			IntCounterVec::new(
				Opts::new("pc_slow_available_count", "Number of slow availability events"),
				&["parachain_id"],
			)?,
			registry,
		)?,
		low_bitfields_count: prometheus_endpoint::register(
			IntCounterVec::new(
				Opts::new("pc_low_bitfields_count", "Number of low bitfields events"),
				&["parachain_id"],
			)?,
			registry,
		)?,
		bitfields: prometheus_endpoint::register(
			IntGaugeVec::new(Opts::new("pc_bitfields_count", "Number of bitfields"), &["parachain_id"]).unwrap(),
			registry,
		)?,
		included_times: prometheus_endpoint::register(
			HistogramVec::new(
				HistogramOpts::new("pc_included_time", "Average included time in relay parent blocks")
					.buckets(HISTOGRAM_TIME_BUCKETS.into()),
				&["parachain_id"],
			)?,
			registry,
		)?,
	})))
}
