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

	/// History mode found no first group on this timeline, as in a retained (DVR) recording.
	#[error("history needs timeline {0} from its first group; retained recordings are not supported")]
	HistoryPruned(String),

	/// A range playlist was requested from a timeline whose media is not durable.
	#[error("range playlists require a durable timeline")]
	RangeNotDurable,

	/// A requested range is invalid or no longer fully retained.
	#[error("requested range is unavailable: {0}")]
	RangeUnavailable(String),

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

	/// An export generation had characters a URL path segment can't carry verbatim.
	#[error("invalid generation {0:?}: expected ASCII letters, digits, '-', or '_'")]
	InvalidGeneration(String),

	/// A playlist or segment URI could not be resolved against its base.
	#[error("url parse: {0}")]
	UrlParse(#[from] url::ParseError),

	/// HTTP error while fetching a playlist or segment.
	#[error("reqwest: {0}")]
	Reqwest(std::sync::Arc<reqwest::Error>),

	/// I/O error while reading a local playlist or segment.
	#[error("io: {0}")]
	Io(std::sync::Arc<std::io::Error>),

	/// An export timeline failed, so its listings refuse rather than read as a finished recording.
	#[error("timeline {track} failed: {source}")]
	TimelineFailed {
		/// The timeline's track name.
		track: String,
		/// Why the timeline failed.
		source: Box<Error>,
	},

	/// A media playlist already carried `EXT-X-ENDLIST` when import first saw it; import follows live playlists only.
	#[error("playlist already ended (EXT-X-ENDLIST); HLS import follows live playlists only")]
	PlaylistEnded,

	/// Import refused a rendition whose timestamps went backwards, such as a reset at an
	/// `EXT-X-DISCONTINUITY` under the same init segment. A restarted source is a new broadcast.
	#[error("rendition {rendition} rewound its timestamps; HLS import refuses a timestamp reset: {source}")]
	Rewind {
		/// The rendition's label.
		rendition: String,
		/// The rewind the importer refused.
		source: moq_mux::Error,
	},
	/// A recording source closed without confirming its complete end.
	#[error("recording closed before finishing")]
	IncompleteRecording,

	/// A recording cursor missed rows that left its window or were reset.
	#[error("recording cursor missed segments")]
	RecordingLagged,

	/// A listed recording segment no longer has all its media available.
	#[error("recording segment {segment} is unavailable")]
	SegmentUnavailable {
		/// The segment whose media cannot be recorded.
		segment: u64,
	},
}

impl Error {
	/// The HTTP status the origin answered with, if it answered with one at all.
	///
	/// The import loop consults it: a `503` on a playlist fetch is worth another pass, a `404` is
	/// the origin's settled answer. Nothing else here is classified; a failure with no status falls
	/// through to the backoff budget.
	pub fn status(&self) -> Option<u16> {
		match self {
			Self::Reqwest(err) => err.status().map(|status| status.as_u16()),
			_ => None,
		}
	}

	/// Whether this is the source's settled answer, which another import pass cannot change.
	///
	/// A response that arrived is the server's answer, and only a narrow set of statuses invites
	/// another attempt: request timeout, rate limit, and the gateway/overload statuses. Every other
	/// status, `404` and `403` included, is settled, as is a playlist that had already ended when
	/// the import joined it. A failure with no answer at all is left to the backoff budget.
	pub(crate) fn settled(&self) -> bool {
		match self {
			Self::PlaylistEnded => true,
			_ => self
				.status()
				.is_some_and(|status| !matches!(status, 408 | 429 | 502 | 503 | 504)),
		}
	}
}

impl From<reqwest::Error> for Error {
	fn from(err: reqwest::Error) -> Self {
		// reqwest prints the full URL in its error, and a playlist or segment URL
		// routinely carries a signed token in its query.
		Error::Reqwest(std::sync::Arc::new(err.without_url()))
	}
}

impl From<std::io::Error> for Error {
	fn from(err: std::io::Error) -> Self {
		Error::Io(std::sync::Arc::new(err))
	}
}

/// Convenience alias for results from the HLS gateway.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn http_error_redacts_url() {
		// A freed port refuses the connection, so reqwest fails with the dialed URL attached.
		let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
		let address = listener.local_addr().expect("local address");
		drop(listener);

		let err: Error = reqwest::get(format!("http://user:pass@{address}/media.m3u8?jwt=secret"))
			.await
			.expect_err("dial a closed port")
			.into();

		let printed = format!("{err} {err:?}");
		for secret in ["jwt", "secret", "user:pass"] {
			assert!(!printed.contains(secret), "error leaked {secret}: {printed}");
		}
	}
}
