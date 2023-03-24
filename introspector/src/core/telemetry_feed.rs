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
//

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use subxt::utils::H256;

type BlockHash = H256;
type BlockNumber = u64;
type Timestamp = u64;
type FeedNodeId = usize;

/// Concise block details
#[derive(Deserialize, Serialize, Debug, Clone, Copy, PartialEq)]
pub struct Block {
	pub hash: BlockHash,
	pub height: BlockNumber,
}

/// Verbose block details
#[derive(Deserialize, Serialize, Debug, Clone, Copy, PartialEq)]
pub struct BlockDetails {
	pub block: Block,
	pub block_time: u64,
	pub block_timestamp: u64,
	pub propagation_time: Option<u64>,
}

#[derive(Debug, PartialEq)]
pub enum TelemetryFeed {
	Version(usize),
	BestBlock { block_number: BlockNumber, timestamp: Timestamp, avg_block_time: Option<u64> },
	BestFinalized { block_number: BlockNumber, block_hash: BlockHash },
	// AddedNode
	RemovedNode { node_id: FeedNodeId },
	LocatedNode { node_id: FeedNodeId, lat: f32, long: f32, city: String },
	ImportedBlock { node_id: FeedNodeId, block_details: BlockDetails },
	FinalizedBlock { node_id: FeedNodeId, block_number: BlockNumber, block_hash: BlockHash },
	// NodeStatsUpdate
	// Hardware
	TimeSync { time: Timestamp },
	AddedChain { name: String, genesis_hash: BlockHash, node_count: usize },
	RemovedChain { genesis_hash: BlockHash },
	SubscribedTo { genesis_hash: BlockHash },
	UnsubscribedFrom { genesis_hash: BlockHash },
	Pong { msg: String },
	StaleNode { node_id: FeedNodeId },
	// NodeIOUpdate
	// ChainStatsUpdate
	UnknownValue { action: u8, value: String },
}

impl TelemetryFeed {
	/// Decodes a slice of bytes into a vector of feed messages.
	/// Telemetry sends encoded messages in an array format like [0,32,1,[14783932,1679657352067,5998]]
	/// where odd values represent action codes and even values represent their payloads.
	pub fn from_bytes(bytes: &[u8]) -> color_eyre::Result<Vec<TelemetryFeed>> {
		let v: Vec<&RawValue> = serde_json::from_slice(bytes)?;

		let mut feed_messages = vec![];
		for raw in v.chunks_exact(2) {
			let action: u8 = serde_json::from_str(raw[0].get())?;
			let msg = TelemetryFeed::decode(action, raw[1])?;

			feed_messages.push(msg);
		}

		Ok(feed_messages)
	}

	// Deserializes the feed message to a value based on the "action" key
	fn decode(action: u8, raw_payload: &RawValue) -> color_eyre::Result<TelemetryFeed> {
		let feed_message = match action {
			// Version:
			0 => {
				let version = serde_json::from_str(raw_payload.get())?;
				TelemetryFeed::Version(version)
			},
			// BestBlock
			1 => {
				let (block_number, timestamp, avg_block_time) = serde_json::from_str(raw_payload.get())?;
				TelemetryFeed::BestBlock { block_number, timestamp, avg_block_time }
			},
			// BestFinalized
			2 => {
				let (block_number, block_hash) = serde_json::from_str(raw_payload.get())?;
				TelemetryFeed::BestFinalized { block_number, block_hash }
			},
			// TODO: Add the following messages
			//  3: AddedNode
			// RemovedNode
			4 => {
				let node_id = serde_json::from_str(raw_payload.get())?;
				TelemetryFeed::RemovedNode { node_id }
			},
			// LocatedNode
			5 => {
				let (node_id, lat, long, city) = serde_json::from_str(raw_payload.get())?;
				TelemetryFeed::LocatedNode { node_id, lat, long, city }
			},
			// ImportedBlock
			6 => {
				let (node_id, (height, hash, block_time, block_timestamp, propagation_time)) =
					serde_json::from_str(raw_payload.get())?;
				TelemetryFeed::ImportedBlock {
					node_id,
					block_details: BlockDetails {
						block: Block { hash, height },
						block_time,
						block_timestamp,
						propagation_time,
					},
				}
			},
			// FinalizedBlock
			7 => {
				let (node_id, block_number, block_hash) = serde_json::from_str(raw_payload.get())?;
				TelemetryFeed::FinalizedBlock { node_id, block_number, block_hash }
			},
			//  8: NodeStatsUpdate
			//  9: Hardware
			// TimeSync
			10 => {
				let time = serde_json::from_str(raw_payload.get())?;
				TelemetryFeed::TimeSync { time }
			},
			// AddedChain
			11 => {
				let (name, genesis_hash, node_count) = serde_json::from_str(raw_payload.get())?;
				TelemetryFeed::AddedChain { name, genesis_hash, node_count }
			},
			// RemovedChain
			12 => {
				let genesis_hash = serde_json::from_str(raw_payload.get())?;
				TelemetryFeed::RemovedChain { genesis_hash }
			},
			// SubscribedTo
			13 => {
				let genesis_hash = serde_json::from_str(raw_payload.get())?;
				TelemetryFeed::SubscribedTo { genesis_hash }
			},
			// UnsubscribedFrom
			14 => {
				let genesis_hash = serde_json::from_str(raw_payload.get())?;
				TelemetryFeed::UnsubscribedFrom { genesis_hash }
			},
			// Pong
			15 => {
				let msg = serde_json::from_str(raw_payload.get())?;
				TelemetryFeed::Pong { msg }
			},
			// StaleNode
			20 => {
				let node_id = serde_json::from_str(raw_payload.get())?;
				TelemetryFeed::StaleNode { node_id }
			},
			// 21: NodeIOUpdate
			// 22: ChainStatsUpdate
			_ => TelemetryFeed::UnknownValue { action, value: raw_payload.to_string() },
		};

		Ok(feed_message)
	}
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn decode_version_best_block_best_finalized() {
		let msg = r#"[0,32,1,[14783932,1679657352067,5998],2,[14783934,"0x0000000000000000000000000000000000000000000000000000000000000000"]]"#;

		assert_eq!(
			TelemetryFeed::from_bytes(msg.as_bytes()).unwrap(),
			vec![
				TelemetryFeed::Version(32),
				TelemetryFeed::BestBlock {
					block_number: 14783932,
					timestamp: 1679657352067,
					avg_block_time: Some(5998)
				},
				TelemetryFeed::BestFinalized { block_number: 14783934, block_hash: BlockHash::zero() }
			]
		);
	}

