use std::collections::HashMap;

use serde::Serialize;

use crate::Result;

use super::Section;

/// Shared state for catalog sections.
#[derive(Default)]
pub struct CatalogState {
	pub sections: HashMap<String, serde_json::Value>,
}

/// A catalog writer that manages typed sections and serializes them to JSON.
///
/// Each section is identified by a name and stores a typed value.
/// The writer can encode all sections into a single JSON object for publishing.
#[derive(Clone)]
pub struct CatalogWriter {
	state: conducer::Producer<CatalogState>,
}

impl Default for CatalogWriter {
	fn default() -> Self {
		Self::new()
	}
}

impl CatalogWriter {
	pub fn new() -> Self {
		Self {
			state: conducer::Producer::new(CatalogState::default()),
		}
	}

	/// Write a section value. Serializes T to a JSON Value and stores it.
	/// Consumers are notified of the change.
	pub fn set<T: Serialize>(&self, section: &Section<T>, value: &T) -> Result<()> {
		let json = serde_json::to_value(value)?;
		let mut state = self.state.write().map_err(|_| crate::Error::Closed)?;
		state.sections.insert(section.name.to_string(), json);
		Ok(())
	}

	/// Remove a section from the catalog.
	/// Consumers are notified of the change.
	pub fn remove(&self, name: &str) -> Result<()> {
		let mut state = self.state.write().map_err(|_| crate::Error::Closed)?;
		state.sections.remove(name);
		Ok(())
	}

	/// Create a consumer that gets notified on any catalog change.
	pub fn consume(&self) -> conducer::Consumer<CatalogState> {
		self.state.consume()
	}

	/// Get read-only access to the current state.
	pub fn read(&self) -> conducer::Ref<'_, CatalogState> {
		self.state.read()
	}

	/// Serialize all sections to a JSON byte vector.
	pub fn encode(&self) -> Result<Vec<u8>> {
		let state = self.state.read();
		Ok(serde_json::to_vec(&state.sections)?)
	}

	/// Close the writer, notifying all consumers.
	pub fn close(&self) {
		let _ = self.state.close();
	}
}
