//! An [`Encoder`](super::Encoder) that owns the thread it runs on, so any
//! thread (or task) can drive it.
//!
//! Off macOS the encoder runs on a dedicated OS thread (mirroring
//! [`capture::pump`](crate::capture)): the Windows hardware encoder is a Media
//! Foundation MFT whose COM handles must be created, driven, and dropped all on
//! one thread (COM apartments are per-thread), and whose encode call blocks on
//! MFT events. Driving it inline on a tokio worker would unbalance the
//! per-thread COM refcount as the future migrates between workers and park a
//! worker on a stalled MFT. A synchronous caller has the same problem for the
//! same reason: an FFI object shared between threads opens the apartment on
//! whichever thread built it and closes it on whichever thread drops it.
//! Confining the whole encoder lifetime to one thread fixes both; frames are
//! `Send` there (Windows D3D11 textures and CPU I420 both are) and packets come
//! back over a channel.
//!
//! macOS keeps encoding inline: VideoToolbox has no COM apartment to balance and
//! doesn't block on an event loop, so a thread would only add a hop, and its
//! zero-copy `CVPixelBuffer` surface is `!Send` and couldn't cross to one anyway.

use super::Encoded;
use super::encoder::Config;
use crate::{Error, Frame};

#[cfg(target_os = "macos")]
use inline::Inner;
#[cfg(not(target_os = "macos"))]
use threaded::Inner;

/// An [`Encoder`](super::Encoder) confined to one thread, driven from anywhere.
///
/// Same shape as [`Encoder`](super::Encoder), one method at a time, except that
/// the calls are `async` and [`encode`](Self::encode) takes the frame by value
/// (it may cross a thread). Reach for this instead of an `Encoder` whenever the
/// encoder outlives a single thread's stack: an object shared across threads, an
/// FFI handle, a task that migrates between executor workers. An `Encoder` you
/// build, drive, and drop inside one function needs none of it.
///
/// Awaiting rather than blocking is the point: the codec runs on its own thread,
/// so the executor keeps its worker while a slow hardware encoder works through
/// a frame. A caller with no executor to yield to (an FFI boundary that must
/// return a result synchronously) blocks on these futures itself.
pub struct Sink(Inner);

impl Sink {
	/// Open an encoder for `config` on its own thread. Returns once the encoder
	/// is built (or its construction fails), so a bad config or a missing backend
	/// surfaces here rather than on the first frame.
	pub async fn open(config: &Config) -> Result<Self, Error> {
		Ok(Self(Inner::open(config).await?))
	}

	/// The encoder name in use, e.g. `"mediafoundation"`.
	pub fn name(&self) -> &str {
		self.0.name()
	}

	/// Ask for the next frame to be encoded as a keyframe, like
	/// [`Encoder::keyframe`](super::Encoder::keyframe).
	///
	/// Queued behind the frames already in flight rather than applied to
	/// whichever one the codec happens to be on, so it keys the next frame you
	/// pass to [`encode`](Self::encode). Only queues the request, so unlike the
	/// rest there is nothing to await.
	pub fn keyframe(&mut self) {
		self.0.keyframe();
	}

	/// Encode one frame, waiting for its access units.
	///
	/// Takes the frame by value because it may be moved to the encode thread.
	/// Otherwise [`Encoder::encode`](super::Encoder::encode): zero or more access
	/// units, each stamped with the frame it came from.
	pub async fn encode(&mut self, frame: Frame) -> Result<Vec<Encoded>, Error> {
		self.0.encode(frame).await
	}

	/// Retune the encoder, waiting for the backend's verdict. See
	/// [`Encoder::set_bitrate`](super::Encoder::set_bitrate) for what a failure
	/// means (not fatal: stop adapting, keep encoding).
	pub async fn set_bitrate(&mut self, bitrate: u64) -> Result<(), Error> {
		self.0.set_bitrate(bitrate).await
	}

	/// Empty the codec at a boundary the output has to respect, leaving it ready
	/// for the frames that follow. See [`Encoder::flush`](super::Encoder::flush).
	///
	/// A live track needs this at every group boundary: a backend that pipelines
	/// is still holding the last frames of a group when it ends, and they would
	/// otherwise surface in the next group ahead of its keyframe, where a
	/// subscriber joining there cannot decode them. Publishing frame by frame
	/// with no group structure needs none of it.
	pub async fn flush(&mut self) -> Result<Vec<Encoded>, Error> {
		self.0.flush().await
	}

