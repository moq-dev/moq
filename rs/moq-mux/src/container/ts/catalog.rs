//! MPEG-TS catalog extension (the `mpegts` section).
//!
//! The `mpegts` section carries everything needed to faithfully re-mux a broadcast
//! back to MPEG-TS that doesn't belong in the codec-neutral media configs: one
//! entry per track (its original PID and PMT descriptors), a `verbatim` carriage
//! record for every elementary stream we don't decode (SCTE-35, teletext, DVB
//! subtitles, private data, ...), the program-level PMT descriptors, and the DVB
//! service layer ([`Service`]: transport/service identity plus the SDT/NIT tables).
//! Demuxed media tracks keep their codec config in the base `video`/`audio`
//! sections; only their MPEG-TS identity lands here.

use std::collections::BTreeMap;

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_with::base64::Base64;
use serde_with::serde_as;

use crate::catalog::hang::CatalogExt;

/// PID of the DVB Network Information Table (NIT).
pub(super) const NIT_PID: u16 = 0x0010;
/// PID of the DVB Service Description Table (SDT).
pub(super) const SDT_PID: u16 = 0x0011;
/// `table_id` of the NIT for the actual network (NIT Actual).
pub(super) const NIT_ACTUAL_TABLE_ID: u8 = 0x40;
/// `table_id` of the SDT for the actual transport stream (SDT Actual).
pub(super) const SDT_ACTUAL_TABLE_ID: u8 = 0x42;

/// The `mpegts` catalog section.
///
/// Omitted from the catalog when empty, so a broadcast that needs none of it stays
/// byte-identical to one without the extension.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Mpegts {
	/// Per-track MPEG-TS info, keyed by MoQ track name. Media tracks record their
	/// PID and PMT descriptors; undecoded tracks add a [`Verbatim`] carriage record.
	#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
	pub tracks: BTreeMap<String, Track>,

	/// PMT program-level descriptors (`program_info`), carried verbatim. Export
	/// re-emits these; the SCTE-35 'CUEI' registration is derived when absent.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub program_descriptors: Vec<Descriptor>,

	/// The DVB service layer (transport/service identity plus the SDT/NIT tables).
	/// Present only for a source that carries it (a TS input); omitted for media-only
	/// broadcasts, so export then synthesizes a minimal identity.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub service: Option<Service>,
}

impl Mpegts {
	/// True when the section carries nothing, so it's omitted from the catalog.
	pub fn is_empty(&self) -> bool {
		self.tracks.is_empty() && self.program_descriptors.is_empty() && self.service.is_none()
	}
}

/// The DVB service layer of a TS program.
///
/// Two kinds of data: the small structured identity export needs to rebuild a
/// consistent PAT/PMT (the transport stream, service, and PMT PID), and the
/// standalone SI tables ([`sdt`](Self::sdt), [`nit`](Self::nit)) carried verbatim
/// so the service name, provider, type, original network, and network description
/// survive the round-trip byte-for-byte. Regenerated tables (TDT/TOT) and EPG (EIT)
/// are out of scope; they are live or dynamic, not static identity.
#[serde_as]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Service {
	/// Transport stream ID from the PAT, preserved so the rebuilt PAT stays
	/// consistent with the carried SDT/NIT.
	pub transport_stream_id: u16,

	/// Program (service) number from the PAT. Re-emitted as the PAT program number
	/// and the PMT `program_number`.
	pub service_id: u16,

	/// Original PMT PID from the PAT, preserved so PMT cross-references survive.
	pub pmt_pid: u16,

	/// SDT Actual section (`table_id` 0x42), carried verbatim and re-emitted on
	/// PID 0x0011. Carries the service name, provider, type, and original network ID.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	#[serde_as(as = "Option<Base64>")]
	pub sdt: Option<Bytes>,

	/// NIT Actual section (`table_id` 0x40), carried verbatim and re-emitted on
	/// PID 0x0010. Describes the originating delivery network, so it is preserved
	/// rather than synthesized (an operator decision).
	#[serde(default, skip_serializing_if = "Option::is_none")]
	#[serde_as(as = "Option<Base64>")]
	pub nit: Option<Bytes>,
}

/// One track's MPEG-TS identity and signaling.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Track {
	/// Original MPEG-TS PID. Export prefers it so PID cross-references survive;
	/// tracks without an entry are renumbered.
	pub pid: u16,

	/// PMT ES-level descriptors (ISO-639 language, registration, ...), carried
	/// verbatim so they survive the round-trip.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub descriptors: Vec<Descriptor>,

	/// Present when the stream is carried verbatim (not decoded into `video`/`audio`).
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub verbatim: Option<Verbatim>,
}

impl Track {
	/// A new media track entry (decoded; no verbatim carriage), recording its PID.
	pub fn new(pid: u16) -> Self {
		Self {
			pid,
			descriptors: Vec::new(),
			verbatim: None,
		}
	}
}

