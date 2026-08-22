//! HLS / LL-HLS <-> MoQ gateway.
//!
//! Bridges HLS (and Low-Latency HLS) and [`moq_net`] broadcasts in both
//! directions, mirroring the WHIP/WHEP split in `moq-rtc`:
//!
//! - [`import`] pulls a remote HLS master/media playlist and publishes its CMAF
//!   segments into MoQ (an HTTP *client* that *publishes*).
//! - [`server`] serves HLS playlists, DASH manifests, and CMAF segments over
//!   HTTP for MoQ broadcasts (an HTTP *server*). It subscribes only to each
//!   broadcast's catalog and timeline tracks; media bytes are FETCHed from
//!   the relay one group at a time, only when a segment is actually requested.
//!   It serves every request; gate access by layering your own middleware onto
//!   [`Server::router`](server::Server::router).
//!
//! All CMAF byte handling (import via [`moq_mux::container::fmp4::Import`],
//! export via [`moq_mux::container::fmp4::Muxer`]) lives in `moq-mux`; this
//! crate owns the HLS playlist and DASH manifest generation, the
//! timeline-driven playlist window, and the HTTP surface.

#![warn(missing_docs)]

mod error;
pub mod export;
pub mod import;
#[cfg(feature = "server")]
pub mod server;

pub(crate) use error::status_retryable;
pub use error::*;
#[cfg(feature = "server")]
pub use server::Server;

/// Re-export of the HTTP stack behind the export server, so consumers can name the
/// types that surface through [`Server::router`] (and layer their own middleware on
/// it) without adding their own axum dependency and risking a version mismatch.
/// `axum::http` covers the `http` crate types too. A major axum bump is therefore a
/// breaking change for this crate.
#[cfg(feature = "server")]
pub use axum;

/// Re-export of the HTTP client used by [`import`], so consumers can name the
/// [`reqwest::Error`] carried by [`Error::Reqwest`] without adding their own reqwest
/// dependency. A major reqwest bump is therefore a breaking change for this crate.
pub use reqwest;

/// Re-export of the URL parser, so consumers can name the [`url::Url`] and
/// [`url::ParseError`] carried by [`Error`] without adding their own url dependency.
/// A major url bump is therefore a breaking change for this crate.
pub use url;

/// Tokio-backed [`moq_net::Timers`] for tests that run an origin driver.
#[cfg(test)]
pub(crate) mod test_timers {
	use std::{pin::Pin, task::Poll};

	/// Hands out tokio sleeps; `now` reads tokio's (pausable) clock.
	#[derive(Clone, Copy)]
	pub(crate) struct Timers;

	impl moq_net::Timers for Timers {
		type Timer = Timer;

		fn timer(&self) -> Self::Timer {
			Timer { at: None, sleep: None }
		}

		fn now(&self) -> moq_net::runtime::Instant {
			tokio::time::Instant::now().into_std()
		}
	}

	/// A tokio sleep driven through the [`moq_net::runtime::Timer`] contract.
	pub(crate) struct Timer {
		at: Option<moq_net::runtime::Instant>,
		// Allocated on the first poll after arming: construction panics without
		// a live tokio time driver, and only the poll runs inside the runtime.
		sleep: Option<Pin<Box<tokio::time::Sleep>>>,
	}

	impl moq_net::runtime::Timer for Timer {
		fn set(&mut self, at: Option<moq_net::runtime::Instant>) {
			self.at = at;
			// Reuse the allocation when there is one; `reset` also clears the
			// elapsed state.
			if let (Some(at), Some(sleep)) = (at, &mut self.sleep) {
				sleep.as_mut().reset(tokio::time::Instant::from_std(at));
			}
		}

		fn poll(&mut self, waiter: &moq_net::kio::Waiter) -> Poll<()> {
			let Some(at) = self.at else { return Poll::Pending };
			let sleep = self
				.sleep
				.get_or_insert_with(|| Box::pin(tokio::time::sleep_until(tokio::time::Instant::from_std(at))));
			if sleep.is_elapsed() {
				return Poll::Ready(());
			}
			waiter.poll_future(sleep.as_mut())
		}
	}
}
