/// Renders an error and its `source()` chain into a single message.
///
/// Dependency errors are stored as messages so their crates stay out of this crate's public
/// API. Several of them keep the actionable half in `source()` and nothing but a category in
/// `Display`, so a plain `to_string()` would drop the only detail worth reporting.
pub(crate) fn message(err: impl std::error::Error) -> String {
	use std::fmt::Write;

	let mut out = err.to_string();
	let mut source = err.source();
	while let Some(err) = source {
		let _ = write!(out, ": {err}");
		source = err.source();
	}
	out
}

/// Errors from moq-mux operations.
///
/// Most variants are delegations to underlying layers: [`moq_net::Error`] for
/// transport / pub-sub failures, [`hang::Error`] for catalog/codec parsing, the
/// per-format Errors for container shape problems, and the per-codec Errors for
/// bitstream parsing problems.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// Error from the underlying moq-net transport.
	#[error("moq: {0}")]
	Moq(#[from] moq_net::Error),

	/// Error from the hang catalog/codec layer.
	#[error("hang: {0}")]
	Hang(#[from] hang::Error),

	/// Error publishing or consuming JSON over a track.
	#[error("json: {0}")]
	Json(#[from] moq_json::Error),

	/// A catalog entry declares a track mode this build does not implement.
	#[error("unsupported track mode: {0}")]
	UnsupportedMode(String),

	/// A locally published track's catalog entry points at another broadcast.
	#[error("a locally published track can't reference another broadcast")]
	ForeignBroadcast,

	/// Error parsing or building CMAF moof+mdat fragments.
	#[error("cmaf: {0}")]
	Cmaf(#[from] crate::container::fmp4::Error),

	/// Error parsing or building MKV / WebM streams.
	#[error("mkv: {0}")]
	Mkv(#[from] crate::container::mkv::Error),

	/// Error decoding the MSF catalog.
	#[error("msf: {0}")]
	Msf(#[from] crate::catalog::msf::Error),

	/// Error parsing or building LOC frames.
	#[error("loc: {0}")]
	Loc(#[from] moq_loc::Error),

	/// Error parsing an Annex B NAL stream.
	#[error("annexb: {0}")]
	Annexb(#[from] crate::codec::annexb::Error),

	/// Error parsing AAC.
	#[error("aac: {0}")]
	Aac(#[from] crate::codec::aac::Error),

	/// Error parsing Opus.
	#[error("opus: {0}")]
	Opus(#[from] crate::codec::opus::Error),

	/// Error parsing FLAC.
	#[error("flac: {0}")]
	Flac(#[from] crate::codec::flac::Error),

	/// Error parsing MP3.
	#[error("mp3: {0}")]
	Mp3(#[from] crate::codec::mp3::Error),

	/// Error parsing H.264.
	#[error("h264: {0}")]
	H264(#[from] crate::codec::h264::Error),

	/// Error parsing H.265.
	#[error("h265: {0}")]
	H265(#[from] crate::codec::h265::Error),

	/// Error parsing AV1.
	#[error("av1: {0}")]
	Av1(#[from] crate::codec::av1::Error),

	/// Error parsing VP8.
	#[error("vp8: {0}")]
	Vp8(#[from] crate::codec::vp8::Error),

	/// Error parsing VP9.
	#[error("vp9: {0}")]
	Vp9(#[from] crate::codec::vp9::Error),

	/// Error parsing legacy audio (MP2 / AC-3 / E-AC-3).
	#[error("legacy: {0}")]
	Legacy(#[from] crate::codec::legacy::Error),

	/// Timestamp overflow when converting between timescales.
	#[error("timestamp overflow")]
	TimestampOverflow(#[from] moq_net::TimeOverflow),

	/// Error decoding or encoding an mp4 atom.
	#[error("mp4: {0}")]
	Mp4(std::sync::Arc<mp4_atom::Error>),

	/// I/O error.
	#[error("io: {0}")]
	Io(std::sync::Arc<std::io::Error>),

	/// URL parse error.
	#[error("url: {0}")]
	Url(String),

	/// Unknown media format.
	#[error("unknown format: {0}")]
	UnknownFormat(String),

	/// A video format that a raw byte stream cannot be split into.
	///
	/// Only the self-delimiting codecs (Annex-B H.264/H.265, AV1 OBUs) carry their own frame
	/// boundaries. The rest need length prefixes or an out-of-band config record, so they can only
	/// be imported as whole frames via [`Track::video`](crate::import::Track::video).
	#[error("{0} is not self-describing, so its frame boundaries can't be inferred from a stream")]
	NotSelfDescribing(String),

	/// A format was handed to a constructor for a different kind of import.
	///
	/// Each entry point takes only the fields its kind can honor, so the format has to match: an
	/// audio format carries no video hint, and a container has no single rendition to label.
	#[error("{format} is a {actual} format, not {wanted}")]
	WrongKind {
		/// The format string the caller passed.
		format: String,
		/// The kind that actually handles it.
		actual: &'static str,
		/// The kind the constructor expected.
		wanted: &'static str,
	},

	/// A non-keyframe frame was received before any keyframe opened a group.
	/// A track joining mid-stream should skip frames until the first keyframe.
	#[error("{0}")]
	MissingKeyframe(#[from] crate::container::MissingKeyframe),

	/// An explicit video endpoint precedes its last frame.
	#[error("{0}")]
	InvalidEnd(#[from] crate::container::InvalidEnd),

	/// A group starts before the previous group did: the source restarted.
	#[error("{0}")]
	TimestampRewind(#[from] crate::container::TimestampRewind),

	/// A rendition was modified before its initial config was published.
	#[error("rendition is not published")]
	NotPublished,

	/// A FLV video frame resolved to a negative presentation timestamp.
	#[error("negative FLV video presentation timestamp: dts={dts_ms}ms composition_time={composition_time_ms}ms")]
	NegativeFlvPts {
		/// The FLV tag decode timestamp in milliseconds.
		dts_ms: u64,
		/// The signed FLV composition-time offset in milliseconds.
		composition_time_ms: i32,
	},

	/// An HLS record did not land at its window's next index: pushed out of order, or read where
	/// its `sequence` differs from its window index.
	#[error("HLS record {actual} where {expected} was next")]
	HlsSequence {
		/// The window's next index.
		expected: u64,
		/// The record's sequence.
		actual: u64,
	},

	/// An HLS frame report named a group before the previous report's.
	#[error("HLS report for group {group} after group {last}")]
	HlsGroup {
		/// The rejected report's group.
		group: u64,
		/// The previous report's group.
		last: u64,
	},

	/// Error from a muxer/demuxer that reports via `anyhow` (currently the MPEG-TS demuxer).
	/// Boxed in an `Arc` so the enum stays `Clone` (`anyhow::Error` is not).
	#[error("{0}")]
	Other(std::sync::Arc<anyhow::Error>),

	/// A timestamp cannot be mapped between the broadcast clock and the wall: PTS zero would land
	/// before the moq epoch (2020) or after now, the host's wall clock was unset so the clock has no
	/// mapping, or a catalog root `clock` would change or clear the producer's fixed clock.
	#[error("timestamp cannot be mapped onto the broadcast clock: {0}")]
	UnmappableTimestamp(String),
	/// Tried to set an application catalog section whose name collides with a
	/// reserved media section (`video`/`audio`).
	#[error("reserved catalog section: {0}")]
	ReservedSection(String),

	/// A rendition declared a container `kind` this build does not recognize, so its
	/// frames cannot be parsed. Such a rendition must be ignored, not guessed at.
	#[error("unsupported container: {0}")]
	UnsupportedContainer(String),

	/// A rendition's `broadcast` reference walks above the root, so it names no broadcast.
	///
	/// The root is the consumer's authorized subtree, so such a reference is an attempt to
	/// name content the consumer cannot reach. It rejects the whole catalog
	/// ([`catalog::hang::Consumer`](crate::catalog::hang::Consumer)) rather than the one
	/// rendition, and is also what a direct [`Source::resolve`](crate::Source::resolve) /
	/// [`Source::subscribe_track`](crate::Source::subscribe_track) reports.
	#[error("broadcast reference escapes the root: {0}")]
	EscapingBroadcast(String),

	/// A reference's `epoch` names another publisher instance than the one serving its broadcast.
	///
	/// The served instance is the one the source resolved first for that path, so a later
	/// reference cannot move it; the reference fails instead of reading other content.
	#[error("broadcast {path} serves epoch {actual}, not the referenced {expected}")]
	EpochMismatch {
		/// The broadcast's path, relative to the origin root.
		path: String,
		/// The epoch the reference names.
		expected: moq_net::Epoch,
		/// The epoch the resolved broadcast serves.
		actual: moq_net::Epoch,
	},

	/// A rendition tried to lower jitter already advertised to subscribers.
	#[error("catalog jitter cannot decrease for a published rendition")]
	JitterDecreased,

	/// A rendition tried to lower delay already advertised to subscribers.
	#[error("catalog delay cannot decrease for a published rendition")]
	DelayDecreased,

	/// A capture instant is ahead of the broadcast clock's now, or before its epoch.
	#[error("capture time is outside the broadcast clock")]
	InvalidCapture,

	/// A transport stream passthrough was fed packets other than 188 bytes.
	///
	/// A 192-byte source (BDAV M2TS, a four-byte timestamp before each packet) or a 204-byte one
	/// (Reed-Solomon parity after each) is refused rather than reframed, since dropping those bytes
	/// would break the byte identity passthrough exists for.
	#[error("transport stream uses {0}-byte packets; passthrough carries 188-byte packets only")]
	TsPacketSize(usize),

	/// The clock PID's PCR went backwards without `discontinuity_indicator`.
	///
	/// The source broke its own timebase without saying so, which leaves no timestamp that both
	/// follows the PCR and stays on one timeline.
	#[error("PCR on PID {pid} went back from {from} to {to} without a discontinuity_indicator")]
	PcrRewind {
		/// The clock PID.
		pid: u16,
		/// The previous PCR, raw 27 MHz ticks.
		from: u64,
		/// The PCR that stepped back, raw 27 MHz ticks.
		to: u64,
	},

	/// The clock PID's PCR went backwards with `discontinuity_indicator`: the source restarted.
	///
	/// What follows is new content, which belongs in a new broadcast rather than on a timeline that
	/// steps back, so the import ends here.
	#[error("PCR on PID {pid} restarted from {from} back to {to}; a restart needs a new broadcast")]
	PcrRestart {
		/// The clock PID.
		pid: u16,
		/// The previous PCR, raw 27 MHz ticks.
		from: u64,
		/// The flagged PCR, raw 27 MHz ticks.
		to: u64,
	},

	/// The clock PID carried no PCR across more bytes than a group may hold.
	///
	/// Those bytes cannot be timed, and holding them for a PCR that may never come is unbounded.
	#[error("no PCR on PID {pid} for {bytes} bytes")]
	PcrGap {
		/// The clock PID.
		pid: u16,
		/// The bytes waiting on a PCR.
		bytes: u64,
	},

	/// Another publisher instance replaced a followed broadcast, and stitching was off.
	#[error("another publisher instance replaced broadcast `{0}`; enable stitch to follow it as a program switch")]
	Replaced(String),
	/// A published rendition changed fields that require a new decoder configuration.
	#[error("rendition `{0}` changed identity; publish it under a new track name")]
	IdentityChanged(String),

	/// A rendition moved to a track whose media goes back before what the export already read for it.
	#[error(
		"rendition {rendition} moved to a track that goes back in time: a group starts at {start:?}, not past {read:?}"
	)]
	RenditionRewound {
		/// The rendition's ID in the catalog.
		rendition: String,
		/// Where the new track's group starts.
		start: std::time::Duration,
		/// The latest media the export read from the rendition's previous track.
		read: std::time::Duration,
	},
}

impl Error {
	/// The error for a rendition whose container this build does not recognize.
	pub(crate) fn unsupported_container(container: &hang::catalog::UnknownContainer) -> Self {
		Self::UnsupportedContainer(container.kind().to_string())
	}

	/// The reason a track this error aborts gives its subscribers. Only a transport error has a
	/// wire code; anything else reaches them as a generic failure while the cause stays here.
	pub(crate) fn reason(&self) -> moq_net::Error {
		match self {
			Self::Moq(err) => err.clone(),
			Self::Json(moq_json::Error::Net(err)) => err.clone(),
			_ => moq_net::StreamError::Internal.into(),
		}
	}
}

impl From<anyhow::Error> for Error {
	fn from(err: anyhow::Error) -> Self {
		Error::Other(std::sync::Arc::new(err))
	}
}

impl From<mp4_atom::Error> for Error {
	fn from(err: mp4_atom::Error) -> Self {
		Error::Mp4(std::sync::Arc::new(err))
	}
}

// Flattened to its message so `url` stays out of this crate's public API.
impl From<url::ParseError> for Error {
	fn from(err: url::ParseError) -> Self {
		Error::Url(message(err))
	}
}

impl From<std::io::Error> for Error {
	fn from(err: std::io::Error) -> Self {
		Error::Io(std::sync::Arc::new(err))
	}
}

/// A Result type alias for moq-mux operations.
pub type Result<T> = std::result::Result<T, Error>;
