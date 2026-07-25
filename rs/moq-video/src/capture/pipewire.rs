//! Screen capture via xdg-desktop-portal + PipeWire (Linux, Wayland and X11).
//!
//! The ScreenCast portal owns source selection: [`open`] pops the compositor's
//! picker dialog, the user chooses a monitor, and the portal hands us a PipeWire
//! fd + node id. A dedicated thread then runs the PipeWire main loop, converting
//! each RGB frame to CPU [`I420`] and pushing it into the shared [`FrameChannel`]
//! (callback-driven like the macOS delegate, not a pull-style pump).
//!
//! Two quirks worth knowing:
//! - `publish_capture` releases the capture while unwatched and reopens it on
//!   demand. A fresh portal session would re-prompt the picker every time, so the
//!   portal's restore token is kept in a process-wide slot and replayed on the
//!   next [`open`], which restores the same grant without a dialog (on
//!   compositors that support persistence). The token is forgotten when the
//!   compositor ends the stream (the user hit "stop sharing"), so a revoked
//!   grant is asked for again rather than silently resumed.
//! - Compositors only deliver frames on damage, so a static screen would starve
//!   the encoder. A loop timer re-emits the last frame whenever a frame interval
//!   passes without a fresh one, mirroring the Windows Desktop Duplication pacing.

use std::cell::RefCell;
use std::os::fd::OwnedFd;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use ashpd::desktop::PersistMode;
use ashpd::desktop::screencast::{CursorMode, Screencast, SelectSourcesOptions, SourceType};
use pipewire as pw;
use pw::spa;
use spa::param::video::{VideoFormat, VideoInfoRaw};

use super::channel::FrameChannel;
use super::pump::Geometry;
use super::{Config, FrameStream};
use crate::Error;
use crate::frame::{I420, Surface};

const DEFAULT_FRAMERATE: u32 = 30;
/// The compositor sends the negotiated format right after the stream connects;
/// if nothing arrives the session is broken (or the grant was revoked mid-setup).
const FORMAT_TIMEOUT: Duration = Duration::from_secs(10);
/// ScreenCast compositors deliver the current content as a first frame right
/// after negotiation; none arriving means the session is broken, so fail `open`
/// rather than hand the encoder a stream that will never produce (same
/// first-frame wait as the macOS ScreenCaptureKit backend).
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(5);

/// The portal restore token from the last grant, replayed on the next [`open`]
/// so a demand-driven reopen skips the picker dialog. Process-wide because the
/// capture session (and its `FrameStream`) is torn down between opens.
static RESTORE_TOKEN: Mutex<Option<String>> = Mutex::new(None);

fn err(ctx: &str, e: impl std::fmt::Display) -> Error {
	Error::Codec(anyhow::anyhow!("{ctx}: {e}"))
}

