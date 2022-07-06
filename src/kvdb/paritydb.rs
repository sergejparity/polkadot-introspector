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

//! Implementation of the introspection using ParityDB

use super::{DBIter, IntrospectorKvdb};
use color_eyre::{eyre::eyre, Result};
use parity_db::{Db, Options as ParityDBOptions};

pub struct IntrospectorParityDB {
	inner: Db,
	columns: Vec<String>,
}

impl IntrospectorKvdb for IntrospectorParityDB {
	fn new(path: &str) -> Result<Self> {
		let metadata = ParityDBOptions::load_metadata(path.as_ref())
			.map_err(|e| eyre!("Error resolving metas: {:?}", e))?
			.ok_or_else(|| eyre!("Missing metadata"))?;
		let opts = ParityDBOptions::with_columns(path.as_ref(), metadata.columns.len() as u8);
		let db = Db::open_read_only(&opts)?;
		let columns = metadata
			.columns
			.iter()
			.enumerate()
			.map(|(idx, _)| format!("col{}", idx))
			.collect::<Vec<_>>();
		Ok(Self { inner: db, columns })
	}

	fn list_columns(&self) -> color_eyre::Result<&Vec<String>> {
		Ok(&self.columns)
	}

	fn iter_values(&self, column: &str) -> Result<DBIter> {
		let column_idx = self
			.columns
			.iter()
			.position(|col| col.as_str() == column)
			.ok_or_else(|| eyre!("invalid column: {}", column))? as u8;
		let mut iter = self.inner.iter(column_idx)?;

		Ok(Box::new(std::iter::from_fn(move || {
			if let Some((key, value)) = iter.next().unwrap_or(None) {
				Some((key.into_boxed_slice(), value.into_boxed_slice()))
			} else {
				None
			}
		})))
	}
}