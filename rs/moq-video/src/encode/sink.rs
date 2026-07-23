//! The encoder as [`publish_capture`](super::publish_capture)'s capture loop
//! drives it, abstracting over where the encode actually runs.
//!
//! Off macOS the encoder runs on a dedicated OS thread (mirroring
//! [`capture::pump`](crate::capture)): the Windows hardware encoder is a Media
//! Foundation MFT whose COM handles must be created, driven, and dropped all on
//! one thread (COM apartments are per-thread), and whose encode call blocks on
//! MFT events. Driving it inline on a tokio worker would unbalance the
//! per-thread COM refcount as the future migrates between workers and park a
//! worker on a stalled MFT. Confining the whole encoder lifetime to one thread
//! fixes both; frames are `Send` there (Windows D3D11 textures and CPU I420 both
//! are) and packets come back over a channel.
//!
//! macOS keeps encoding inline: VideoToolbox has no COM apartment to balance and
//! doesn't block on an event loop, so a thread would only add a hop, and its
//! zero-copy `CVPixelBuffer` surface is `!Send` and couldn't cross to one anyway.

#[cfg(not(target_os = "macos"))]
pub(super) use threaded::Sink;

#[cfg(target_os = "macos")]
pub(super) use inline::Sink;

#[cfg(not(target_os = "macos"))]
mod threaded {
	use std::thread::JoinHandle;

	use bytes::Bytes;
	use tokio::sync::{mpsc, oneshot};

	use super::super::encoder::{self, Encoder};
	use crate::Error;
	use crate::frame::Surface;

	/// Work for the encode thread. Both variants go down the same channel so a
	/// bitrate change lands in order with the frames around it, rather than
	/// racing them.
	enum Request {
		/// A frame and whether to force a keyframe, plus a oneshot to return that
		/// frame's packets (or an error) in order.
		Encode {
			frame: Surface,
			keyframe: bool,
			resp: oneshot::Sender<Result<Vec<Bytes>, Error>>,
		},
		/// Retune to a new bitrate, reporting whether the backend took it so the
		/// caller can stop adapting against an encoder that can't. The round trip
		/// is affordable because the rate control policy only sends one of these
		/// when the target moves meaningfully, not per frame.
		SetBitrate {
			bitrate: u64,
			resp: oneshot::Sender<Result<(), Error>>,
		},
	}

	/// An [`Encoder`] running on its own thread. See the module docs.
	pub(in crate::encode) struct Sink {
		/// `Option` so `Drop` can drop the sender (signalling the thread to exit)
		/// before joining.
		tx: Option<mpsc::UnboundedSender<Request>>,
		handle: Option<JoinHandle<()>>,
		name: String,
	}

	impl Sink {
		/// Open an encoder for `config` on a dedicated thread. Returns once the
		/// encoder is built (or its construction fails), so a bad config or a
		/// missing backend surfaces here rather than on the first frame.
		pub(in crate::encode) async fn open(config: &encoder::Config) -> Result<Self, Error> {
			let (req_tx, mut req_rx) = mpsc::unbounded_channel::<Request>();
			let (ready_tx, ready_rx) = oneshot::channel::<Result<String, Error>>();
			let config = config.clone();

			let handle = std::thread::spawn(move || {
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
				while let Some(req) = req_rx.blocking_recv() {
					match req {
						Request::Encode { frame, keyframe, resp } => {
							let _ = resp.send(encoder.encode(&frame, keyframe));
						}
						Request::SetBitrate { bitrate, resp } => {
							let _ = resp.send(encoder.set_bitrate(bitrate));
						}
					}
				}
				// `encoder` drops here, on this thread, balancing the COM apartment.
			});

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

		/// The encoder name in use, e.g. `"mediafoundation"`.
		pub(in crate::encode) fn name(&self) -> &str {
			&self.name
		}

		/// Encode one frame, awaiting its packets. The frame is moved to the
		/// encode thread; the result returns over a oneshot.
		pub(in crate::encode) async fn encode(&mut self, frame: Surface, keyframe: bool) -> Result<Vec<Bytes>, Error> {
			self.request(|resp| Request::Encode { frame, keyframe, resp }).await
		}

		/// Retune the encoder, awaiting the backend's verdict.
		pub(in crate::encode) async fn set_bitrate(&mut self, bitrate: u64) -> Result<(), Error> {
			self.request(|resp| Request::SetBitrate { bitrate, resp }).await
		}

		/// Send a request built around a fresh oneshot and await its reply,
		/// mapping a dead encode thread onto an error either way.
		async fn request<T>(
			&self,
			build: impl FnOnce(oneshot::Sender<Result<T, Error>>) -> Request,
		) -> Result<T, Error> {
			let (resp_tx, resp_rx) = oneshot::channel();
			self.tx
				.as_ref()
				.ok_or_else(gone)?
				.send(build(resp_tx))
				.map_err(|_| gone())?;
			resp_rx.await.map_err(|_| gone())?
		}
	}

	impl Drop for Sink {
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
	use bytes::Bytes;

	use super::super::encoder::{self, Encoder};
	use crate::Error;
	use crate::frame::Surface;

	/// An [`Encoder`] driven inline on the capture task (see the module docs).
	pub(in crate::encode) struct Sink(Encoder);

	impl Sink {
		pub(in crate::encode) async fn open(config: &encoder::Config) -> Result<Self, Error> {
			Ok(Self(Encoder::new(config)?))
		}

		/// The encoder name in use, e.g. `"videotoolbox"`.
		pub(in crate::encode) fn name(&self) -> &str {
			self.0.name()
		}

		pub(in crate::encode) async fn encode(&mut self, frame: Surface, keyframe: bool) -> Result<Vec<Bytes>, Error> {
			self.0.encode(&frame, keyframe)
		}

		/// Retune the encoder. Async only to match the threaded `Sink`; there's
		/// no thread to hand this to, so it applies inline.
		pub(in crate::encode) async fn set_bitrate(&mut self, bitrate: u64) -> Result<(), Error> {
			self.0.set_bitrate(bitrate)
		}
	}
}
