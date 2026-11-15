//! An [`Encoder`](super::Encoder) that owns the thread it runs on, so any
//! thread (or task) can drive it.
//!
//! Off macOS and iOS the encoder runs on a dedicated OS thread (mirroring the capture
//! pump): the Windows hardware encoder is a Media
//! Foundation MFT whose COM handles must be created, driven, and dropped all on
//! one thread (COM apartments are per-thread), and whose encode call blocks on
//! MFT events. Driving it inline on a tokio worker would unbalance the
//! per-thread COM refcount as the future migrates between workers and park a
//! worker on a stalled MFT. A synchronous caller has the same problem for the
//! same reason: an FFI object shared between threads opens the apartment on
//! whichever thread built it and closes it on whichever thread drops it.
//! Confining the whole encoder lifetime to one thread fixes both; frames are
//! `Send` there (Windows D3D11 textures and CPU I420 both are), and the thread
//! moves the packets each call readies into a queue the caller pops.
//!
//! macOS and iOS keep encoding inline: VideoToolbox has no COM apartment to balance and
//! doesn't block on an event loop, so a thread would only add a hop, and its
//! zero-copy `CVPixelBuffer` surface is `!Send` and couldn't cross to one anyway.

use std::sync::Arc;

use super::Encoded;
use super::encoder::{Applied, Config};
use crate::{Error, Frame};

#[cfg(apple)]
use inline::Inner;
#[cfg(not(apple))]
use threaded::Inner;

/// An [`Encoder`](super::Encoder) confined to one thread, driven from anywhere.
///
/// Same shape as [`Encoder`](super::Encoder), one method at a time, except that
/// the calls that reach the codec are `async` and [`encode`](Self::encode) takes
/// the frame by value (it may cross a thread). [`receive`](Self::receive) stays
/// synchronous: each `encode` and `flush` has already moved what the codec
/// readied to this side. Reach for this instead of an `Encoder` whenever the
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
/// holding output nobody collected. Rather than let the next call carry on and
/// publish a track quietly missing those frames, the sink refuses every call
/// after a cancelled one. Drop it and open another.
///
/// Racing an encode against a shutdown signal is fine, since the sink is on its
/// way out anyway. What does not work is cancelling one and carrying on.
///
/// Apple platforms never refuse, because there is no thread to run ahead: the encoder
/// runs inline, so a dropped future either had not started the call or had
/// already finished it. Write to the contract above regardless, or the same code
/// loses frames elsewhere.
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

	/// The latency and compression controls the backend applied, like the
	/// encoder's own report.
	pub fn applied(&self) -> Applied {
		self.0.applied()
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

	/// Submit one frame, waiting until the codec has taken it.
	///
	/// Otherwise [`Encoder::encode`](super::Encoder::encode): its access units come
	/// out of [`receive`](Self::receive), each stamped with the frame it came from.
	///
	/// One call at a time, so the sink never queues raw frames of its own: the
	/// only pictures in flight are the one being encoded and whatever the codec
	/// pipelines (none on NVENC or openh264). A source that outruns the codec
	/// should drop the raw frames it has not submitted yet, never encoded
	/// packets, which later frames depend on.
	///
	/// Takes ownership, since the frame may be moved to the encode thread, but
	/// takes it as anything that can become an [`Arc`] so a caller fanning one
	/// frame out to several encoders (a transcode ladder) hands over a clone of
	/// the handle rather than a copy of the pixels. Pass a [`Frame`] and it is
	/// wrapped for you.
	pub async fn encode(&mut self, frame: impl Into<Arc<Frame>>) -> Result<(), Error> {
		self.0.encode(frame.into()).await
	}

	/// Pop the oldest ready access unit, or `None` while the codec holds the rest.
	///
	/// Call until it returns `None` after each [`encode`](Self::encode) and
	/// [`flush`](Self::flush).
	pub fn receive(&mut self) -> Result<Option<Encoded>, Error> {
		self.0.receive()
	}

	/// Retune the encoder, waiting for the backend's verdict. See
	/// [`Encoder::set_bitrate`](super::Encoder::set_bitrate) for what a failure
	/// means (not fatal: stop adapting, keep encoding).
	pub async fn set_bitrate(&mut self, bitrate: moq_net::bandwidth::Rate) -> Result<(), Error> {
		self.0.set_bitrate(bitrate).await
	}

	/// Mark the end of input, so [`receive`](Self::receive) returns every access
	/// unit the codec still holds, leaving it ready for the frames that follow.
	/// See [`Encoder::flush`](super::Encoder::flush).
	///
	/// A live track needs this at every group boundary: a backend that pipelines
	/// is still holding the last frames of a group when it ends, and they would
	/// otherwise surface in the next group ahead of its keyframe, where a
	/// subscriber joining there cannot decode them. Publishing frame by frame
	/// with no group structure needs none of it.
	///
	/// Flush and receive before dropping the sink, or its last pictures never
	/// reach a subscriber.
	pub async fn flush(&mut self) -> Result<(), Error> {
		self.0.flush().await
	}
}

