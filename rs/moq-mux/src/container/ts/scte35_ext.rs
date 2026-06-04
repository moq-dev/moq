//! SCTE-35 application catalog extension for ingesting MPEG-TS splice cues.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::catalog::hang::CatalogExt;

/// SCTE-35 splice cue tracks: a map of renditions (one per MPEG-TS PID), each
/// carried as the verbatim `splice_info_section` bytes.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Scte35 {
	pub renditions: BTreeMap<String, Scte35Config>,
}

impl Scte35 {
	/// Omitted from the catalog when empty, so a broadcast without cues stays byte-identical.
	pub fn is_empty(&self) -> bool {
		self.renditions.is_empty()
	}
}

/// One SCTE-35 cue track. Records how the verbatim section was framed; the
/// stream_type (0x86) and CUEI signaling are implicit to SCTE-35.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Scte35Config {
	#[serde(default)]
	pub container: hang::catalog::Container,
}

impl Scte35Config {
	pub fn new() -> Self {
		Self::default()
	}
}

/// The application catalog extension carrying the `scte35` section. Empty by
/// default, so the section is omitted until a cue track is added.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct Scte35Ext {
	#[serde(default, skip_serializing_if = "Scte35::is_empty")]
	pub scte35: Scte35,
}

impl CatalogExt for Scte35Ext {}

/// An extension that can carry an SCTE-35 catalog section.
///
/// Implement this for an application extension to compose SCTE-35 with
/// additional sections.
pub trait Scte35Catalog: CatalogExt {
	/// The section to write cues into, or `None` for an extension that doesn't carry them.
	///
	/// Keep this stable per catalog: an importer samples support once at construction, so a
	/// result that flips between `Some` and `None` mid-stream would disable cues or fail.
	fn scte35_mut(&mut self) -> Option<&mut Scte35>;
}

impl Scte35Catalog for () {
	fn scte35_mut(&mut self) -> Option<&mut Scte35> {
		None
	}
}

impl Scte35Catalog for Scte35Ext {
	fn scte35_mut(&mut self) -> Option<&mut Scte35> {
		Some(&mut self.scte35)
	}
}
