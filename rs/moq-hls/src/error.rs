//! Errors for the HLS / LL-HLS gateway.

/// Errors produced by the HLS <-> MoQ gateway (import and export).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// Error from the underlying moq-net transport.
	#[error("moq: {0}")]
	Moq(#[from] moq_net::Error),

	/// Error from the moq-mux CMAF import/export layer.
	#[error("mux: {0}")]
	Mux(#[from] moq_mux::Error),

	/// The playlist argument looked like an HTTP(S) URL but failed to parse.
	#[error("invalid playlist URL")]
	InvalidPlaylistUrl,

	/// The playlist argument was a local path that could not be made into a `file://` URL.
	#[error("invalid file path")]
	InvalidFilePath,

	/// A `file://` URL could not be turned back into a filesystem path.
	#[error("invalid file URL")]
	InvalidFileUrl,

	/// The fetched HLS playlist could not be parsed.
	#[error("failed to parse HLS playlist: {0}")]
	ParsePlaylist(String),

	/// The master playlist contained no variant this gateway can import.
	#[error("no usable variants found in master playlist")]
	NoVariants,

	/// A media playlist had no `EXT-X-MAP`, so there is no CMAF init segment.
	#[error("playlist missing EXT-X-MAP")]
	MissingMap,

	/// A media segment had an empty URI.
	#[error("encountered segment with empty URI")]
	EmptySegmentUri,

	/// An implicit HLS byte range had no preceding range for the same resource.
	#[error("implicit byte range for {url} has no preceding range for the same resource")]
	MissingByteRangeOffset {
		/// The resource whose range omitted an offset.
		url: url::Url,
	},

	/// An HLS byte range was empty or overflowed its integer representation.
	#[error("invalid byte range {start}+{length} for {url}")]
	InvalidByteRange {
		/// The ranged resource.
		url: url::Url,
		/// The first requested byte.
		start: u64,
		/// The requested byte count.
		length: u64,
	},

	/// A ranged resource response had a different length than requested.
	#[error("byte range for {url} returned {actual} bytes, expected {expected}")]
	ByteRangeLengthMismatch {
		/// The ranged resource.
		url: url::Url,
		/// The requested byte count.
		expected: u64,
		/// The returned byte count.
		actual: usize,
	},

	/// A partial HTTP response identified a different byte range than requested.
	#[error("byte range for {url} returned a mismatched Content-Range, expected bytes {start}-{end}")]
	ByteRangeResponseMismatch {
		/// The ranged resource.
		url: url::Url,
		/// The first requested byte.
		start: u64,
		/// The last requested byte, inclusive.
		end: u64,
	},

	/// An HLS media or discontinuity sequence was too large to pack into a MoQ group sequence.
	#[error("HLS {kind} sequence {value} is too large to encode")]
	SequenceOverflow {
		/// Which sequence overflowed: `"media"` or `"discontinuity"`.
		kind: &'static str,
		/// The offending sequence value.
		value: u64,
	},

	/// A playlist or segment URI could not be resolved against its base.
	#[error(transparent)]
	UrlParse(#[from] url::ParseError),

	/// HTTP error while fetching a playlist or segment.
	#[error(transparent)]
	Reqwest(#[from] reqwest::Error),

	/// I/O error while reading a local playlist or segment.
	#[error(transparent)]
	Io(#[from] std::io::Error),

	/// Catch-all for gateway logic that reports via `anyhow`.
	#[error(transparent)]
	Other(#[from] anyhow::Error),
}

/// Convenience alias for results from the HLS gateway.
pub type Result<T> = std::result::Result<T, Error>;
