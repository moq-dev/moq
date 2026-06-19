//! TS-specific catalog extension.
//!
//! The `ts` section carries everything needed to faithfully re-mux a broadcast
//! back to MPEG-TS that doesn't belong in the codec-neutral media configs: the
//! original PID of each track, and a verbatim description of every elementary
//! stream we don't decode (SCTE-35, teletext, DVB subtitles, private data, ...).
//! Demuxed media tracks stay in the base `video`/`audio` sections; only their
//! PID (when preservation matters) lands here.

use std::collections::BTreeMap;

use bytes::Bytes;
use serde::{Deserialize, Serialize};

use crate::catalog::hang::CatalogExt;

/// Serialize [`Bytes`] as a base64 string in the catalog JSON.
mod base64_bytes {
	use base64::Engine;
	use bytes::Bytes;
	use serde::{Deserialize, Deserializer, Serializer};

	pub fn serialize<S: Serializer>(bytes: &Bytes, serializer: S) -> Result<S::Ok, S::Error> {
		serializer.serialize_str(&base64::engine::general_purpose::STANDARD.encode(bytes))
	}

	pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Bytes, D::Error> {
		let encoded = String::deserialize(deserializer)?;
		let decoded = base64::engine::general_purpose::STANDARD
			.decode(encoded.as_bytes())
			.map_err(serde::de::Error::custom)?;
		Ok(Bytes::from(decoded))
	}
}

/// The `ts` catalog section.
///
/// Both maps are keyed by MoQ track name and omitted from the catalog when empty,
/// so a broadcast that needs neither stays byte-identical.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Section {
	/// Original MPEG-TS PID per track name, for media and verbatim streams alike.
	/// Export prefers these so PID cross-references survive; tracks not listed are
	/// renumbered.
	#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
	pub pids: BTreeMap<String, u16>,

	/// Elementary streams we don't decode, carried verbatim, one MoQ track per PID.
	/// SCTE-35 is just an entry here with `stream_type` 0x86.
	#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
	pub streams: BTreeMap<String, Stream>,
}

impl Section {
	/// True when the section carries nothing, so it's omitted from the catalog.
	pub fn is_empty(&self) -> bool {
		self.pids.is_empty() && self.streams.is_empty()
	}
}

/// How a verbatim stream's payload is framed on the wire, so export can repacketize it.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum Framing {
	/// Packetized Elementary Stream: each frame is one PES payload (access unit),
	/// timestamped by its PTS. Used by private PES, teletext, DVB subtitles, ...
	#[default]
	Pes,
	/// Private sections (table_id + section_length framing). Each frame is one
	/// complete section. Used by SCTE-35 and other private-section signaling.
	Section,
}

/// One PMT descriptor, carried verbatim so language/registration/etc. survive the
/// round-trip without a per-descriptor parser.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Descriptor {
	/// The descriptor tag (e.g. 0x05 registration, 0x0A ISO-639 language).
	pub tag: u8,
	/// The descriptor body, base64-encoded in the catalog.
	#[serde(with = "base64_bytes")]
	pub data: Bytes,
}

/// One undecoded elementary stream, its payload carried verbatim as MoQ frames.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Stream {
	/// PMT `stream_type` to re-announce (0x86 SCTE-35, 0x06 private PES, 0x05
	/// private sections, ...).
	pub stream_type: u8,

	/// How the verbatim payload is framed, so export knows how to repacketize it.
	#[serde(default)]
	pub framing: Framing,

	/// PMT ES-level descriptors, carried verbatim.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub descriptors: Vec<Descriptor>,

	/// How the verbatim bytes are timestamp-framed as MoQ frames.
	#[serde(default)]
	pub container: hang::catalog::Container,
}

impl Stream {
	/// A new verbatim stream of the given `stream_type` and `framing`, framed as
	/// [`Container::Legacy`](hang::catalog::Container::Legacy) MoQ frames.
	pub fn new(stream_type: u8, framing: Framing) -> Self {
		Self {
			stream_type,
			framing,
			descriptors: Vec::new(),
			container: hang::catalog::Container::Legacy,
		}
	}
}

/// The application catalog extension carrying the `ts` section. Empty by default,
/// so the section is omitted until a TS-specific detail is recorded.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct Ext {
	#[serde(default, skip_serializing_if = "Section::is_empty")]
	pub ts: Section,
}

impl CatalogExt for Ext {}

/// An extension that can carry a `ts` catalog section.
///
/// Implement this for an application extension to compose TS carriage with
/// additional sections.
pub trait Catalog: CatalogExt {
	/// The section to record TS details into, or `None` for an extension that
	/// doesn't carry them.
	///
	/// Keep this stable per catalog: an importer samples support once at
	/// construction, so a result that flips between `Some` and `None` mid-stream
	/// would disable verbatim carriage or fail.
	fn ts_mut(&mut self) -> Option<&mut Section>;
}

impl Catalog for () {
	fn ts_mut(&mut self) -> Option<&mut Section> {
		None
	}
}

impl Catalog for Ext {
	fn ts_mut(&mut self) -> Option<&mut Section> {
		Some(&mut self.ts)
	}
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn empty_section_omitted() {
		// An empty `ts` section serializes to `{}` so a media-only broadcast stays
		// byte-identical to one without the extension.
		let ext = Ext::default();
		assert_eq!(serde_json::to_string(&ext).unwrap(), "{}");
	}

	#[test]
	fn section_roundtrip() {
		let mut section = Section::default();
		section.pids.insert("video".to_string(), 0x100);
		section.pids.insert(".ts".to_string(), 0x102);
		let mut stream = Stream::new(0x86, Framing::Section);
		stream.descriptors.push(Descriptor {
			tag: 0x05,
			data: Bytes::from_static(b"CUEI"),
		});
		section.streams.insert(".ts".to_string(), stream);

		let json = serde_json::to_string(&Ext { ts: section.clone() }).unwrap();
		// Descriptor bytes are base64 ("CUEI" -> "Q1VFSQ==").
		assert!(json.contains("\"Q1VFSQ==\""), "descriptor data is base64: {json}");

		let parsed: Ext = serde_json::from_str(&json).unwrap();
		assert_eq!(parsed.ts, section, "ts section round-trips");
	}
}