/// Open a portal screen capture and stream its frames from a PipeWire loop thread.
pub(super) async fn open(config: &Config, device: Option<&str>) -> Result<FrameStream, Error> {
	if let Some(device) = device {
		tracing::debug!(%device, "portal screen capture ignores the device selector; the picker owns selection");
	}

	let (node_id, fd, session) = portal_negotiate(config.cursor).await?;

	let chan = FrameChannel::new();
	let framerate = config.framerate.unwrap_or(DEFAULT_FRAMERATE).max(1);
	let (geo_tx, geo_rx) = tokio::sync::oneshot::channel();
	let (quit_tx, quit_rx) = pw::channel::channel::<()>();

	let handle = std::thread::spawn({
		let chan = chan.clone();
		move || {
			let state = Rc::new(RefCell::new(State {
				format: VideoInfoRaw::default(),
				geometry: None,
				geo_tx: Some(geo_tx),
				last: None,
				fresh: false,
			}));
			if let Err(e) = run_loop(fd, node_id, framerate, chan.clone(), state.clone(), quit_rx) {
				// Surface a setup failure through the awaiting `open`; a mid-stream
				// failure just ends the stream (the encode loop reopens on demand).
				match state.borrow_mut().geo_tx.take() {
					Some(tx) => drop(tx.send(Err(e))),
					None => tracing::warn!(error = %e, "screen capture stream failed"),
				}
			}
			chan.close();
		}
	});

	// Own the thread from here on, so cancelling this `await` still stops and
	// joins it instead of detaching a loop that holds the portal session open.
	// `Drop` quits and joins the loop first, then its `session` field closes the
	// portal session, so the compositor's sharing indicator turns off and the
	// token-clearing `Unconnected` path can't fire on our own teardown.
	let guard = LoopGuard {
		quit: quit_tx,
		handle: Some(handle),
		_session: session,
	};

	let geo = match tokio::time::timeout(FORMAT_TIMEOUT, geo_rx).await {
		Ok(Ok(result)) => result?,
		Ok(Err(_)) => {
			return Err(Error::Codec(anyhow::anyhow!(
				"screen capture thread exited before negotiating a format"
			)));
		}
		Err(_) => {
			return Err(Error::Codec(anyhow::anyhow!(
				"no video format from the compositor within {FORMAT_TIMEOUT:?}"
			)));
		}
	};

	let first = match tokio::time::timeout(FIRST_FRAME_TIMEOUT, chan.recv()).await {
		Ok(Some(frame)) => frame,
		Ok(None) | Err(_) => {
			return Err(Error::Codec(anyhow::anyhow!(
				"no frames from the compositor within {FIRST_FRAME_TIMEOUT:?}"
			)));
		}
	};

	tracing::info!(
		node = node_id,
		width = geo.width,
		height = geo.height,
		"opened screen capture (PipeWire)"
	);

	Ok(FrameStream::new(
		chan,
		geo.width,
		geo.height,
		geo.framerate,
		geo.device,
		Some(first),
		Box::new(guard),
	))
}

/// Ask the ScreenCast portal for a monitor: create a session, (re)select the
/// source, and start it, returning the PipeWire node to stream, the fd of the
/// portal's PipeWire remote, and a guard that closes the session on drop.
async fn portal_negotiate(cursor: bool) -> Result<(u32, OwnedFd, SessionGuard), Error> {
	let proxy = Screencast::new().await.map_err(|e| err("screencast portal", e))?;
	let session = proxy
		.create_session(Default::default())
		.await
		.map_err(|e| err("portal session", e))?;

	let restore = RESTORE_TOKEN.lock().unwrap().clone();
	proxy
		.select_sources(
			&session,
			SelectSourcesOptions::default()
				.set_cursor_mode(if cursor {
					CursorMode::Embedded
				} else {
					CursorMode::Hidden
				})
				.set_sources(ashpd::enumflags2::BitFlags::from(SourceType::Monitor))
				.set_multiple(false)
				.set_persist_mode(PersistMode::Application)
				.set_restore_token(restore.as_deref()),
		)
		.await
		.map_err(|e| err("portal select sources", e))?;

	// This is where the compositor's picker dialog appears (unless the restore
	// token silently re-grants), so it blocks on the user.
	let response = proxy
		.start(&session, None, Default::default())
		.await
		.map_err(|e| err("portal start", e))?
		.response()
		.map_err(|e| err("screen capture request denied", e))?;
	*RESTORE_TOKEN.lock().unwrap() = response.restore_token().map(str::to_string);

	let stream = response
		.streams()
		.first()
		.ok_or_else(|| Error::Codec(anyhow::anyhow!("portal granted no streams")))?;
	let node_id = stream.pipe_wire_node_id();

	let fd = proxy
		.open_pipe_wire_remote(&session, Default::default())
		.await
		.map_err(|e| err("portal pipewire remote", e))?;
	Ok((node_id, fd, SessionGuard::new(session)))
}

/// Closes the portal session when dropped, so the compositor's "screen is being
/// shared" indicator turns off and sessions don't pile up across demand-driven
/// reopens. The close call is async and `Drop` is not, so a task spawned here
/// waits for the guard to drop. Closing does not invalidate the restore token.
struct SessionGuard {
	_close: tokio::sync::oneshot::Sender<()>,
}

impl SessionGuard {
	fn new(session: ashpd::desktop::Session<Screencast>) -> Self {
		let (tx, rx) = tokio::sync::oneshot::channel::<()>();
		tokio::spawn(async move {
			// Resolves with `Err` once the guard (the sender) drops.
			let _ = rx.await;
			if let Err(e) = session.close().await {
				tracing::debug!(error = %e, "failed to close portal session");
			}
		});
		Self { _close: tx }
	}
}

