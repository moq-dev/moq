//! WebRTC ↔ MoQ gateway.
//!
//! Bridges WHIP (RFC 9725) and WHEP between WebRTC peers and
//! [`moq_net`] broadcasts. The crate is split along two orthogonal axes
//! so all four combinations can land independently:
//!
//! | | RTP-in (ingest into MoQ) | RTP-out (egress from MoQ) |
//! |---|---|---|
//! | HTTP server | [`Server::publish_router`] (WHIP server) | [`Server::subscribe_router`] (WHEP server) |
//! | HTTP client | [`Client::subscribe`] (WHEP client) | [`Client::publish`] (WHIP client) |
//!
//! The two HTTP-client paths and the two HTTP-server paths share a single
//! internal session driver and the same per-codec adapters; the per-direction
//! split lives in the (crate-private) ingest and egress sources.
//!
//! ## Embedding
//!
//! Build a [`Server`] over your own
//! [`OriginProducer`](moq_net::origin::Producer) /
//! [`OriginConsumer`](moq_net::origin::Consumer) and merge
//! [`Server::publish_router`] / [`Server::subscribe_router`] into your own axum
//! app, or dial out with [`Client`]. A command-line interface is provided by the
//! `moq-cli` binary, on top of this library.
//!
//! The bundled routers are unauthenticated: they derive the broadcast name from
//! the request path. To own the HTTP route and authorize requests yourself
//! (resolving the broadcast name from a verified token), skip the routers and
//! call [`whip::accept`] (ingest) / [`whep::accept`] (egress) from your own
//! handler. Return the [`Response::answer`] in your HTTP response, then run
//! [`Response::run`] to drive the media session for its lifetime.
//!
//! ## Bitstream gotcha
//!
//! The WebRTC ↔ MoQ shape conversion for H.264 and H.265 is handled by
//! `moq-mux` importers: str0m hands us Annex-B (start-code NALs with inline
//! parameter sets) and that's exactly what the importers want. AV1 uses the
//! shared OBU splitter/importer. Opus, VP8, and VP9 pass through.

#![warn(missing_docs)]

pub mod client;
pub mod server;

// Implementation detail modules: these carry the WebRTC/str0m plumbing (str0m
// `Rtc`, `Mid`/`Pt`, tokio channels, raw packet buffers) and are deliberately
// crate-private, so the public surface stays `Client`, `Server`,
// `whip`/`whep::accept`, and `Response`.
mod codec;
mod egress;
mod error;
mod ingest;
mod net;
mod sdp;
mod session;

/// Re-export of the underlying WebRTC stack, so consumers can name the str0m
/// types that surface through [`Error::Rtc`] / [`Error::RtcInput`] without adding
/// their own str0m dependency (and risking a version mismatch). A major str0m
/// bump is therefore a breaking change for this crate.
pub use str0m;

/// Re-export of the HTTP router stack, so consumers can merge the [`axum::Router`]
/// returned by [`Server::publish_router`] / [`Server::subscribe_router`] (and by
/// [`whip::router`] / [`whep::router`]) into their own app without adding their own
/// axum dependency (and risking a version mismatch). A major axum bump is therefore
/// a breaking change for this crate.
pub use axum;

/// Re-export of the URL type, so consumers can build the [`url::Url`] that
/// [`Client::subscribe`] / [`Client::publish`] dial without adding their own url
/// dependency (and risking a version mismatch). A major url bump is therefore a
/// breaking change for this crate.
pub use url;

pub use client::Client;
pub use error::*;
pub use server::{Response, Server, whep, whip};

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