	/// Drain the codec, returning every access unit it was still holding, and
	/// shut the encoder down.
	///
	/// Consumes the sink, like [`Encoder::finish`](super::Encoder::finish).
	/// Dropping a sink without this is fine and tears down just as cleanly, it
	/// just discards the tail: publish the returned frames before ending a track,
	/// or its last pictures never reach a subscriber.
	pub async fn finish(self) -> Result<Vec<Encoded>, Error> {
		self.0.finish().await
	}
}

#[cfg(not(target_os = "macos"))]
mod threaded {
	use std::thread::JoinHandle;

	use tokio::sync::{mpsc, oneshot};

	use super::super::Encoded;
	use super::super::encoder::{Config, Encoder};
	use crate::{Error, Frame};

	/// Work for the encode thread. Every variant goes down the same channel so a
	/// keyframe request or a bitrate change lands in order with the frames around
	/// it, rather than racing them.
	enum Request {
		/// A frame to encode, plus a oneshot to return the resulting access units
		/// (or an error) in order.
		Encode {
			frame: Frame,
			resp: oneshot::Sender<Result<Vec<Encoded>, Error>>,
		},
		/// Key the next frame. No reply: the encoder only records the request, so
		/// there is nothing to report and nothing to wait for.
		Keyframe,
		/// Retune to a new bitrate, reporting whether the backend took it so the
		/// caller can stop adapting against an encoder that can't. The round trip
		/// is affordable because the rate control policy only sends one of these
		/// when the target moves meaningfully, not per frame.
		SetBitrate {
			bitrate: u64,
			resp: oneshot::Sender<Result<(), Error>>,
		},
		/// Empty the codec at a group boundary, leaving it running. Unlike
		/// `Finish` the encoder survives, so this is served like any other
		/// request.
		Flush {
			resp: oneshot::Sender<Result<Vec<Encoded>, Error>>,
		},
		/// Drain the codec and shut down, returning the tail. Last request the
		/// thread serves: `Encoder::finish` consumes the encoder, so the loop has
		/// to break out rather than come back round for another frame.
		Finish {
			resp: oneshot::Sender<Result<Vec<Encoded>, Error>>,
		},
	}

	/// An [`Encoder`] running on its own thread. See the module docs.
	pub struct Inner {
		/// `Option` so `Drop` can drop the sender (signalling the thread to exit)
		/// before joining.
		tx: Option<mpsc::UnboundedSender<Request>>,
		handle: Option<JoinHandle<()>>,
		name: String,
	}

	impl Inner {
		pub async fn open(config: &Config) -> Result<Self, Error> {
			let (req_tx, mut req_rx) = mpsc::unbounded_channel::<Request>();
			let (ready_tx, ready_rx) = oneshot::channel::<Result<String, Error>>();
			let config = config.clone();

			let handle = std::thread::Builder::new()
				.name("moq-video-encode".into())
				.spawn(move || {
					let mut encoder = match Encoder::new(&config) {
						Ok(encoder) => encoder,
						Err(err) => {
							let _ = ready_tx.send(Err(err));
							return;
						}
					};
					// If the awaiting `open` was cancelled, give up before encoding.
					if ready_tx.send(Ok(encoder.name().to_string())).is_err() {
						return;
					}

					// Serve each request in arrival order. The encoder and its COM /
					// MFT handles are created, used, and dropped only on this thread.
					// `finish` consumes the encoder, so it breaks out and drains below
					// rather than serving another request.
					let mut draining = None;
					while let Some(req) = req_rx.blocking_recv() {
						match req {
							Request::Encode { frame, resp } => {
								let _ = resp.send(encoder.encode(&frame));
							}
							Request::Keyframe => encoder.keyframe(),
							Request::SetBitrate { bitrate, resp } => {
								let _ = resp.send(encoder.set_bitrate(bitrate));
							}
							Request::Flush { resp } => {
								let _ = resp.send(encoder.flush());
							}
							Request::Finish { resp } => {
								draining = Some(resp);
								break;
							}
						}
					}
					// The drain runs here, on this thread, and consumes the encoder;
					// otherwise `encoder` drops here. Either way the COM apartment it
					// opened closes on the thread that opened it.
					if let Some(resp) = draining {
						let _ = resp.send(encoder.finish());
					}
				})
				.map_err(|err| Error::Codec(anyhow::anyhow!("failed to spawn the encode thread: {err}")))?;

			match ready_rx.await {
				Ok(Ok(name)) => Ok(Self {
					tx: Some(req_tx),
					handle: Some(handle),
					name,
				}),
				Ok(Err(err)) => Err(err),
				Err(_) => {
					let _ = handle.join();
					Err(Error::Codec(anyhow::anyhow!("encode thread exited before opening")))
				}
			}
		}

