//! An [`Encoder`](super::Encoder) that owns the thread it runs on, so any
//! thread (or task) can drive it.
//!
//! Off macOS the encoder runs on a dedicated OS thread (mirroring the capture
//! pump): the Windows hardware encoder is a Media
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

use std::sync::Arc;

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
///
/// # Cancellation
///
/// These futures are not cancel-safe, and the sink says so rather than letting
/// it slide. The codec runs on its own thread, so a request that has been queued
/// runs whether or not anyone is still waiting: dropping the future (racing it in
/// a `select!`, giving it a timeout) leaves the codec a step ahead of the stream,
/// holding output nobody received. Rather than let the next call carry on and
/// publish a track quietly missing those frames, the sink refuses every call
/// after a cancelled one. Drop it and open another.
///
/// Racing an encode against a shutdown signal is fine, since the sink is on its
/// way out anyway. What does not work is cancelling one and carrying on.
///
/// macOS never refuses, because there is no thread to run ahead: the encoder
/// runs inline, so a dropped future either had not started the call or had
/// already finished it. Write to the contract above regardless, or the same code
/// loses frames off macOS.
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

	/// Cut a new group at the next frame, like
	/// [`Encoder::cut`](super::Encoder::cut).
	///
	/// Queued behind the frames already in flight rather than applied to
	/// whichever one the codec happens to be on, so it opens the group at the
	/// next frame you pass to [`encode`](Self::encode). Awaited for the
	/// backend's verdict: a backend that cannot cut refuses here with
	/// [`Error::CutUnsupported`](crate::Error::CutUnsupported), the same answer
	/// the direct encoder gives, rather than queueing a request it will ignore.
	pub async fn cut(&mut self) -> Result<(), Error> {
		self.0.cut().await
	}

	/// Encode one frame, waiting for its access units.
	///
	/// Otherwise [`Encoder::encode`](super::Encoder::encode): zero or more access
	/// units, each stamped with the frame it came from.
	///
	/// Takes ownership, since the frame may be moved to the encode thread, but
	/// takes it as anything that can become an [`Arc`] so a caller fanning one
	/// frame out to several encoders (a transcode ladder) hands over a clone of
	/// the handle rather than a copy of the pixels. Pass a [`Frame`] and it is
	/// wrapped for you.
	pub async fn encode(&mut self, frame: impl Into<Arc<Frame>>) -> Result<Vec<Encoded>, Error> {
		self.0.encode(frame.into()).await
	}

	/// Retune the encoder, waiting for the backend's verdict. See
	/// [`Encoder::set_bitrate`](super::Encoder::set_bitrate) for what a failure
	/// means (not fatal: stop adapting, keep encoding).
	pub async fn set_bitrate(&mut self, bitrate: moq_net::bandwidth::Rate) -> Result<(), Error> {
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
	use std::sync::Arc;

	use tokio::sync::{mpsc, oneshot};

	use super::super::Encoded;
	use super::super::encoder::{Config, Encoder};
	use crate::worker::{Ready, Worker};
	use crate::{Error, Frame};

	/// Work for the encode thread. Every variant goes down the same channel so a
	/// cut or a bitrate change lands in order with the frames around it, rather
	/// than racing them.
	enum Request {
		/// A frame to encode, plus a oneshot to return the resulting access units
		/// (or an error) in order.
		Encode {
			frame: Arc<Frame>,
			resp: oneshot::Sender<Result<Vec<Encoded>, Error>>,
		},
		/// Cut a group at the next frame, reporting whether the backend can: a
		/// refusal has to reach the caller, since the alternative is a group
		/// boundary that silently never happens.
		Cut { resp: oneshot::Sender<Result<(), Error>> },
		/// Retune to a new bitrate, reporting whether the backend took it so the
		/// caller can stop adapting against an encoder that can't. The round trip
		/// is affordable because the rate control policy only sends one of these
		/// when the target moves meaningfully, not per frame.
		SetBitrate {
			bitrate: moq_net::bandwidth::Rate,
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

	/// Build an encoder for `config` and serve requests until the channel closes.
	/// Runs entirely on the encode thread; see [`crate::worker`].
	fn run(config: Config, ready: Ready, mut requests: mpsc::UnboundedReceiver<Request>) {
		let mut encoder = match Encoder::new(&config) {
			Ok(encoder) => encoder,
			Err(err) => return ready.err(err),
		};
		// If the awaiting `open` was cancelled, give up before encoding.
		if !ready.ok(encoder.name()) {
			return;
		}

		// Serve each request in arrival order. The encoder and its COM / MFT
		// handles are created, used, and dropped only on this thread. `finish`
		// consumes the encoder, so it breaks out and drains below rather than
		// serving another request.
		let mut draining = None;
		while let Some(req) = requests.blocking_recv() {
			match req {
				Request::Encode { frame, resp } => {
					let _ = resp.send(encoder.encode(&frame));
				}
				Request::Cut { resp } => {
					let _ = resp.send(encoder.cut());
				}
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
		// The drain runs here, on this thread, and consumes the encoder; otherwise
		// `encoder` drops here. Either way the COM apartment it opened closes on
		// the thread that opened it.
		if let Some(resp) = draining {
			let _ = resp.send(encoder.finish());
		}
	}

	/// An [`Encoder`] running on its own thread. See the module docs.
	pub struct Inner(Worker<Request>);

	impl Inner {
		pub async fn open(config: &Config) -> Result<Self, Error> {
			let config = config.clone();
			let worker = Worker::open("moq-video-encode", move |ready, requests| run(config, ready, requests)).await?;
			Ok(Self(worker))
		}

		pub fn name(&self) -> &str {
			self.0.name()
		}

		pub async fn cut(&mut self) -> Result<(), Error> {
			self.0.request(|resp| Request::Cut { resp }).await
		}

		pub async fn encode(&mut self, frame: Arc<Frame>) -> Result<Vec<Encoded>, Error> {
			self.0.request(|resp| Request::Encode { frame, resp }).await
		}

		pub async fn set_bitrate(&mut self, bitrate: moq_net::bandwidth::Rate) -> Result<(), Error> {
			self.0.request(|resp| Request::SetBitrate { bitrate, resp }).await
		}

		pub async fn flush(&mut self) -> Result<Vec<Encoded>, Error> {
			self.0.request(|resp| Request::Flush { resp }).await
		}

		pub async fn finish(mut self) -> Result<Vec<Encoded>, Error> {
			// `self` drops on the way out, which drops the sender and joins the
			// thread that just drained and released the encoder.
			self.0.request(|resp| Request::Finish { resp }).await
		}
	}
}

#[cfg(target_os = "macos")]
mod inline {
	use std::sync::Arc;

	use super::super::Encoded;
	use super::super::encoder::{Config, Encoder};
	use crate::{Error, Frame};

	/// An [`Encoder`] driven inline on the calling thread (see the module docs).
	pub struct Inner(Encoder);

	// SAFETY: VideoToolbox and Core Foundation handles may move between threads
	// when calls remain serialized. `Sink` provides that serialization; the
	// synchronous `Encoder` remains thread-bound.
	unsafe impl Send for Inner {}

	impl Inner {
		pub async fn open(config: &Config) -> Result<Self, Error> {
			Ok(Self(Encoder::new(config)?))
		}

		pub fn name(&self) -> &str {
			self.0.name()
		}

		/// Async only to match the threaded `Inner`; there's no thread to hand this
		/// to, so it runs inline. The same holds for the calls below.
		pub async fn cut(&mut self) -> Result<(), Error> {
			self.0.cut()
		}

		pub async fn encode(&mut self, frame: Arc<Frame>) -> Result<Vec<Encoded>, Error> {
			self.0.encode(&frame)
		}

		pub async fn set_bitrate(&mut self, bitrate: moq_net::bandwidth::Rate) -> Result<(), Error> {
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

#[cfg(test)]
mod tests {
	#[cfg(not(target_os = "macos"))]
	use std::collections::HashSet;
	#[cfg(not(target_os = "macos"))]
	use std::sync::{Arc, Mutex};
	#[cfg(not(target_os = "macos"))]
	use std::thread::ThreadId;

	use super::super::backend::probe;
	use super::super::{Codec, Kind};
	use super::*;
	use crate::{I420, Surface};

	/// A mid-gray frame at the probe backend's resolution, stamped as the
	/// `index`th frame of a 30fps stream.
	fn gray(index: u64) -> Frame {
		let size = crate::Size::new(320, 240);
		let i420 = I420::new(size, vec![0x80u8; I420::len(size).unwrap()]).unwrap();
		Frame::new(
			Surface::I420(i420),
			moq_net::Timestamp::from_micros(index * 33_333).unwrap(),
		)
	}

	fn probe_config() -> Config {
		let mut config = Config::new(320, 240, crate::Rate::new(30, 1).unwrap());
		config.codec = Codec::H264;
		config.kind = Kind::Named(probe::NAME.into());
		config
	}

	/// The sink and the direct encoder answer a cut the same way: queued for the
	/// next frame on a backend that can, and that frame is the one the codec
	/// sees it on rather than whichever it was busy with.
	#[test]
	fn a_cut_lands_on_the_next_frame() {
		let _probe = probe::exclusive();

		let mut sink = pollster::block_on(Sink::open(&probe_config())).unwrap();
		pollster::block_on(sink.encode(gray(0))).unwrap();
		pollster::block_on(sink.cut()).unwrap();
		pollster::block_on(sink.encode(gray(1))).unwrap();
		pollster::block_on(sink.encode(gray(2))).unwrap();
		drop(sink);

		let events: Vec<_> = probe::take()
			.into_iter()
			.map(|(event, _)| event)
			.filter(|event| matches!(*event, "encode" | "cut"))
			.collect();
		assert_eq!(events, vec!["encode", "encode", "cut", "encode"]);
	}

	/// A backend that cannot cut refuses through the sink exactly as it does
	/// directly, and the sink stays usable: the refusal is the caller's to act
	/// on, not a poisoned session.
	#[test]
	fn a_backend_that_cannot_cut_refuses_through_the_sink() {
		let _probe = probe::exclusive();

		let mut config = probe_config();
		config.kind = Kind::Named(probe::NO_CUT.into());
		let mut sink = pollster::block_on(Sink::open(&config)).unwrap();
		assert_eq!(sink.name(), probe::NO_CUT);

		let err = pollster::block_on(sink.cut()).expect_err("the backend cannot cut");
		assert!(
			matches!(err, Error::CutUnsupported(name) if name == probe::NO_CUT),
			"unexpected error: {err:?}"
		);

		pollster::block_on(sink.encode(gray(0))).unwrap();
		drop(sink);
		let log = probe::take();
		assert!(
			!log.iter().any(|(event, _)| *event == "cut"),
			"a refused cut still reached the codec: {log:?}"
		);
	}

	/// Regression: a queued request runs on the encode thread whether or not the
	/// caller is still waiting, so a cancelled `encode` leaves the codec a step
	/// ahead of the stream with output nobody received. Carrying on would publish
	/// a track quietly missing those frames, which is worse than an error: only
	/// the publisher could ever tell, and only by decoding its own output.
	///
	/// macOS is exempt by design: the inline sink encodes on the calling thread,
	/// so there is nothing to run ahead (see the module docs).
	#[cfg(not(target_os = "macos"))]
	#[test]
	fn a_cancelled_call_poisons_the_sink() {
		let _probe = probe::exclusive();

		let mut sink = pollster::block_on(Sink::open(&probe_config())).unwrap();

		// Cancel an encode the moment it starts waiting, the shape a `select!` or a
		// timeout produces. Holding the codec inside the call is what makes the
		// cancel land mid-flight rather than race the encode thread for it.
		let gate = probe::hold();
		pollster::block_on(async {
			let mut encode = Box::pin(sink.encode(gray(0)));
			assert!(
				futures::poll!(encode.as_mut()).is_pending(),
				"the encode should still be waiting on the held codec"
			);
			// Dropped here, with the request queued and the reply still to come.
		});
		drop(gate);

		// The codec really did run, so the stream is missing whatever came back.
		let err = pollster::block_on(sink.encode(gray(1))).expect_err("the sink should refuse");
		assert!(err.to_string().contains("cancelled"), "unexpected error: {err}");
		// ...and it stays refused rather than recovering on the call after.
		assert!(pollster::block_on(sink.flush()).is_err());

		drop(sink);
		let log = probe::take();
		assert!(
			log.iter().any(|(event, _)| *event == "encode"),
			"the cancelled request should still have reached the codec: {log:?}"
		);
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
	/// machine none of CI has. macOS is exempt by design: the inline sink has
	/// no thread of its own to confine anything to.
	#[cfg(not(target_os = "macos"))]
	#[test]
	fn the_codec_stays_on_one_thread_however_it_is_driven() {
		let _probe = probe::exclusive();

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
				pollster::block_on(sink.cut()).unwrap();
				pollster::block_on(sink.encode(gray(index))).unwrap();
				pollster::block_on(sink.set_bitrate(moq_net::bandwidth::Rate::from_bps(500_000 + index))).unwrap();
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
