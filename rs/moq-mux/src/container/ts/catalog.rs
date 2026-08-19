//! MPEG-TS catalog extension (the `mpegts` section).
//!
//! The `mpegts` section carries everything needed to faithfully re-mux a broadcast
//! back to MPEG-TS that doesn't belong in the codec-neutral media configs: one
//! entry per track (its original PID and PMT descriptors), a `verbatim` carriage
//! record for every elementary stream we don't decode (SCTE-35, teletext, DVB
//! subtitles, private data, ...), the program-level PMT descriptors, the program
//! identity ([`Program`]), and the standalone SI table map ([`SiEntry`]).
//! Demuxed media tracks keep their codec config in the base `video`/`audio`
//! sections; only their MPEG-TS identity lands here.

use std::collections::BTreeMap;
use std::time::Duration;

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_with::base64::Base64;
use serde_with::{DisplayFromStr, DurationMilliSeconds, serde_as};

use crate::catalog::hang::CatalogExt;

/// The standalone SI PIDs we capture. Everything on a captured PID is carried,
/// whatever its `table_id`: the SDT PID also carries the BAT, the EIT PID carries
/// both now/next and the full schedule, and a table we've never heard of is exactly
/// as worth preserving as one we have.
///
/// TDT/TOT (0x0014) is proxied, not synthesized (#2914): EIT event times are
/// expressed on the source's clock, so a locally-minted time next to a relayed
/// schedule can disagree with it, and TOT's `local_time_offset` descriptors (DST
/// transitions, per-country offsets) are operator data an exporter cannot invent.
/// Each tick replaces the last on its latest-value slot, so staleness is bounded by
/// the source's own repetition interval plus path latency, the same class a TS
/// receiver already lives with.
pub(super) const SI_PIDS: &[u16] = &[
	0x0010, // NIT (network description)
	0x0011, // SDT and BAT (service and bouquet description)
	0x0012, // EIT (event information: now/next and schedule)
	0x0014, // TDT and TOT (time and local-time offset)
];

/// The DVB maximum repetition interval for a known `table_id` (ETSI TS 101 211).
///
/// Export treats the interval as an upper bound, not the source's observed cadence,
/// which is a property of that multiplexer's bitrate shaping and means nothing
/// downstream. `None` for a table we don't recognize; export then falls back to its
/// PSI cadence, so an unknown table degrades to a safe (fast) rate rather than being
/// dropped. Keyed by `table_id` rather than PID because that is the granularity the
/// spec defines: the EIT PID alone carries tables wanting 2s and 30s.
pub(super) fn si_interval(table_id: u8) -> Option<Duration> {
	match table_id {
		// NIT actual/other.
		0x40..=0x41 => Some(Duration::from_secs(10)),
		// SDT actual.
		0x42 => Some(Duration::from_secs(2)),
		// SDT other.
		0x46 => Some(Duration::from_secs(10)),
		// BAT.
		0x4A => Some(Duration::from_secs(10)),
		// EIT now/next actual.
		0x4E => Some(Duration::from_secs(2)),
		// EIT now/next other.
		0x4F => Some(Duration::from_secs(10)),
		// EIT schedule actual.
		0x50..=0x5F => Some(Duration::from_secs(10)),
		// EIT schedule other.
		0x60..=0x6F => Some(Duration::from_secs(30)),
		// TDT and TOT.
		0x70 | 0x73 => Some(Duration::from_secs(30)),
		_ => None,
	}
}

/// The `mpegts` catalog section.
///
/// Omitted from the catalog when empty, so a broadcast that needs none of it stays
/// byte-identical to one without the extension.
#[serde_as]
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

	/// Program identity from the PAT. Present only for a source that carries one (a TS
	/// input); omitted for media-only broadcasts, so export then synthesizes a minimal
	/// identity.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub program: Option<Program>,

	/// Standalone SI tables, keyed by the PID they ride on (0x0011 SDT, 0x0010 NIT,
	/// 0x0012 EIT, ...) and then by `table_id`. The section bytes themselves live on
	/// each entry's snapshot [track](SiEntry::track); the catalog only maps where they
	/// are and how often export must re-emit them. Export re-emits each entry's
	/// sections byte-for-byte on its PID, so the service name, provider, network, and
	/// EPG survive without anyone parsing them.
	///
	/// `table_id` is byte 0 of generic section syntax (ISO 13818-1), so the key is no
	/// less generic than the PID: which *ranges* mean what is a DVB convention, and
	/// none appears here.
	///
	/// JSON object keys are strings, so both keys are written in decimal (`"17"`)
	/// rather than as numbers. The catalog is parsed via `serde_json::Value`, which
	/// will not coerce a string key back to an integer on its own.
	#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
	#[serde_as(as = "BTreeMap<DisplayFromStr, BTreeMap<DisplayFromStr, _>>")]
	pub si: BTreeMap<u16, BTreeMap<u8, SiEntry>>,
}