		pub fn name(&self) -> &str {
			&self.name
		}

		pub fn keyframe(&mut self) {
			// Nothing to report: a dead encode thread surfaces on the next encode,
			// which is where the caller is already handling one.
			let _ = self.send(Request::Keyframe);
		}

		pub async fn encode(&mut self, frame: Frame) -> Result<Vec<Encoded>, Error> {
			self.request(|resp| Request::Encode { frame, resp }).await
		}

		pub async fn set_bitrate(&mut self, bitrate: u64) -> Result<(), Error> {
			self.request(|resp| Request::SetBitrate { bitrate, resp }).await
		}

		pub async fn flush(&mut self) -> Result<Vec<Encoded>, Error> {
			self.request(|resp| Request::Flush { resp }).await
		}

		pub async fn finish(self) -> Result<Vec<Encoded>, Error> {
			// `self` drops on the way out, which drops the sender and joins the
			// thread that just drained and released the encoder.
			self.request(|resp| Request::Finish { resp }).await
		}

		/// Queue a request, mapping a dead encode thread onto an error.
		fn send(&self, req: Request) -> Result<(), Error> {
			self.tx.as_ref().ok_or_else(gone)?.send(req).map_err(|_| gone())
		}

		/// Send a request built around a fresh oneshot and await its reply,
		/// mapping a dead encode thread onto an error either way.
		async fn request<T>(
			&self,
			build: impl FnOnce(oneshot::Sender<Result<T, Error>>) -> Request,
		) -> Result<T, Error> {
			let (resp_tx, resp_rx) = oneshot::channel();
			self.send(build(resp_tx))?;
			resp_rx.await.map_err(|_| gone())?
		}
	}

	impl Drop for Inner {
		fn drop(&mut self) {
			// Drop the sender so the thread's `blocking_recv` returns `None` and it
			// exits, dropping the encoder on its own thread; then join so teardown
			// (COM uninit) completes before we return. A wedged encode blocking the
			// thread would stall this join, the same tradeoff as the capture pump.
			self.tx.take();
			if let Some(handle) = self.handle.take() {
				let _ = handle.join();
			}
		}
	}

	fn gone() -> Error {
		Error::Codec(anyhow::anyhow!("encode thread stopped unexpectedly"))
	}
}

#[cfg(target_os = "macos")]
mod inline {
	use super::super::Encoded;
	use super::super::encoder::{Config, Encoder};
	use crate::{Error, Frame};

	/// An [`Encoder`] driven inline on the calling thread (see the module docs).
	pub struct Inner(Encoder);

	impl Inner {
		pub async fn open(config: &Config) -> Result<Self, Error> {
			Ok(Self(Encoder::new(config)?))
		}

		pub fn name(&self) -> &str {
			self.0.name()
		}

		pub fn keyframe(&mut self) {
			self.0.keyframe();
		}

		/// Async only to match the threaded `Inner`; there's no thread to hand this
		/// to, so it encodes inline. The same holds for the two below.
		pub async fn encode(&mut self, frame: Frame) -> Result<Vec<Encoded>, Error> {
			self.0.encode(&frame)
		}

		pub async fn set_bitrate(&mut self, bitrate: u64) -> Result<(), Error> {
			self.0.set_bitrate(bitrate)
		}