/// Stops the PipeWire loop and joins its thread on drop, then (via the
/// `session` field, which drops after `drop` runs) closes the portal session
/// and thereby the frame channel's upstream.
struct LoopGuard {
	quit: pw::channel::Sender<()>,
	handle: Option<JoinHandle<()>>,
	/// Held so the portal session outlives the loop; dropping it closes the
	/// session only after the loop thread has been joined above.
	_session: SessionGuard,
}

impl Drop for LoopGuard {
	fn drop(&mut self) {
		let _ = self.quit.send(());
		if let Some(handle) = self.handle.take() {
			let _ = handle.join();
		}
	}
}

/// Shared by the stream callbacks and the pacing timer, all on the loop thread.
struct State {
	format: VideoInfoRaw,
	/// The even-clamped size sent to `open`; a later renegotiation to a different
	/// size quits the loop so the encode loop reopens at the new geometry.
	geometry: Option<(u32, u32)>,
	geo_tx: Option<tokio::sync::oneshot::Sender<Result<Geometry, Error>>>,
	/// Most recent converted frame, re-emitted while the screen is static.
	last: Option<I420>,
	/// Whether a fresh frame arrived since the last pacing tick.
	fresh: bool,
}

/// Connect to the portal's PipeWire node and run the main loop until the stream
/// ends, `quit_rx` fires (the `FrameStream` dropped), or the format changes.
fn run_loop(
	fd: OwnedFd,
	node_id: u32,
	framerate: u32,
	chan: Arc<FrameChannel>,
	state: Rc<RefCell<State>>,
	quit_rx: pw::channel::Receiver<()>,
) -> Result<(), Error> {
	pw::init();

	let mainloop = pw::main_loop::MainLoopRc::new(None).map_err(|e| err("pipewire main loop", e))?;
	let context = pw::context::ContextRc::new(&mainloop, None).map_err(|e| err("pipewire context", e))?;
	let core = context
		.connect_fd_rc(fd, None)
		.map_err(|e| err("pipewire connect", e))?;

	let stream = pw::stream::StreamRc::new(
		core,
		"moq-screen",
		pw::properties::properties! {
			*pw::keys::MEDIA_TYPE => "Video",
			*pw::keys::MEDIA_CATEGORY => "Capture",
			*pw::keys::MEDIA_ROLE => "Screen",
		},
	)
	.map_err(|e| err("pipewire stream", e))?;

	let _listener = stream
		.add_local_listener::<()>()
		.state_changed({
			let mainloop = mainloop.downgrade();
			move |_, _, _, new| {
				// Error is fatal; Unconnected after setup means the user stopped
				// sharing from the compositor. Either way the stream is over.
				let done = matches!(
					new,
					pw::stream::StreamState::Error(_) | pw::stream::StreamState::Unconnected
				);
				if done {
					// The compositor ended the stream while our loop was live (the
					// user hit "stop sharing", or the output went away). Forget the
					// restore token so the demand-driven reopen asks again instead
					// of silently resuming a grant the user just revoked. Our own
					// teardown quits the loop before anything disconnects, so it
					// never reaches this path.
					*RESTORE_TOKEN.lock().unwrap() = None;
					tracing::debug!(state = ?new, "screen capture stream ended");
					if let Some(mainloop) = mainloop.upgrade() {
						mainloop.quit();
					}
				}
			}
		})
		.param_changed({
			let state = state.clone();
			let mainloop = mainloop.downgrade();
			move |_, _, id, param| {
				let Some(param) = param else { return };
				if id != spa::param::ParamType::Format.as_raw() {
					return;
				}
				let Ok((media_type, media_subtype)) = spa::param::format_utils::parse_format(param) else {
					return;
				};
				if media_type != spa::param::format::MediaType::Video
					|| media_subtype != spa::param::format::MediaSubtype::Raw
				{
					return;
				}

				let mut state = state.borrow_mut();
				if let Err(e) = state.format.parse(param) {
					tracing::warn!(error = %e, "failed to parse pipewire video format");
					return;
				}

				let size = state.format.size();
				// I420 chroma is 2x2 subsampled; clamp down (drop the last odd
				// row/column, the stride still covers the full source row).
				let (width, height) = (size.width & !1, size.height & !1);
				if width == 0 || height == 0 {
					tracing::warn!(width = size.width, height = size.height, "unusable capture size");
					return;
				}

				if let Some(tx) = state.geo_tx.take() {
					state.geometry = Some((width, height));
					// The compositor reports 0/1 for a variable rate; only a real
					// rate is worth forwarding to the encoder.
					let fr = state.format.framerate();
					let framerate = (fr.num > 0 && fr.denom > 0).then(|| (fr.num / fr.denom).max(1));
					let _ = tx.send(Ok(Geometry {
						width,
						height,
						framerate,
						device: format!("pipewire:{node_id}"),
					}));
				} else if state.geometry != Some((width, height)) {
					// Renegotiated to a new size (e.g. the monitor changed mode).
					// End the stream; the encode loop reopens at the new geometry
					// and the restore token skips the picker.
					tracing::info!(width, height, "capture size changed; restarting the stream");
					if let Some(mainloop) = mainloop.upgrade() {
						mainloop.quit();
					}
				}
			}
		})
		.process({
			let state = state.clone();
			let chan = chan.clone();
			let mainloop = mainloop.downgrade();
			move |stream, _| {
				let mut state = state.borrow_mut();
				let Some((width, height)) = state.geometry else { return };
				let Some(mut buffer) = stream.dequeue_buffer() else {
					return;
				};
				let datas = buffer.datas_mut();
				let Some(data) = datas.first_mut() else { return };

				let offset = data.chunk().offset() as usize;
				let size = data.chunk().size() as usize;
				let stride = data.chunk().stride();
				// Compositors mark skipped frames as empty or corrupted; drop those
				// rather than treating them as a fatal conversion failure below.
				if size == 0 || data.chunk().flags().contains(spa::buffer::ChunkFlags::CORRUPTED) {
					return;
				}
				// Without dmabuf modifiers in our format offer the compositor uses
				// shared memory, which MAP_BUFFERS mmaps for us; `None` here means
				// it forced something we can't read, so give up cleanly.
				let Some(bytes) = data.data() else {
					tracing::warn!("pipewire buffer is not CPU-mapped; stopping capture");
					if let Some(mainloop) = mainloop.upgrade() {
						mainloop.quit();
					}
					return;
				};
				let Some(bytes) = bytes.get(offset..offset + size) else {
					return;
				};
				// Fall back to the unclamped source width: for an odd-width source
				// the real row is one pixel wider than the clamped `width`.
				let stride = if stride > 0 {
					stride as u32
				} else {
					state.format.size().width * 4
				};

				match convert(state.format.format(), bytes, stride, width, height) {
					Ok(i420) => {
						chan.push(Surface::I420(i420.clone()));
						state.last = Some(i420);
						state.fresh = true;
					}
					Err(e) => {
						// Persistent (bad format), not per-frame; stop rather than spam.
						tracing::warn!(error = %e, "screen frame conversion failed; stopping capture");
						if let Some(mainloop) = mainloop.upgrade() {
							mainloop.quit();
						}
					}
				}
			}
		})
		.register()
		.map_err(|e| err("pipewire listener", e))?;

	// Offer the CPU-convertible RGB layouts; the compositor picks one and
	// replies through `param_changed`. No dmabuf modifiers, so buffers stay in
	// shared memory (the CPU path, like the other non-macOS backends).
	let pod = format_offer(framerate);
	let mut params = [spa::pod::Pod::from_bytes(&pod)
		.ok_or_else(|| Error::Codec(anyhow::anyhow!("failed to build pipewire format offer")))?];
	stream
		.connect(
			spa::utils::Direction::Input,
			Some(node_id),
			pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
			&mut params,
		)
		.map_err(|e| err("pipewire stream connect", e))?;

	// Re-emit the last frame when an interval passes without a fresh one, so a
	// static screen still produces a steady stream for the encoder.
	let timer = mainloop.loop_().add_timer({
		let state = state.clone();
		let chan = chan.clone();
		move |_| {
			let mut state = state.borrow_mut();
			if std::mem::take(&mut state.fresh) {
				return;
			}
			if let Some(last) = &state.last {
				chan.push(Surface::I420(last.clone()));
			}
		}
	});
	let interval = Duration::from_micros(1_000_000 / framerate as u64);
	timer
		.update_timer(Some(interval), Some(interval))
		.into_result()
		.map_err(|e| err("pipewire timer", e))?;

	// Quit when the FrameStream drops.
	let _quit = quit_rx.attach(mainloop.loop_(), {
		let mainloop = mainloop.downgrade();
		move |_| {
			if let Some(mainloop) = mainloop.upgrade() {
				mainloop.quit();
			}
		}
	});

	mainloop.run();
	Ok(())
}

