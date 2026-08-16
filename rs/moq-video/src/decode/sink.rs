//! A [`Decoder`](super::Decoder) that owns the thread it runs on, so any thread
//! (or task) can drive it.
//!
//! The decode-side mirror of [`encode::Sink`](crate::encode::Sink), for the same
//! reason: the Windows backend is a Media Foundation transform whose COM handles
//! must be created, driven, and dropped all on one thread (COM apartments are
//! per-thread). `unsafe impl Send for MediaFoundation` holds only while that is
//! true, and a decoder held across `.await` in a plain task does not keep it: the
//! future migrates between executor workers, so the apartment is opened on one
//! and closed on another.
//!
//! macOS keeps decoding inline: VideoToolbox has no COM apartment to balance, so
//! a thread would only add a hop, and its zero-copy `CVPixelBuffer` surface is
//! `!Send` and could not cross to one anyway.

use bytes::Bytes;
use hang::catalog::VideoConfig;
use moq_net::Timestamp;

use super::decoder::Config;
use crate::{Error, Frame};

#[cfg(target_os = "macos")]
use inline::Inner;
#[cfg(not(target_os = "macos"))]
use threaded::Inner;

/// A [`Decoder`](super::Decoder) confined to one thread, driven from anywhere.
///
/// Same shape as [`Decoder`](super::Decoder), except that
/// [`decode`](Self::decode) is `async` and takes its payload by value (it may
/// cross a thread). Reach for this instead of a `Decoder` whenever the decoder
/// outlives a single thread's stack: a spawned task, an object shared across
/// threads, an FFI handle. A `Decoder` you build, drive, and drop inside one
/// function needs none of it.
///
/// [`Consumer`](super::Consumer) is built on one, so a caller reading a
/// subscribed track from a task already gets this and needs nothing here.
///
/// # Cancellation
///
/// Not cancel-safe, and it says so rather than letting it slide. A queued decode
/// runs whether or not anyone is still waiting, so dropping the future (racing it
/// in a `select!`, giving it a timeout) leaves the decoder a step ahead of the
/// stream, holding frames nobody received. Rather than let the next call carry on
/// against a decoder that has moved, the sink refuses every call after a
/// cancelled one. Drop it and open another.
///
/// macOS never refuses, because there is no thread to run ahead: the decoder runs
/// inline, so a dropped future either had not started the call or had already
/// finished it. Write to the contract above regardless, or the same code loses
/// frames off macOS.
pub struct Sink(Inner);

impl Sink {
	/// Open a decoder for `catalog` on its own thread. Returns once the decoder
	/// is built (or its construction fails), so an unsupported codec surfaces
	/// here rather than on the first frame.
	pub async fn open(catalog: &VideoConfig, config: &Config) -> Result<Self, Error> {
		Ok(Self(Inner::open(catalog, config).await?))
	}

	/// The decoder name in use, e.g. `"mediafoundation"`.
	pub fn name(&self) -> &str {
		self.0.name()
	}

	/// Decode one access unit, waiting for whatever pictures it yields.
	///
	/// Otherwise [`Decoder::decode`](super::Decoder::decode): zero or more frames,
	/// since a backend that reorders holds pictures back. `payload` is a [`Bytes`],
	/// so handing it over is a refcount rather than a copy.
	pub async fn decode(&mut self, payload: Bytes, timestamp: Timestamp, keyframe: bool) -> Result<Vec<Frame>, Error> {
		self.0.decode(payload, timestamp, keyframe).await
	}
}

#[cfg(not(target_os = "macos"))]
mod threaded {
	use bytes::Bytes;
	use hang::catalog::VideoConfig;
	use moq_net::Timestamp;
	use tokio::sync::{mpsc, oneshot};

	use super::super::decoder::{Config, Decoder};
	use crate::worker::{Ready, Worker};
	use crate::{Error, Frame};

	/// Work for the decode thread. One variant today; the enum is here so a
	/// future request (a flush, a reset) lands in order with the frames around it
	/// rather than racing them.
	enum Request {
		Decode {
			payload: Bytes,
			timestamp: Timestamp,
			keyframe: bool,
			resp: oneshot::Sender<Result<Vec<Frame>, Error>>,
		},
	}

	/// Build a decoder and serve requests until the channel closes. Runs entirely
	/// on the decode thread; see [`crate::worker`].
	fn run(catalog: VideoConfig, config: Config, ready: Ready, mut requests: mpsc::UnboundedReceiver<Request>) {
		let mut decoder = match Decoder::new(&catalog, &config) {
			Ok(decoder) => decoder,
			Err(err) => return ready.err(err),
		};
		// If the awaiting `open` was cancelled, give up before decoding.
		if !ready.ok(decoder.name()) {
			return;
		}

		// Serve each request in arrival order. The decoder and its COM / MFT
		// handles are created, used, and dropped only on this thread.
		while let Some(req) = requests.blocking_recv() {
			match req {
				Request::Decode {
					payload,
					timestamp,
					keyframe,
					resp,
				} => {
					let _ = resp.send(decoder.decode(&payload, timestamp, keyframe));
				}
			}
		}
		// `decoder` drops here, on this thread, balancing the COM apartment.
	}