		pub async fn flush(&mut self) -> Result<Vec<Encoded>, Error> {
			self.0.flush()
		}

		pub async fn finish(self) -> Result<Vec<Encoded>, Error> {
			self.0.finish()
		}
	}
}

/// macOS is exempt by design: the inline sink encodes on the calling thread, so
/// there is no confinement to assert (see the module docs).
#[cfg(all(test, not(target_os = "macos")))]
mod tests {
	use std::collections::HashSet;
	use std::sync::{Arc, Mutex};
	use std::thread::ThreadId;

	use super::super::backend::probe;
	use super::super::{Codec, Kind};
	use super::*;
	use crate::{I420, Surface};

	/// A mid-gray frame at the probe backend's resolution, stamped as the
	/// `index`th frame of a 30fps stream.
	fn gray(index: u64) -> Frame {
		let i420 = I420::new(320, 240, vec![0x80u8; I420::len(320, 240)]).unwrap();
		Frame::new(
			Surface::I420(i420),
			moq_net::Timestamp::from_micros(index * 33_333).unwrap(),
		)
	}

	fn probe_config() -> Config {
		let mut config = Config::new(320, 240, 30);
		config.codec = Codec::H264;
		config.kind = Kind::Named(probe::NAME.into());
		config
	}

	/// Regression: the Windows backend opens a COM apartment on the thread that
	/// builds the codec and closes it on the thread that drops it, so a codec
	/// reachable from more than one thread has to own a thread of its own. Both
	/// FFI bindings held a bare `Encoder` and drove it from whichever thread
	/// called in, which leaked the opening thread's initialization and ran
	/// `CoUninitialize` on a thread that never initialized COM.
	///
	/// Asserted on every platform rather than only Windows: the confinement is
	/// what the bindings now rely on, so it should fail here rather than on a
	/// machine none of CI has.
	#[test]
	fn the_codec_stays_on_one_thread_however_it_is_driven() {
		let _ = probe::take();

		let sink = Arc::new(Mutex::new(Some(
			pollster::block_on(Sink::open(&probe_config())).unwrap(),
		)));

		// Drive it the way an FFI handle gets driven: a fresh caller thread every
		// time, none of them the thread that opened it.
		let mut callers = vec![std::thread::current().id()];
		let mut flushed = Vec::new();
		for index in 0..3u64 {
			let sink = sink.clone();
			let caller = std::thread::spawn(move || {
				let mut guard = sink.lock().unwrap();
				let sink = guard.as_mut().unwrap();
				sink.keyframe();
				pollster::block_on(sink.encode(gray(index))).unwrap();
				pollster::block_on(sink.set_bitrate(500_000 + index)).unwrap();
				// Only the first frame closes a group, so the two after it stay in
				// the codec and leave the drain below something to find.
				let flushed = match index {
					0 => pollster::block_on(sink.flush()).unwrap(),
					_ => Vec::new(),
				};
				(std::thread::current().id(), flushed)
			});
			let (caller, drained) = caller.join().unwrap();
			callers.push(caller);
			flushed.extend(drained);
		}

		// The probe holds each frame back by one, so a flush that reached the codec
		// hands back the frame the group ended on. An inherited no-op would return
		// nothing here and silently drop it into the next group.
		let flushed: Vec<_> = flushed.iter().map(|frame| frame.timestamp.as_micros()).collect();
		assert_eq!(flushed, vec![0], "the group boundary did not empty the codec");

		// ...and finished, so dropped, from yet another.
		let closer = std::thread::spawn(move || {
			let sink = sink.lock().unwrap().take().unwrap();
			let tail = pollster::block_on(sink.finish()).unwrap();
			(std::thread::current().id(), tail)
		});
		let (closer, tail) = closer.join().unwrap();
		callers.push(closer);

		// Frame 2 never came back from an encode call and no flush claimed it, so
		// the drain has to. Dropping the sink instead would lose it silently.
		let tail: Vec<_> = tail.iter().map(|frame| frame.timestamp.as_micros()).collect();
		assert_eq!(tail, vec![2 * 33_333], "the drain lost the codec's tail");

		let log = probe::take();
		for what in ["open", "encode", "set_bitrate", "flush", "finish", "drop"] {
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