/// Carriage record for an undecoded elementary stream carried byte-for-byte.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Verbatim {
	/// PMT `stream_type` to re-announce (0x86 SCTE-35, 0x06 private PES, 0x05
	/// private sections, ...).
	pub stream_type: u8,

	/// How the verbatim payload is framed, so export knows how to repacketize it.
	#[serde(default)]
	pub framing: Framing,

	/// Original PES `stream_id` (e.g. 0xBD private_stream_1 for teletext/DVB
	/// subtitles/DVB AC-3, 0xC0-0xDF audio). Preserved so export re-emits the PES
	/// under its real id rather than relabeling it, which strict broadcast demuxers
	/// and TR 101 290 analyzers reject. `None` for section framing or a non-TS
	/// source; export then falls back to `private_stream_1`.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub stream_id: Option<u8>,
}

impl Verbatim {
	/// A new verbatim carriage record of the given `stream_type` and `framing`.
	pub fn new(stream_type: u8, framing: Framing) -> Self {
		Self {
			stream_type,
			framing,
			stream_id: None,
		}
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
#[serde_as]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Descriptor {
	/// The descriptor tag (e.g. 0x05 registration, 0x0A ISO-639 language).
	pub tag: u8,
	/// The descriptor body, base64-encoded in the catalog.
	#[serde_as(as = "Base64")]
	pub data: Bytes,
}

/// The application catalog extension carrying the `mpegts` section. Empty by
/// default, so the section is omitted until an MPEG-TS detail is recorded.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[non_exhaustive]
pub struct Ext {
	#[serde(default, skip_serializing_if = "Mpegts::is_empty")]
	pub mpegts: Mpegts,
}

impl CatalogExt for Ext {}

/// An extension that can carry an `mpegts` catalog section.
///
/// Implement this for an application extension to compose MPEG-TS carriage with
/// additional sections.
pub trait Catalog: CatalogExt {
	/// The section to record MPEG-TS details into, or `None` for an extension that
	/// doesn't carry them.
	///
	/// Keep this stable per catalog: an importer samples support once at
	/// construction, so a result that flips between `Some` and `None` mid-stream
	/// would disable verbatim carriage or fail.
	fn mpegts_mut(&mut self) -> Option<&mut Mpegts>;
}

impl Catalog for () {
	fn mpegts_mut(&mut self) -> Option<&mut Mpegts> {
		None
	}
}

// The untyped passthrough carries no typed mpegts section (a TS importer driving an `Extra`
// catalog records verbatim streams as raw JSON sections, not the typed `Mpegts` view).
impl Catalog for crate::catalog::hang::Extra {
	fn mpegts_mut(&mut self) -> Option<&mut Mpegts> {
		None
	}
}

impl Catalog for Ext {
	fn mpegts_mut(&mut self) -> Option<&mut Mpegts> {
		Some(&mut self.mpegts)
	}
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn empty_section_omitted() {
		// An empty `mpegts` section serializes to `{}` so a media-only broadcast stays
		// byte-identical to one without the extension.
		let ext = Ext::default();
		assert_eq!(serde_json::to_string(&ext).unwrap(), "{}");
	}

	#[test]
	fn section_roundtrip() {
		let mut mpegts = Mpegts::default();
		// A media track: PID + a language descriptor, no verbatim carriage.
		mpegts.tracks.insert(
			"audio".to_string(),
			Track {
				pid: 0x101,
				descriptors: vec![Descriptor {
					tag: 0x0a,
					data: Bytes::from_static(b"eng\x00"),
				}],
				verbatim: None,
			},
		);
		// A verbatim SCTE-35 track.
		mpegts.tracks.insert(
			".scte35".to_string(),
			Track {
				pid: 0x102,
				descriptors: Vec::new(),
				verbatim: Some(Verbatim::new(0x86, Framing::Section)),
			},
		);
		mpegts.program_descriptors.push(Descriptor {
			tag: 0x05,
			data: Bytes::from_static(b"CUEI"),
		});

		let json = serde_json::to_string(&Ext { mpegts: mpegts.clone() }).unwrap();
		// Descriptor bytes are base64 ("CUEI" -> "Q1VFSQ==").
		assert!(json.contains("\"Q1VFSQ==\""), "descriptor data is base64: {json}");

		let parsed: Ext = serde_json::from_str(&json).unwrap();
		assert_eq!(parsed.mpegts, mpegts, "mpegts section round-trips");
	}

	#[test]
	fn service_roundtrip() {
		let mpegts = Mpegts {
			service: Some(Service {
				transport_stream_id: 0x1234,
				service_id: 1,
				pmt_pid: 0x0064,
				sdt: Some(Bytes::from_static(b"\x42\xf0\x25")),
				nit: None,
				..Default::default()
			}),
			..Default::default()
		};
		assert!(!mpegts.is_empty(), "a service record is not empty");

		let json = serde_json::to_string(&Ext { mpegts: mpegts.clone() }).unwrap();
		// The SDT section is base64; the absent NIT is omitted.
		assert!(json.contains("\"sdt\""), "sdt present: {json}");
		assert!(!json.contains("\"nit\""), "absent nit omitted: {json}");

		let parsed: Ext = serde_json::from_str(&json).unwrap();
		assert_eq!(parsed.mpegts, mpegts, "service section round-trips");
	}
}