	/// A [`Decoder`] running on its own thread. See the module docs.
	pub struct Inner(Worker<Request>);

	impl Inner {
		pub async fn open(catalog: &VideoConfig, config: &Config) -> Result<Self, Error> {
			let catalog = catalog.clone();
			let config = config.clone();
			let worker = Worker::open("moq-video-decode", move |ready, requests| {
				run(catalog, config, ready, requests)
			})
			.await?;
			Ok(Self(worker))
		}

		pub fn name(&self) -> &str {
			self.0.name()
		}

		pub async fn decode(
			&mut self,
			payload: Bytes,
			timestamp: Timestamp,
			keyframe: bool,
		) -> Result<Vec<Frame>, Error> {
			self.0
				.request(|resp| Request::Decode {
					payload,
					timestamp,
					keyframe,
					resp,
				})
				.await
		}
	}
}

#[cfg(target_os = "macos")]
mod inline {
	use bytes::Bytes;
	use hang::catalog::VideoConfig;
	use moq_net::Timestamp;

	use super::super::decoder::{Config, Decoder};
	use crate::{Error, Frame};

	/// A [`Decoder`] driven inline on the calling thread (see the module docs).
	pub struct Inner(Decoder);

	impl Inner {
		pub async fn open(catalog: &VideoConfig, config: &Config) -> Result<Self, Error> {
			Ok(Self(Decoder::new(catalog, config)?))
		}

		pub fn name(&self) -> &str {
			self.0.name()
		}

		/// Async only to match the threaded `Inner`; there's no thread to hand
		/// this to, so it decodes inline.
		pub async fn decode(
			&mut self,
			payload: Bytes,
			timestamp: Timestamp,
			keyframe: bool,
		) -> Result<Vec<Frame>, Error> {
			self.0.decode(&payload, timestamp, keyframe)
		}
	}
}

/// macOS is exempt by design: the inline sink decodes on the calling thread, so
/// there is no confinement to assert (see the module docs).
#[cfg(all(test, not(target_os = "macos")))]
mod tests {
	use std::collections::HashSet;
	use std::sync::{Arc, Mutex};
	use std::thread::ThreadId;

	use super::super::Kind;
	use super::super::backend::probe;
	use super::*;

	fn probe_catalog() -> VideoConfig {
		let mut catalog = VideoConfig::new(hang::catalog::H264 {
			inline: true,
			profile: 0x42,
			constraints: 0,
			level: 30,
		});
		catalog.coded_width = Some(probe::SIZE.width);
		catalog.coded_height = Some(probe::SIZE.height);
		catalog
	}

	fn probe_config() -> Config {
		let mut config = Config::new();
		config.kind = Kind::Named(probe::NAME.into());
		config
	}

	fn at(index: u64) -> Timestamp {
		Timestamp::from_micros(index * 33_333).unwrap()
	}

	/// Regression: the Windows decoder opens a COM apartment on the thread that
	/// builds it and closes it on the thread that drops it. Every owner holds the
	/// codec across `.await` in a spawned task (`decode::Consumer`'s read loop,
	/// which libmoq drives; moq-transcode's feed and fetch pipeline), so the
	/// future migrates between executor workers and the apartment is opened on
	/// one and closed on another.
	#[test]
	fn the_codec_stays_on_one_thread_however_it_is_driven() {
		let _probe = probe::exclusive();

		let sink = Arc::new(Mutex::new(Some(
			pollster::block_on(Sink::open(&probe_catalog(), &probe_config())).unwrap(),
		)));

		// Drive it the way a migrating task does: a fresh caller thread every
		// time, none of them the one that opened it.
		let mut callers = vec![std::thread::current().id()];
		for index in 0..3u64 {
			let sink = sink.clone();
			let caller = std::thread::spawn(move || {
				let mut guard = sink.lock().unwrap();
				let sink = guard.as_mut().unwrap();
				let frames = pollster::block_on(sink.decode(Bytes::from_static(b"au"), at(index), index == 0)).unwrap();
				// The frame really came back, so the assertions below are about a
				// decoder that ran rather than one that no-opped.
				assert_eq!(frames.len(), 1);
				assert_eq!(frames[0].timestamp, at(index));
				std::thread::current().id()
			});
			callers.push(caller.join().unwrap());
		}

		// ...and dropped from yet another.
		let closer = std::thread::spawn(move || {
			sink.lock().unwrap().take();
			std::thread::current().id()
		});
		callers.push(closer.join().unwrap());

		let log = probe::take();
		for what in ["open", "decode", "drop"] {
			assert!(log.iter().any(|(event, _)| *event == what), "no {what} in {log:?}");
		}

		let threads: HashSet<ThreadId> = log.iter().map(|(_, id)| *id).collect();
		assert_eq!(threads.len(), 1, "the codec ran on more than one thread: {log:?}");

		let codec = threads.into_iter().next().unwrap();
		assert!(
			!callers.contains(&codec),
			"the codec ran on a caller's thread rather than its own: {log:?}"
		);
	}
}