#[cfg(not(apple))]
mod threaded {
	use std::sync::Arc;

	use tokio::sync::{mpsc, oneshot};

	use super::super::Encoded;
	use super::super::encoder::{Applied, Config, Encoder};
	use crate::worker::{Queue, Ready, Worker};
	use crate::{Error, Frame};

	/// Work for the encode thread. Every variant goes down the same channel so a
	/// cut or a bitrate change lands in order with the frames around it, rather
	/// than racing them.
	enum Request {
		/// A frame to encode, plus a oneshot to report, in order, whether the codec
		/// took it.
		Encode {
			frame: Arc<Frame>,
			resp: oneshot::Sender<Result<(), Error>>,
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
		/// Mark the end of input at a group boundary, leaving the codec running.
		Flush { resp: oneshot::Sender<Result<(), Error>> },
	}

	/// What the encode thread reports once its encoder is open.
	pub struct Opened {
		name: String,
		applied: Applied,
	}

	/// Move everything the encoder has ready into `queue`.
	fn collect(encoder: &mut Encoder, queue: &Queue<Encoded>) -> Result<(), Error> {
		let mut queue = queue.lock();
		while let Some(encoded) = encoder.receive()? {
			queue.push_back(encoded);
		}
		Ok(())
	}

	/// Build an encoder for `config` and serve requests until the channel closes.
	/// Runs entirely on the encode thread; see [`crate::worker`].
	fn run(
		config: Config,
		queue: Queue<Encoded>,
		ready: Ready<Opened>,
		mut requests: mpsc::UnboundedReceiver<Request>,
	) {
		let mut encoder = match Encoder::new(&config) {
			Ok(encoder) => encoder,
			Err(err) => return ready.err(err),
		};
		// If the awaiting `open` was cancelled, give up before encoding.
		let opened = Opened {
			name: encoder.name().to_owned(),
			applied: encoder.applied(),
		};
		if !ready.ok(opened) {
			return;
		}

		// Serve each request in arrival order. The encoder and its COM / MFT
		// handles are created, used, and dropped only on this thread, which the
		// encoder drops on once the channel closes.
		while let Some(req) = requests.blocking_recv() {
			match req {
				Request::Encode { frame, resp } => {
					let encoded = encoder.encode(&frame).and_then(|()| collect(&mut encoder, &queue));
					let _ = resp.send(encoded);
				}
				Request::Cut { resp } => {
					let _ = resp.send(encoder.cut());
				}
				Request::SetBitrate { bitrate, resp } => {
					let _ = resp.send(encoder.set_bitrate(bitrate));
				}
				Request::Flush { resp } => {
					let flushed = encoder.flush().and_then(|()| collect(&mut encoder, &queue));
					let _ = resp.send(flushed);
				}
			}
		}
	}

	/// An [`Encoder`] running on its own thread. See the module docs.
	pub struct Inner {
		worker: Worker<Request, Opened>,
		queue: Queue<Encoded>,
	}

	impl Inner {
		pub async fn open(config: &Config) -> Result<Self, Error> {
			let config = config.clone();
			let queue = Queue::default();
			let filled = queue.clone();
			let worker = Worker::open("moq-video-encode", move |ready, requests| {
				run(config, filled, ready, requests)
			})
			.await?;
			Ok(Self { worker, queue })
		}

		pub fn name(&self) -> &str {
			&self.worker.info().name
		}

		pub fn applied(&self) -> Applied {
			self.worker.info().applied.clone()
		}

		pub async fn cut(&mut self) -> Result<(), Error> {
			self.worker.request(|resp| Request::Cut { resp }).await
		}

