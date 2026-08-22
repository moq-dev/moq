//! SRT gateway for MoQ, both directions.
//!
//! Runs an [SRT](https://www.haivision.com/products/srt-secure-reliable-transport/)
//! listener and routes each connection by its stream-id `m=` mode against a
//! [`moq_net::origin::Producer`]:
//!
//! - `m=publish` (the default): demux the MPEG-TS the connection carries with
//!   [`moq_mux`] and publish it into the origin as an ordinary broadcast. The
//!   contribution-ingest analogue of `moq-cli import ... hls` and `moq-rtc`'s WHIP.
//! - `m=request`: re-mux a broadcast from the origin back to MPEG-TS and stream
//!   it to the caller, so a plain SRT player (VLC, ffmpeg) can watch it.
//!
//! Two entry points, depending on how much control you need over each request:
//!
//! - [`run`]: the unauthenticated convenience. Build a [`Config`] and hand it
//!   plus an origin to [`run`]; it accepts every publisher and subscriber and
//!   routes by prefix + resource name. A relay embeds this with
//!   `run(cluster.origin.clone(), config)`.
//! - [`Server`] / [`Request`]: bring your own auth. Loop on [`Server::accept`],
//!   inspect [`Request::resource`] / [`Request::stream_id`] (treat the stream id
//!   as a token if you like), then match on the [`Request`]: accept a [`Publish`]
//!   into an origin, or accept a [`Subscribe`] out of one, at a path of your
//!   choosing (or reject it). This is how an embedder (e.g. a relay verifying a
//!   JWT and scoping the origin per token) plugs its policy in. It mirrors
//!   `moq-tokio`'s `Server` / `Request`.
//!
//! Beyond the listener, the [`dial`] module is the *dial-out* (client) role: build a
//! [`dial::Config`] naming a remote SRT listener and either [`dial::publish`] a MoQ
//! broadcast to it (restream MoQ out to a remote SRT ingest) or [`dial::pull`] a
//! remote stream into an origin (ingest a remote SRT source). It reuses the same
//! MPEG-TS <-> moq bridge; only the SRT caller transport is new.
//!
//! A command-line interface is provided by the `moq-cli` binary, on top of this
//! library.
//!
//! Pure Rust: SRT is provided by `srt-tokio`, with no libsrt or ffmpeg
//! dependency.

#![warn(missing_docs)]

pub mod dial;
mod error;
mod listen;
mod server;
mod ts;

pub use error::{Error, Result};
pub use listen::{Config, run};
pub use server::{Publish, Request, Server, Subscribe};

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