impl Mpegts {
	/// True when the section carries nothing, so it's omitted from the catalog.
	pub fn is_empty(&self) -> bool {
		self.tracks.is_empty() && self.program_descriptors.is_empty() && self.program.is_none() && self.si.is_empty()
	}
}

/// Program identity, from the PAT.
///
/// The only part of the service layer we parse, because export has to: these three
/// values rebuild a PAT/PMT consistent with the SI tables carried on the tracks
/// [`SiEntry`] maps. Everything else about the program (its name, provider, type,
/// network) stays opaque bytes. Named for the MPEG concept rather than the DVB one: DVB calls
/// a program a service, but the same PAT fields carry an ATSC or ISDB stream too.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Program {
	/// Transport stream ID from the PAT, preserved so the rebuilt PAT stays
	/// consistent with the carried SI.
	pub transport_stream_id: u16,

	/// Program number from the PAT (the DVB service ID). Re-emitted as the PAT program
	/// number and the PMT `program_number`.
	pub program_number: u16,

	/// Original PMT PID from the PAT, preserved so PMT cross-references survive.
	pub pmt_pid: u16,
}

/// One standalone SI table's carriage: which track holds its sections, and how
/// often export must re-emit them.
///
/// The sections themselves ride the named snapshot track (see the `ts` module docs
/// for the track format), not the catalog, so a table revision republishes a small
/// track group instead of the whole catalog. Sections stay opaque: nothing here
/// parses a table, so an SDT, a BAT, an EPG, or a table we've never heard of all
/// round-trip the same way.
#[serde_as]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct SiEntry {
	/// Name of the snapshot track carrying this table's sections, on the same
	/// broadcast as the catalog. Each group is a complete snapshot of the table's
	/// current sections; a joiner reads only the newest group.
	pub track: String,

	/// Re-emit at least this often. A hint: import fills in the DVB maximum for the
	/// tables it recognizes, and export is free to clamp it. Absent for an unknown
	/// `table_id`, which export then re-emits on its own PSI cadence, so an unknown
	/// table degrades to a safe rate rather than being dropped.
	///
	/// Serialized as an integer number of milliseconds (sub-ms precision is truncated).
	#[serde_as(as = "Option<DurationMilliSeconds<u64>>")]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub interval: Option<Duration>,
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
	fn program_and_si_roundtrip() {
		let entry = SiEntry {
			track: "0x0011-0x42.si".to_string(),
			interval: Some(Duration::from_secs(2)),
			..Default::default()
		};

		let mpegts = Mpegts {
			program: Some(Program {
				transport_stream_id: 0x1234,
				program_number: 1,
				pmt_pid: 0x0064,
				..Default::default()
			}),
			si: BTreeMap::from([(0x0011, BTreeMap::from([(0x42u8, entry)]))]),
			..Default::default()
		};
		assert!(!mpegts.is_empty(), "a program record is not empty");

		let json = serde_json::to_string(&Ext { mpegts: mpegts.clone() }).unwrap();
		assert!(json.contains("\"pmtPid\":100"), "identity stays structured: {json}");
		// Both keys are decimal strings, since JSON object keys are strings.
		assert!(json.contains("\"17\":"), "PID key written as a string: {json}");
		assert!(json.contains("\"66\":"), "table_id key written as a string: {json}");
		assert!(
			json.contains("\"track\":\"0x0011-0x42.si\""),
			"the entry names its track: {json}"
		);
		// Lock in the interval's wire shape: a bare integer number of milliseconds.
		// Without the `DurationMilliSeconds` adapter this regresses to serde's default
		// `{secs, nanos}` object.
		assert!(json.contains("\"interval\":2000"), "interval is bare millis: {json}");

		let parsed: Ext = serde_json::from_str(&json).unwrap();
		assert_eq!(parsed.mpegts, mpegts, "program and SI round-trip");
	}
}