/// Convert one strided RGB screen frame to tightly-packed I420.
fn convert(format: VideoFormat, bytes: &[u8], stride: u32, width: u32, height: u32) -> Result<I420, Error> {
	match format {
		VideoFormat::BGRx | VideoFormat::BGRA => I420::from_bgra(bytes, stride, width, height),
		VideoFormat::RGBx | VideoFormat::RGBA => I420::from_rgba(bytes, stride, width, height),
		other => Err(Error::Codec(anyhow::anyhow!(
			"pipewire negotiated an unsupported video format {other:?}"
		))),
	}
}

/// Serialize the `EnumFormat` pod offering the RGB layouts we can convert,
/// any size, and a framerate range preferring `framerate`.
fn format_offer(framerate: u32) -> Vec<u8> {
	let obj = spa::pod::object!(
		spa::utils::SpaTypes::ObjectParamFormat,
		spa::param::ParamType::EnumFormat,
		spa::pod::property!(
			spa::param::format::FormatProperties::MediaType,
			Id,
			spa::param::format::MediaType::Video
		),
		spa::pod::property!(
			spa::param::format::FormatProperties::MediaSubtype,
			Id,
			spa::param::format::MediaSubtype::Raw
		),
		spa::pod::property!(
			spa::param::format::FormatProperties::VideoFormat,
			Choice,
			Enum,
			Id,
			VideoFormat::BGRx,
			VideoFormat::BGRx,
			VideoFormat::BGRA,
			VideoFormat::RGBx,
			VideoFormat::RGBA,
		),
		spa::pod::property!(
			spa::param::format::FormatProperties::VideoSize,
			Choice,
			Range,
			Rectangle,
			spa::utils::Rectangle {
				width: 1920,
				height: 1080
			},
			spa::utils::Rectangle { width: 1, height: 1 },
			spa::utils::Rectangle {
				width: 8192,
				height: 8192
			}
		),
		spa::pod::property!(
			spa::param::format::FormatProperties::VideoFramerate,
			Choice,
			Range,
			Fraction,
			spa::utils::Fraction {
				num: framerate,
				denom: 1
			},
			spa::utils::Fraction { num: 0, denom: 1 },
			spa::utils::Fraction { num: 1000, denom: 1 }
		),
	);
	spa::pod::serialize::PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &spa::pod::Value::Object(obj))
		.expect("serializing a static format pod cannot fail")
		.0
		.into_inner()
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::capture::Config;

	/// The serialized format offer must parse back as a valid pod.
	#[test]
	fn format_offer_is_valid_pod() {
		let bytes = format_offer(30);
		assert!(spa::pod::Pod::from_bytes(&bytes).is_some(), "offer did not round-trip");
	}

	/// Open the portal, grab a few frames, and check geometry. Ignored because it
	/// needs a desktop session, PipeWire, and a human clicking the picker dialog:
	/// `cargo test -p moq-video --features pipewire portal_capture -- --ignored`.
	#[tokio::test]
	#[ignore]
	async fn portal_captures_frames() {
		let mut stream = match open(&Config::default(), None).await {
			Ok(stream) => stream,
			Err(e) => {
				eprintln!("skipping: no portal screen capture available: {e}");
				return;
			}
		};

		assert!(stream.width() >= 2 && stream.width().is_multiple_of(2), "bad width");
		assert!(stream.height() >= 2 && stream.height().is_multiple_of(2), "bad height");

		for i in 0..5 {
			let frame = stream.read().await.unwrap_or_else(|| panic!("no frame {i}"));
			assert_eq!(frame.width(), stream.width());
			assert_eq!(frame.height(), stream.height());
		}
		eprintln!("captured 5 frames at {}x{}", stream.width(), stream.height());
	}
}