	#[test]
	fn decode_removed_node_located_node() {
		let msg = r#"[4,42,5,[1560,35.6893,139.6899,"Tokyo"]]"#;
		assert_eq!(
			TelemetryFeed::from_bytes(msg.as_bytes()).unwrap(),
			vec![
				TelemetryFeed::RemovedNode { node_id: 42 },
				TelemetryFeed::LocatedNode { node_id: 1560, lat: 35.6893, long: 139.6899, city: "Tokyo".to_owned() }
			]
		);
	}

	#[test]
	fn decode_imported_block_finalized_block() {
		let msg = r#"[6,[297,[11959,"0x0000000000000000000000000000000000000000000000000000000000000000",6073,1679669286310,233]],7,[92,12085,"0x0000000000000000000000000000000000000000000000000000000000000000"]]"#;
		assert_eq!(
			TelemetryFeed::from_bytes(msg.as_bytes()).unwrap(),
			vec![
				TelemetryFeed::ImportedBlock {
					node_id: 297,
					block_details: BlockDetails {
						block: Block { hash: BlockHash::zero(), height: 11959 },
						block_time: 6073,
						block_timestamp: 1679669286310,
						propagation_time: Some(233)
					}
				},
				TelemetryFeed::FinalizedBlock { node_id: 92, block_number: 12085, block_hash: BlockHash::zero() }
			]
		);
	}

	#[test]
	fn decode_time_sync() {
		let msg = r#"[10,1679670187855]"#;
		assert_eq!(
			TelemetryFeed::from_bytes(msg.as_bytes()).unwrap(),
			vec![TelemetryFeed::TimeSync { time: 1679670187855 }]
		);
	}

	#[test]
	fn decode_added_chain_removed_chain() {
		let msg = r#"[11,["Tick 558","0x0000000000000000000000000000000000000000000000000000000000000000",2],12,"0x0000000000000000000000000000000000000000000000000000000000000000"]"#;
		assert_eq!(
			TelemetryFeed::from_bytes(msg.as_bytes()).unwrap(),
			vec![
				TelemetryFeed::AddedChain {
					name: "Tick 558".to_owned(),
					genesis_hash: BlockHash::zero(),
					node_count: 2
				},
				TelemetryFeed::RemovedChain { genesis_hash: BlockHash::zero() }
			]
		);
	}

	#[test]
	fn decode_subscribed_to_unsubscribed_from() {
		let msg = r#"[13,"0x0000000000000000000000000000000000000000000000000000000000000000",14,"0x0000000000000000000000000000000000000000000000000000000000000000"]"#;
		assert_eq!(
			TelemetryFeed::from_bytes(msg.as_bytes()).unwrap(),
			vec![
				TelemetryFeed::SubscribedTo { genesis_hash: BlockHash::zero() },
				TelemetryFeed::UnsubscribedFrom { genesis_hash: BlockHash::zero() }
			]
		);
	}

	#[test]
	fn decode_pong_stale_node() {
		let msg = r#"[15,"pong",20,297]"#;
		assert_eq!(
			TelemetryFeed::from_bytes(msg.as_bytes()).unwrap(),
			vec![TelemetryFeed::Pong { msg: "pong".to_owned() }, TelemetryFeed::StaleNode { node_id: 297 }]
		);
	}

	#[test]
	fn decode_unknown() {
		let msg = r#"[0,32,42,["0x0000000000000000000000000000000000000000000000000000000000000000", 1]]"#;

		assert_eq!(
			TelemetryFeed::from_bytes(msg.as_bytes()).unwrap(),
			vec![
				TelemetryFeed::Version(32),
				TelemetryFeed::UnknownValue {
					action: 42,
					value: "[\"0x0000000000000000000000000000000000000000000000000000000000000000\", 1]".to_owned()
				}
			]
		);
	}
}
