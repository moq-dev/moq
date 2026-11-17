//! How a gateway publishes the broadcasts it ingests.

use std::time::Duration;

/// How a gateway publishes what it ingests: the media tracks' max age, the allocator they
/// claim on, how long a cleanly ended broadcast stays up, and the cap on what the source sends.
///
/// RTMP, SRT, RTSP, and WHIP each take one per ingest, so the same knobs mean the same thing
/// across them.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Ingest {
	/// How long a superseded group of an ingested media track stays fresh, or `None` for no
	/// publisher limit. Defaults to [`MAX_AGE`](hang::container::MAX_AGE); see
	/// [`catalog::Config::with_max_age`](crate::catalog::Config::with_max_age).
	pub max_age: Option<Duration>,

	/// The connection allocator each ingested track claims its peak-hold bitrate on, so a
	/// co-resident encoder targets what is left. Defaults to unlimited; see
	/// [`catalog::Config::with_bandwidth`](crate::catalog::Config::with_bandwidth).
	pub bandwidth: moq_net::bandwidth::Allocator,

	/// How long a broadcast whose source ended cleanly stays up, so a subscriber that
	/// arrives after the end still gets the tail. Defaults to [`Linger::DEFAULT`](crate::Linger::DEFAULT);
	/// zero closes it at once.
	pub linger: Duration,

	/// A live cap on the media the source sends, or `None` (the default) to never cap it.
	///
	/// Only WHIP enforces it so far, steering the encoder with TIAS and REMB and closing a
	/// session that runs past the cap plus its burst; RTMP, SRT, and RTSP ingest ignore it.
	/// Clones share the cap, so every session handed a clone is capped at it, each on its own.
	pub rate: Option<moq_net::bandwidth::Limit>,
}

impl Ingest {
	/// The catalog config the ingested tracks are minted under.
	pub fn catalog(&self) -> crate::catalog::Config {
		crate::catalog::Config::default()
			.with_max_age(self.max_age)
			.with_bandwidth(self.bandwidth.clone())
	}
}

impl Default for Ingest {
	fn default() -> Self {
		Self {
			max_age: Some(hang::container::MAX_AGE),
			bandwidth: moq_net::bandwidth::Allocator::unlimited(),
			linger: crate::Linger::DEFAULT,
			rate: None,
		}
	}
}
