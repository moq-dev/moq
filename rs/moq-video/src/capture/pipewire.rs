//! Screen capture via xdg-desktop-portal + PipeWire (Linux, Wayland and X11).
//!
//! The ScreenCast portal owns source selection: [`open`] pops the compositor's
//! picker dialog, the user chooses a monitor, and the portal hands us a PipeWire
//! fd + node id. A dedicated thread then runs the PipeWire main loop, converting
//! each packed RGB or NV12 frame to CPU [`I420`] and pushing it into the shared
//! [`FrameChannel`] (callback-driven like the macOS delegate, not a pull-style
//! pump).
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

use std::borrow::Cow;
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
use crate::frame::{I420, Surface};
use crate::{Color, Error, Size};

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
				color: None,
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
	/// The even-clamped size sent to `open`.
	geometry: Option<(u32, u32)>,
	/// The NV12 color space used to configure the encoder.
	color: Option<Color>,
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
				if let Err(e) = replace_video_format(&mut state.format, |format| format.parse(param)) {
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
				let color = match pipewire_color(state.format, width, height) {
					Ok(color) => color,
					Err(e) => {
						match state.geo_tx.take() {
							Some(tx) => drop(tx.send(Err(e))),
							None => {
								tracing::warn!(error = %e, "unsupported pipewire video color space");
								if let Some(mainloop) = mainloop.upgrade() {
									mainloop.quit();
								}
							}
						}
						return;
					}
				};

				if let Some(tx) = state.geo_tx.take() {
					state.geometry = Some((width, height));
					state.color = color;
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
				} else if format_requires_restart(state.geometry, state.color, width, height, color) {
					// The encoder's geometry and VUI are fixed when it opens. End the
					// stream so the encode loop reopens it with the new format.
					tracing::info!(width, height, ?color, "capture format changed; restarting the stream");
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
				let source_height = state.format.size().height;
				let color = state.color;
				let Some(mut buffer) = stream.dequeue_buffer() else {
					return;
				};
				let datas = buffer.datas_mut();
				let Some(data) = datas.first_mut() else { return };

				let maxsize = data.as_raw().maxsize;
				let size = clamp_chunk_size(data.chunk().size(), maxsize);
				match chunk_kind(size, data.chunk().flags()) {
					ChunkKind::Data => {}
					ChunkKind::Empty => {
						let Ok(frame) = neutral_frame(width, height, color) else {
							return;
						};
						chan.push(Surface::I420(frame.clone()));
						state.last = Some(frame);
						state.fresh = true;
						return;
					}
					ChunkKind::Invalid => return,
				}
				let Some(offset) = normalize_chunk_offset(data.chunk().offset(), maxsize) else {
					tracing::warn!("pipewire buffer has zero maximum size");
					return;
				};
				// Fall back to the unclamped source width: for an odd-width source
				// the real row is one pixel wider than the clamped `width`. The
				// packing differs per format, so NV12 cannot assume 4 bytes/pixel.
				let stride = match u32::try_from(data.chunk().stride()) {
					Ok(stride) if stride > 0 => stride,
					_ => match state.format.format() {
						VideoFormat::NV12 => state.format.size().width,
						_ => state.format.size().width.saturating_mul(4),
					},
				};
				let layout = FrameLayout {
					stride,
					width,
					height,
					source_height,
				};

				// The chunk only says how many bytes the producer wrote. Check it
				// actually spans every row `convert` will sample, so a short or
				// mislabeled buffer is dropped here rather than read past.
				let Some(required) = frame_data_size(state.format.format(), layout) else {
					tracing::warn!("pipewire frame layout overflows its buffer");
					return;
				};
				if required > size || required > maxsize as usize {
					tracing::warn!(
						required,
						available = size,
						"pipewire chunk does not contain a complete frame"
					);
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
				let Some(allocation) = bytes.get(..maxsize as usize) else {
					return;
				};
				let Some(bytes) = chunk_bytes(allocation, offset, required) else {
					return;
				};

				match convert(state.format.format(), bytes.as_ref(), layout, color) {
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

/// The geometry `convert` needs: the producer's row stride and source height,
/// plus the even-clamped output size.
#[derive(Clone, Copy)]
struct FrameLayout {
	stride: u32,
	width: u32,
	height: u32,
	/// Unclamped source height. NV12's chroma plane starts after this many luma
	/// rows, which is one more than `height` for an odd-height source.
	source_height: u32,
}

const CHUNK_FLAG_EMPTY: i32 = 1 << 1;

#[derive(Debug, PartialEq, Eq)]
enum ChunkKind {
	Data,
	Empty,
	Invalid,
}

fn chunk_kind(size: usize, flags: spa::buffer::ChunkFlags) -> ChunkKind {
	if flags.contains(spa::buffer::ChunkFlags::CORRUPTED) {
		ChunkKind::Invalid
	} else if flags.bits() & CHUNK_FLAG_EMPTY != 0 {
		ChunkKind::Empty
	} else if size == 0 {
		ChunkKind::Invalid
	} else {
		ChunkKind::Data
	}
}

fn neutral_frame(width: u32, height: u32, color: Option<Color>) -> Result<I420, Error> {
	let color = color.unwrap_or_else(|| Color::infer(Size::new(width, height)));
	let luma = width as usize * height as usize;
	let mut data = vec![128; I420::len(width, height)];
	data[..luma].fill(if color.limited() { 16 } else { 0 });
	Ok(I420::new(width, height, data)?.with_color(color))
}

/// Parse into a zeroed value before replacing the current format. libspa leaves
/// omitted optional properties untouched, so parsing into the reused value
/// would retain stale color metadata across renegotiation.
fn replace_video_format<T, E>(
	current: &mut VideoInfoRaw,
	parse: impl FnOnce(&mut VideoInfoRaw) -> Result<T, E>,
) -> Result<T, E> {
	let mut next = VideoInfoRaw::default();
	let result = parse(&mut next)?;
	*current = next;
	Ok(result)
}

/// Preserve the color description that names NV12 samples. Unknown fields use
/// the same size-based, limited-range fallback as the encoder. Reject matrices
/// the crate cannot represent rather than writing a false 601/709 VUI.
fn pipewire_color(format: VideoInfoRaw, width: u32, height: u32) -> Result<Option<Color>, Error> {
	if format.format() != VideoFormat::NV12 {
		return Ok(None);
	}
	let size = Size::new(width, height);
	let color = color_from_pipewire(format.color_range(), format.color_matrix(), size)?;
	validate_pipewire_description(
		color.unwrap_or_else(|| Color::infer(size)),
		format.color_primaries(),
		format.transfer_function(),
	)?;
	Ok(color)
}

fn color_from_pipewire(range: u32, matrix: u32, size: Size) -> Result<Option<Color>, Error> {
	if range == spa::sys::SPA_VIDEO_COLOR_RANGE_UNKNOWN && matrix == spa::sys::SPA_VIDEO_COLOR_MATRIX_UNKNOWN {
		return Ok(None);
	}

	let limited = match range {
		spa::sys::SPA_VIDEO_COLOR_RANGE_UNKNOWN | spa::sys::SPA_VIDEO_COLOR_RANGE_16_235 => true,
		spa::sys::SPA_VIDEO_COLOR_RANGE_0_255 => false,
		_ => {
			return Err(Error::Codec(anyhow::anyhow!(
				"unsupported PipeWire NV12 color range {range}"
			)));
		}
	};
	let bt709 = match matrix {
		spa::sys::SPA_VIDEO_COLOR_MATRIX_UNKNOWN => {
			matches!(Color::infer(size), Color::Bt709Limited | Color::Bt709Full)
		}
		spa::sys::SPA_VIDEO_COLOR_MATRIX_BT709 => true,
		spa::sys::SPA_VIDEO_COLOR_MATRIX_BT601 => false,
		_ => {
			return Err(Error::Codec(anyhow::anyhow!(
				"unsupported PipeWire NV12 color matrix {matrix}"
			)));
		}
	};

	Ok(Some(match (bt709, limited) {
		(false, true) => Color::Bt601Limited,
		(false, false) => Color::Bt601Full,
		(true, true) => Color::Bt709Limited,
		(true, false) => Color::Bt709Full,
	}))
}

fn validate_pipewire_description(color: Color, primaries: u32, transfer: u32) -> Result<(), Error> {
	let expected_primaries = match color {
		Color::Bt601Limited | Color::Bt601Full => spa::sys::SPA_VIDEO_COLOR_PRIMARIES_SMPTE170M,
		Color::Bt709Limited | Color::Bt709Full => spa::sys::SPA_VIDEO_COLOR_PRIMARIES_BT709,
	};
	if primaries != spa::sys::SPA_VIDEO_COLOR_PRIMARIES_UNKNOWN && primaries != expected_primaries {
		return Err(Error::Codec(anyhow::anyhow!(
			"PipeWire NV12 primaries {primaries} do not match the negotiated matrix"
		)));
	}
	if !matches!(
		transfer,
		spa::sys::SPA_VIDEO_TRANSFER_UNKNOWN
			| spa::sys::SPA_VIDEO_TRANSFER_BT709
			| spa::sys::SPA_VIDEO_TRANSFER_BT601
			| spa::sys::SPA_VIDEO_TRANSFER_BT2020_10
	) {
		return Err(Error::Codec(anyhow::anyhow!(
			"unsupported PipeWire NV12 transfer function {transfer}"
		)));
	}
	Ok(())
}

fn format_requires_restart(
	geometry: Option<(u32, u32)>,
	color: Option<Color>,
	width: u32,
	height: u32,
	next_color: Option<Color>,
) -> bool {
	geometry.is_some_and(|geometry| geometry != (width, height) || color != next_color)
}

/// A chunk offset is a ring position within the allocation, so wrap it rather
/// than trusting it to be in range. `None` means the allocation is unusable.
fn normalize_chunk_offset(offset: u32, maxsize: u32) -> Option<usize> {
	(maxsize != 0).then(|| (offset % maxsize) as usize)
}

fn clamp_chunk_size(size: u32, maxsize: u32) -> usize {
	size.min(maxsize) as usize
}

fn chunk_bytes(data: &[u8], offset: usize, size: usize) -> Option<Cow<'_, [u8]>> {
	if offset >= data.len() || size > data.len() {
		return None;
	}
	let end = offset.checked_add(size)?;
	if end <= data.len() {
		return Some(Cow::Borrowed(&data[offset..end]));
	}

	let mut wrapped = Vec::with_capacity(size);
	wrapped.extend_from_slice(&data[offset..]);
	let head = size - wrapped.len();
	wrapped.extend_from_slice(&data[..head]);
	Some(Cow::Owned(wrapped))
}

/// Bytes from the chunk start through the span required by `convert`. NV12 stops
/// at the visible width of its final row; packed RGB requires every full stride.
fn frame_data_size(format: VideoFormat, layout: FrameLayout) -> Option<usize> {
	let stride = layout.stride as usize;
	let width = layout.width as usize;
	let height = layout.height as usize;
	let row_size = match format {
		VideoFormat::NV12 => width,
		VideoFormat::BGRx | VideoFormat::BGRA | VideoFormat::RGBx | VideoFormat::RGBA => width.checked_mul(4)?,
		_ => return None,
	};
	if stride < row_size {
		return None;
	}

	match format {
		// Chroma follows every source luma row, then runs at half height.
		VideoFormat::NV12 => (layout.source_height as usize)
			.checked_add(height / 2)?
			.checked_sub(1)?
			.checked_mul(stride)?
			.checked_add(row_size),
		_ => stride.checked_mul(height),
	}
}

/// Deinterleave strided NV12 into the crate's tightly packed I420 layout.
fn nv12_to_i420(data: &[u8], layout: FrameLayout) -> Result<I420, Error> {
	let (stride, width, height, source_height) = (
		layout.stride as usize,
		layout.width as usize,
		layout.height as usize,
		layout.source_height as usize,
	);
	if source_height < height {
		return Err(Error::Codec(anyhow::anyhow!(
			"NV12 source is shorter than the cropped output"
		)));
	}
	let y_len = stride
		.checked_mul(source_height)
		.ok_or_else(|| Error::Codec(anyhow::anyhow!("NV12 luma size overflow")))?;
	let uv_rows = height / 2;
	let uv_len = uv_rows
		.checked_sub(1)
		.and_then(|rows| rows.checked_mul(stride))
		.and_then(|offset| offset.checked_add(width))
		.ok_or_else(|| Error::Codec(anyhow::anyhow!("NV12 chroma size overflow")))?;
	let frame_len = y_len
		.checked_add(uv_len)
		.ok_or_else(|| Error::Codec(anyhow::anyhow!("NV12 frame size overflow")))?;
	if stride < width || data.len() < frame_len {
		return Err(Error::Codec(anyhow::anyhow!(
			"NV12 frame is shorter than its declared rows"
		)));
	}

	let mut packed = vec![0; I420::len(width as u32, height as u32)];
	for row in 0..height {
		packed[row * width..(row + 1) * width].copy_from_slice(&data[row * stride..row * stride + width]);
	}
	let uv = &data[y_len..frame_len];
	let packed_uv = width * height;
	for row in 0..uv_rows {
		packed[packed_uv + row * width..packed_uv + (row + 1) * width]
			.copy_from_slice(&uv[row * stride..row * stride + width]);
	}
	I420::from_nv12(&packed, width as u32, height as u32)
}

/// Convert one strided screen frame to tightly-packed I420.
fn convert(format: VideoFormat, bytes: &[u8], layout: FrameLayout, color: Option<Color>) -> Result<I420, Error> {
	match format {
		VideoFormat::NV12 => {
			let frame = nv12_to_i420(bytes, layout)?;
			Ok(match color {
				Some(color) => frame.with_color(color),
				None => frame,
			})
		}
		VideoFormat::BGRx | VideoFormat::BGRA => I420::from_bgra(bytes, layout.stride, layout.width, layout.height),
		VideoFormat::RGBx | VideoFormat::RGBA => I420::from_rgba(bytes, layout.stride, layout.width, layout.height),
		other => Err(Error::Codec(anyhow::anyhow!(
			"pipewire negotiated an unsupported video format {other:?}"
		))),
	}
}

/// Serialize the `EnumFormat` pod offering the layouts we can convert (packed
/// RGB first, then NV12), any size, and a framerate range preferring `framerate`.
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
			VideoFormat::NV12,
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

	/// The serialized format offer must parse back as a valid pod, carrying every
	/// layout `convert` knows how to handle.
	#[test]
	fn format_offer_is_valid_pod() {
		let bytes = format_offer(30);
		let (remaining, value) = spa::pod::deserialize::PodDeserializer::deserialize_any_from(&bytes)
			.expect("format offer did not round-trip");
		assert!(remaining.is_empty());
		let spa::pod::Value::Object(object) = value else {
			panic!("format offer is not an object");
		};
		let property = object
			.properties
			.iter()
			.find(|property| property.key == spa::param::format::FormatProperties::VideoFormat.as_raw())
			.expect("missing video format property");
		assert_eq!(
			property.value,
			spa::pod::Value::Choice(spa::pod::ChoiceValue::Id(spa::utils::Choice(
				spa::utils::ChoiceFlags::empty(),
				spa::utils::ChoiceEnum::Enum {
					default: spa::utils::Id(VideoFormat::BGRx.as_raw()),
					alternatives: vec![
						spa::utils::Id(VideoFormat::BGRx.as_raw()),
						spa::utils::Id(VideoFormat::BGRA.as_raw()),
						spa::utils::Id(VideoFormat::RGBx.as_raw()),
						spa::utils::Id(VideoFormat::RGBA.as_raw()),
						spa::utils::Id(VideoFormat::NV12.as_raw()),
					],
				},
			)))
		);
	}

	#[test]
	fn chunk_offset_wraps_to_the_allocation() {
		assert_eq!(normalize_chunk_offset(18, 16), Some(2));
		assert_eq!(normalize_chunk_offset(0, 0), None);
	}

	#[test]
	fn chunk_size_is_clamped_to_the_allocation() {
		assert_eq!(clamp_chunk_size(18, 16), 16);
		assert_eq!(clamp_chunk_size(8, 16), 8);
	}

	#[test]
	fn chunk_flags_distinguish_neutral_and_invalid_frames() {
		let empty = spa::buffer::ChunkFlags::from_bits_retain(CHUNK_FLAG_EMPTY);
		assert_eq!(chunk_kind(0, spa::buffer::ChunkFlags::empty()), ChunkKind::Invalid);
		assert_eq!(chunk_kind(1, spa::buffer::ChunkFlags::CORRUPTED), ChunkKind::Invalid);
		assert_eq!(chunk_kind(1, empty), ChunkKind::Empty);
		assert_eq!(chunk_kind(0, empty), ChunkKind::Empty);
		assert_eq!(chunk_kind(1, spa::buffer::ChunkFlags::empty()), ChunkKind::Data);
	}

	#[test]
	fn empty_chunk_is_limited_range_black() {
		let frame = neutral_frame(4, 2, None).unwrap();
		assert_eq!(frame.y(), &[16; 8]);
		assert_eq!(frame.u(), &[128; 2]);
		assert_eq!(frame.v(), &[128; 2]);
	}

	#[test]
	fn negotiated_nv12_color_overrides_size_inference() {
		let size = Size::new(1920, 1080);
		assert_eq!(
			color_from_pipewire(
				spa::sys::SPA_VIDEO_COLOR_RANGE_0_255,
				spa::sys::SPA_VIDEO_COLOR_MATRIX_BT601,
				size,
			)
			.unwrap(),
			Some(Color::Bt601Full)
		);
		assert_eq!(
			color_from_pipewire(
				spa::sys::SPA_VIDEO_COLOR_RANGE_UNKNOWN,
				spa::sys::SPA_VIDEO_COLOR_MATRIX_UNKNOWN,
				size,
			)
			.unwrap(),
			None
		);
		assert!(
			color_from_pipewire(
				spa::sys::SPA_VIDEO_COLOR_RANGE_16_235,
				spa::sys::SPA_VIDEO_COLOR_MATRIX_BT2020,
				size,
			)
			.is_err()
		);
		let mut format = VideoInfoRaw::default();
		format.set_format(VideoFormat::NV12);
		format.set_color_range(spa::sys::SPA_VIDEO_COLOR_RANGE_16_235);
		format.set_color_matrix(spa::sys::SPA_VIDEO_COLOR_MATRIX_BT709);
		format.set_color_primaries(spa::sys::SPA_VIDEO_COLOR_PRIMARIES_BT2020);
		format.set_transfer_function(spa::sys::SPA_VIDEO_TRANSFER_BT709);
		assert!(pipewire_color(format, 1920, 1080).is_err());
		format.set_color_range(spa::sys::SPA_VIDEO_COLOR_RANGE_UNKNOWN);
		format.set_color_matrix(spa::sys::SPA_VIDEO_COLOR_MATRIX_UNKNOWN);
		format.set_transfer_function(spa::sys::SPA_VIDEO_TRANSFER_SMPTE2084);
		assert!(pipewire_color(format, 1920, 1080).is_err());

		let layout = FrameLayout {
			stride: 4,
			width: 4,
			height: 2,
			source_height: 2,
		};
		let frame = convert(
			VideoFormat::NV12,
			&[16, 16, 16, 16, 16, 16, 16, 16, 128, 128, 128, 128],
			layout,
			Some(Color::Bt601Full),
		)
		.unwrap();
		assert_eq!(frame.color(), Some(Color::Bt601Full));
	}

	#[test]
	fn color_renegotiation_restarts_the_capture() {
		let geometry = Some((1920, 1080));
		assert!(!format_requires_restart(
			geometry,
			Some(Color::Bt709Limited),
			1920,
			1080,
			Some(Color::Bt709Limited),
		));
		assert!(format_requires_restart(
			geometry,
			Some(Color::Bt709Limited),
			1920,
			1080,
			Some(Color::Bt709Full),
		));
	}

	#[test]
	fn omitted_color_fields_reset_on_renegotiation() {
		let mut format = VideoInfoRaw::default();
		format.set_color_range(spa::sys::SPA_VIDEO_COLOR_RANGE_0_255);
		format.set_color_matrix(spa::sys::SPA_VIDEO_COLOR_MATRIX_BT709);
		replace_video_format(&mut format, |next| {
			next.set_format(VideoFormat::NV12);
			Ok::<_, std::convert::Infallible>(())
		})
		.unwrap();
		assert_eq!(format.color_range(), spa::sys::SPA_VIDEO_COLOR_RANGE_UNKNOWN);
		assert_eq!(format.color_matrix(), spa::sys::SPA_VIDEO_COLOR_MATRIX_UNKNOWN);
	}

	#[test]
	fn empty_chunk_uses_full_range_black() {
		let frame = neutral_frame(4, 2, Some(Color::Bt709Full)).unwrap();
		assert_eq!(frame.y(), &[0; 8]);
		assert_eq!(frame.u(), &[128; 2]);
		assert_eq!(frame.v(), &[128; 2]);
		assert_eq!(frame.color(), Some(Color::Bt709Full));
	}

	/// The required size must reach the last sampled row, but not the padding
	/// past it, or a tightly-sized final row would be rejected.
	#[test]
	fn frame_data_size_covers_every_sampled_row() {
		let packed = FrameLayout {
			stride: 20,
			width: 4,
			height: 2,
			source_height: 2,
		};
		assert_eq!(frame_data_size(VideoFormat::BGRx, packed), Some(40));

		let nv12 = FrameLayout {
			stride: 6,
			width: 4,
			height: 2,
			source_height: 3,
		};
		assert_eq!(frame_data_size(VideoFormat::NV12, nv12), Some(22));

		// A stride narrower than one row means the layout is nonsense.
		assert_eq!(
			frame_data_size(VideoFormat::BGRx, FrameLayout { stride: 15, ..packed }),
			None
		);
	}

	#[test]
	fn wrapped_chunk_is_reassembled() {
		let data = [0, 1, 2, 3, 4, 5];
		assert_eq!(chunk_bytes(&data, 1, 3).as_deref(), Some([1, 2, 3].as_slice()));
		assert_eq!(chunk_bytes(&data, 4, 4).as_deref(), Some([4, 5, 0, 1].as_slice()));
	}

	#[test]
	fn nv12_stride_is_removed_and_chroma_is_deinterleaved() {
		let data = [
			1, 2, 3, 4, 99, 99, // Y row 0 plus padding
			5, 6, 7, 8, 99, 99, // Y row 1 plus padding
			9, 10, 11, 12, 99, 99, // UV row plus padding
		];
		let frame = nv12_to_i420(
			&data,
			FrameLayout {
				stride: 6,
				width: 4,
				height: 2,
				source_height: 2,
			},
		)
		.unwrap();
		assert_eq!(frame.y(), &[1, 2, 3, 4, 5, 6, 7, 8]);
		assert_eq!(frame.u(), &[9, 11]);
		assert_eq!(frame.v(), &[10, 12]);
	}

	#[test]
	fn nv12_accepts_a_width_precise_final_row() {
		let layout = FrameLayout {
			stride: 6,
			width: 4,
			height: 2,
			source_height: 2,
		};
		let mut data = vec![99; frame_data_size(VideoFormat::NV12, layout).unwrap()];
		data[..4].copy_from_slice(&[1, 2, 3, 4]);
		data[6..10].copy_from_slice(&[5, 6, 7, 8]);
		data[12..16].copy_from_slice(&[9, 10, 11, 12]);

		let frame = nv12_to_i420(&data, layout).unwrap();
		assert_eq!(frame.y(), &[1, 2, 3, 4, 5, 6, 7, 8]);
		assert_eq!(frame.u(), &[9, 11]);
		assert_eq!(frame.v(), &[10, 12]);
	}

	/// An odd-height source has one more luma row than the clamped output, and
	/// chroma starts after all of them.
	#[test]
	fn nv12_crop_uses_the_source_height_for_chroma() {
		let data = [
			1, 2, 3, 4, // Y row 0
			5, 6, 7, 8, // Y row 1
			99, 99, 99, 99, // cropped Y row 2
			9, 10, 11, 12, // UV row 0
		];
		let frame = nv12_to_i420(
			&data,
			FrameLayout {
				stride: 4,
				width: 4,
				height: 2,
				source_height: 3,
			},
		)
		.unwrap();
		assert_eq!(frame.y(), &[1, 2, 3, 4, 5, 6, 7, 8]);
		assert_eq!(frame.u(), &[9, 11]);
		assert_eq!(frame.v(), &[10, 12]);
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
