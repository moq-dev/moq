//! Errors for the HLS / LL-HLS gateway.

/// Which HLS sequence counter a value belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SequenceKind {
	/// `EXT-X-MEDIA-SEQUENCE`: names a segment within the live window.
	Media,
	/// `EXT-X-DISCONTINUITY-SEQUENCE`: names a media timeline.
	Discontinuity,
}

impl std::fmt::Display for SequenceKind {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Self::Media => write!(f, "media"),
			Self::Discontinuity => write!(f, "discontinuity"),
		}
	}
}

/// Errors produced by the HLS <-> MoQ gateway (import and export).
#[derive(Debug, Clone, thiserror::Error)]
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
		/// Which sequence overflowed.
		kind: SequenceKind,
		/// The offending sequence value.
		value: u64,
	},

	/// A playlist or segment URI could not be resolved against its base.
	#[error("url parse: {0}")]
	UrlParse(#[from] url::ParseError),

	/// HTTP error while fetching a playlist or segment.
	#[error("reqwest: {0}")]
	Reqwest(std::sync::Arc<reqwest::Error>),

	/// I/O error while reading a local playlist or segment.
	#[error("io: {0}")]
	Io(std::sync::Arc<std::io::Error>),

	/// Catch-all for gateway logic that reports via `anyhow`.
	#[error("{0}")]
	Other(std::sync::Arc<anyhow::Error>),
}

impl Error {
	/// Whether repeating the failed operation could plausibly succeed with nothing else changing.
	/// See [`moq_net::Error::is_retryable`].
	///
	/// The gateway sits between an HTTP origin and a MoQ relay, so both halves can be transient. A
	/// playlist that didn't parse, a segment whose byte range didn't add up, or a URL that isn't one
	/// will fail identically on the next pass: those end the import instead of looping on it.
	pub fn is_retryable(&self) -> bool {
		match self {
			Self::Moq(err) => err.is_retryable(),
			Self::Mux(err) => err.is_retryable(),
			Self::Io(err) => moq_net::retry::io_retryable(err),
			// No response at all is the network; a response that arrived is the origin's answer.
			Self::Reqwest(err) => err
				.status()
				.is_none_or(|status| moq_net::retry::status_retryable(status.as_u16())),

			// The playlist, its URLs, or the segments it points at are malformed.
			Self::InvalidPlaylistUrl
			| Self::InvalidFilePath
			| Self::InvalidFileUrl
			| Self::UrlParse(_)
			| Self::ParsePlaylist(_)
			| Self::NoVariants
			| Self::MissingMap
			| Self::EmptySegmentUri
			| Self::MissingByteRangeOffset { .. }
			| Self::InvalidByteRange { .. }
			| Self::ByteRangeLengthMismatch { .. }
			| Self::ByteRangeResponseMismatch { .. }
			| Self::SequenceOverflow { .. } => false,

			// Untyped, so there is nothing to classify on.
			Self::Other(_) => false,
		}
	}
}

impl From<reqwest::Error> for Error {
	fn from(err: reqwest::Error) -> Self {
		Error::Reqwest(std::sync::Arc::new(err))
	}
}

impl From<std::io::Error> for Error {
	fn from(err: std::io::Error) -> Self {
		Error::Io(std::sync::Arc::new(err))
	}
}

impl From<anyhow::Error> for Error {
	fn from(err: anyhow::Error) -> Self {
		Error::Other(std::sync::Arc::new(err))
	}
}

/// Convenience alias for results from the HLS gateway.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
	use super::*;

	/// The import loop retries on this classification, so a malformed playlist ending up on the
	/// retryable side is an infinite loop that publishes nothing.
	#[test]
	fn only_transient_failures_are_retryable() {
		assert!(Error::Moq(moq_net::Error::Transport("connection lost".to_string())).is_retryable());
		assert!(Error::from(std::io::Error::from(std::io::ErrorKind::ConnectionReset)).is_retryable());

		for err in [
			Error::ParsePlaylist("not a playlist".to_string()),
			Error::NoVariants,
			Error::MissingMap,
			Error::InvalidPlaylistUrl,
			Error::SequenceOverflow {
				kind: SequenceKind::Media,
				value: u64::MAX,
			},
			Error::from(std::io::Error::from(std::io::ErrorKind::NotFound)),
		] {
			assert!(!err.is_retryable(), "{err} should be terminal");
		}
	}
}