		pub async fn encode(&mut self, frame: Arc<Frame>) -> Result<(), Error> {
			self.worker.request(|resp| Request::Encode { frame, resp }).await
		}

		pub fn receive(&mut self) -> Result<Option<Encoded>, Error> {
			Ok(self.queue.pop())
		}

		pub async fn set_bitrate(&mut self, bitrate: moq_net::bandwidth::Rate) -> Result<(), Error> {
			self.worker.request(|resp| Request::SetBitrate { bitrate, resp }).await
		}

		pub async fn flush(&mut self) -> Result<(), Error> {
			self.worker.request(|resp| Request::Flush { resp }).await
		}
	}
}

#[cfg(apple)]
mod inline {
	use std::sync::Arc;

	use super::super::Encoded;
	use super::super::encoder::{Applied, Config, Encoder};
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

		pub fn applied(&self) -> Applied {
			self.0.applied()
		}

		/// Async only to match the threaded `Inner`; there's no thread to hand this
		/// to, so it runs inline. The same holds for the calls below.
		pub async fn cut(&mut self) -> Result<(), Error> {
			self.0.cut()
		}

		pub async fn encode(&mut self, frame: Arc<Frame>) -> Result<(), Error> {
			self.0.encode(&frame)
		}

		pub fn receive(&mut self) -> Result<Option<Encoded>, Error> {
			self.0.receive()
		}

		pub async fn set_bitrate(&mut self, bitrate: moq_net::bandwidth::Rate) -> Result<(), Error> {
			self.0.set_bitrate(bitrate)
		}

		pub async fn flush(&mut self) -> Result<(), Error> {
			self.0.flush()
		}
	}
}

#[cfg(test)]
mod tests {
	#[cfg(not(apple))]
	use std::collections::HashSet;
	#[cfg(not(apple))]
	use std::sync::{Arc, Mutex};
	#[cfg(not(apple))]
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
		let mut config = Config::new(320, 240, crate::FrameRate::new(30, 1).unwrap());
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
		assert!(!sink.applied().cut, "the report says it cannot cut");

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
	/// Apple platforms are exempt by design: the inline sink encodes on the calling thread,
	/// so there is nothing to run ahead (see the module docs).
	#[cfg(not(apple))]
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
	/// machine none of CI has. Apple platforms are exempt by design: the inline sink has
	/// no thread of its own to confine anything to.
	#[cfg(not(apple))]
	#[test]
	fn the_codec_stays_on_one_thread_however_it_is_driven() {
		let _probe = probe::exclusive();

		let sink = Arc::new(Mutex::new(Some(
			pollster::block_on(Sink::open(&probe_config())).unwrap(),
		)));

		// Drive it the way an FFI handle gets driven: a fresh caller thread every
		// time, none of them the thread that opened it.
		let mut callers = vec![std::thread::current().id()];
		let mut received = Vec::new();
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
				if index == 0 {
					pollster::block_on(sink.flush()).unwrap();
				}
				let mut received = Vec::new();
				while let Some(frame) = sink.receive().unwrap() {
					received.push(frame.timestamp.as_micros().unwrap());
				}
				(std::thread::current().id(), received)
			});
			let (caller, frames) = caller.join().unwrap();
			callers.push(caller);
			received.extend(frames);
		}

		// The probe holds each frame back by one, so a flush that reached the codec
		// hands back the frame the group ended on, ahead of frame 1, which frame 2
		// pushed out. An inherited no-op would leave frame 0 to surface after the
		// boundary, in the next group.
		assert_eq!(received, vec![0, 33_333], "the group boundary did not empty the codec");

		// ...and drained, then dropped, from yet another.
		let closer = std::thread::spawn(move || {
			let mut sink = sink.lock().unwrap().take().unwrap();
			pollster::block_on(sink.flush()).unwrap();
			let mut tail = Vec::new();
			while let Some(frame) = sink.receive().unwrap() {
				tail.push(frame.timestamp.as_micros().unwrap());
			}
			drop(sink);
			(std::thread::current().id(), tail)
		});
		let (closer, tail) = closer.join().unwrap();
		callers.push(closer);

		// Frame 2 never came back from an encode call and no flush claimed it, so
		// the drain has to. Dropping the sink instead would lose it silently.
		assert_eq!(tail, vec![2 * 33_333], "the drain lost the codec's tail");

		let log = probe::take();
		for what in ["open", "encode", "set_bitrate", "flush", "drop"] {
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
