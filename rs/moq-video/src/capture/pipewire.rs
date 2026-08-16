//! Screen capture via xdg-desktop-portal + PipeWire (Linux, Wayland and X11).
//!
//! The ScreenCast portal owns source selection: [`open`] pops the compositor's
//! picker dialog, the user chooses a monitor, and the portal hands us a PipeWire
//! fd + node id. A dedicated thread then runs the PipeWire main loop, forwarding
//! DMA-BUF frames without copying when the compositor offers them and converting
//! shared-memory frames to CPU [`I420`] otherwise. It pushes both into the shared
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
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use ashpd::desktop::PersistMode;
use ashpd::desktop::screencast::{CursorMode, Screencast, SelectSourcesOptions, SourceType};
use pipewire as pw;
use pw::spa;
use spa::buffer::DataType;
use spa::param::video::{VideoFormat, VideoInfoRaw};

use super::channel::FrameChannel;
use super::pump::Geometry;
use super::{Config, FrameStream};
use crate::frame::{DmaBuf, DmaBufFrame, DmaBufPlane, DrmFormat, I420, Surface, wait_dma_buf_readable};
use crate::{Color, Error, Size};

const DEFAULT_FRAMERATE: u32 = 30;
// libspa 0.10 omits this flag from its safe wrapper, but exposes the raw bits.
const CHUNK_FLAG_EMPTY: i32 = 1 << 1;
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
	let (return_tx, return_rx) = pw::channel::channel::<Lease>();

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
				generation: 0,
				dmabuf_modifier: None,
			}));
			if let Err(e) = run_loop(CaptureLoop {
				fd,
				node_id,
				framerate,
				chan: chan.clone(),
				state: state.clone(),
				quit_rx,
				return_rx,
				return_tx,
			}) {
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
	last: Option<Last>,
	/// Whether a fresh frame arrived since the last pacing tick.
	fresh: bool,
	/// Buffer-pool generation. Returns from superseded pools are discarded.
	generation: u64,
	/// Explicit DRM modifier from the negotiated DMA-BUF format.
	dmabuf_modifier: Option<u64>,
}

/// A retained frame for the static-screen pacing tick.
enum Last {
	I420(I420),
	DmaBuf(DmaBuf),
}

impl Last {
	fn surface(&self) -> Surface {
		match self {
			Self::I420(frame) => Surface::I420(frame.clone()),
			Self::DmaBuf(frame) => Surface::DmaBuf(frame.clone()),
		}
	}
}

#[derive(Clone, Copy)]
struct FrameLayout {
	stride: u32,
	width: u32,
	height: u32,
	source_height: u32,
}

/// A dequeued PipeWire DMA-BUF kept out of the producer's pool until every
/// frame clone drops. Duplicating the fd alone is not enough: it preserves the
/// allocation, but the compositor may overwrite its pixels as soon as the
/// original buffer is queued again.
struct PipeWireDmaBuf {
	fd: OwnedFd,
	return_tx: pw::channel::Sender<Lease>,
	lease: Lease,
	map_offset: u32,
	allocation_size: Option<usize>,
	data_offset: usize,
	layout: FrameLayout,
	format: DrmFormat,
	modifier: u64,
	color: Option<Color>,
}

impl Drop for PipeWireDmaBuf {
	fn drop(&mut self) {
		// A failed send means the stream and its pool have already been destroyed.
		// The duplicated fd still closes normally; the stale pointer is never used.
		let _ = self.return_tx.send(self.lease);
	}
}

impl DmaBufFrame for PipeWireDmaBuf {
	fn export(&self) -> std::io::Result<OwnedFd> {
		self.fd.as_fd().try_clone_to_owned()
	}

	fn download_i420(&self) -> Result<I420, Error> {
		// DRM_FORMAT_MOD_LINEAR is zero. A tiled allocation cannot be interpreted
		// as strided rows; its fallback needs a GPU/VPP download instead.
		if self.modifier != 0 {
			return Err(Error::Codec(anyhow::anyhow!(
				"cannot download DMA-BUF modifier {:#x} as linear rows",
				self.modifier
			)));
		}

		wait_dma_buf_readable(self.fd.as_fd())
			.map_err(|e| Error::Codec(anyhow::anyhow!("waiting for DMA-BUF producer: {e}")))?;
		with_dma_buf_read(&self.fd, || {
			let allocation_size = self
				.allocation_size
				.ok_or_else(|| Error::Codec(anyhow::anyhow!("DMA-BUF descriptor does not report a mappable size")))?;
			let mapping = Mapping::new(&self.fd, self.map_offset, allocation_size)?;
			let data = mapping
				.as_slice()
				.get(self.data_offset..)
				.ok_or_else(|| Error::Codec(anyhow::anyhow!("DMA-BUF chunk starts outside its allocation")))?;

			match self.format {
				DrmFormat::NV12 => {
					let frame = nv12_to_i420(data, self.layout)?;
					Ok(match self.color {
						Some(color) => frame.with_color(color),
						None => frame,
					})
				}
				DrmFormat::XRGB8888 | DrmFormat::ARGB8888 => {
					I420::from_bgra(data, self.layout.stride, self.layout.width, self.layout.height)
				}
				DrmFormat::XBGR8888 | DrmFormat::ABGR8888 => {
					I420::from_rgba(data, self.layout.stride, self.layout.width, self.layout.height)
				}
				other => Err(Error::Codec(anyhow::anyhow!(
					"cannot download DMA-BUF format {:#x}",
					other.as_raw()
				))),
			}
		})
	}
}

const DMA_BUF_SYNC_READ: u64 = 1 << 0;
const DMA_BUF_SYNC_END: u64 = 1 << 2;

#[repr(C)]
struct DmaBufSync {
	flags: u64,
}

/// Brackets CPU reads with the cache-coherency protocol required by DMA-BUF.
fn with_dma_buf_read<T>(fd: &OwnedFd, read: impl FnOnce() -> Result<T, Error>) -> Result<T, Error> {
	let sync = DmaBufRead::new(fd)?;
	let result = read();
	let end = sync.finish();
	match (result, end) {
		(Ok(value), Ok(())) => Ok(value),
		(Err(err), _) => Err(err),
		(Ok(_), Err(err)) => Err(err),
	}
}

struct DmaBufRead<'a> {
	fd: &'a OwnedFd,
	finished: bool,
}

impl<'a> DmaBufRead<'a> {
	fn new(fd: &'a OwnedFd) -> Result<Self, Error> {
		dma_buf_sync(fd, DMA_BUF_SYNC_READ)?;
		Ok(Self { fd, finished: false })
	}

	fn finish(mut self) -> Result<(), Error> {
		self.finished = true;
		dma_buf_sync(self.fd, DMA_BUF_SYNC_READ | DMA_BUF_SYNC_END)
	}
}

impl Drop for DmaBufRead<'_> {
	fn drop(&mut self) {
		if !self.finished
			&& let Err(err) = dma_buf_sync(self.fd, DMA_BUF_SYNC_READ | DMA_BUF_SYNC_END)
		{
			tracing::warn!(%err, "ending DMA-BUF CPU access failed");
		}
	}
}

fn dma_buf_sync(fd: &OwnedFd, flags: u64) -> Result<(), Error> {
	let mut sync = DmaBufSync { flags };
	loop {
		// SAFETY: this is the DMA-BUF sync ioctl with its matching UAPI payload,
		// and `fd` stays open for the duration of the call.
		let result = unsafe {
			libc::ioctl(
				std::os::fd::AsRawFd::as_raw_fd(fd),
				linux_raw_sys::ioctl::DMA_BUF_IOCTL_SYNC as libc::Ioctl,
				&mut sync,
			)
		};
		if result == 0 {
			return Ok(());
		}
		let err = std::io::Error::last_os_error();
		if err.kind() != std::io::ErrorKind::Interrupted {
			return Err(Error::Codec(anyhow::anyhow!("DMA-BUF sync: {err}")));
		}
	}
}

fn dma_buf_allocation_size(fd: BorrowedFd<'_>, map_offset: u32) -> std::io::Result<Option<usize>> {
	let raw = std::os::fd::AsRawFd::as_raw_fd(&fd);
	dma_buf_allocation_size_with_seek(map_offset, |offset, whence| seek_fd(raw, offset, whence))
}

fn dma_buf_allocation_size_with_seek(
	map_offset: u32,
	mut seek: impl FnMut(libc::off_t, libc::c_int) -> std::io::Result<libc::off_t>,
) -> std::io::Result<Option<usize>> {
	// DMA-BUF supports SEEK_END specifically so userspace can discover the
	// allocation size even though fstat commonly reports zero.
	let end = seek(0, libc::SEEK_END)?;
	// DMA-BUF also supports SEEK_SET. Reset the shared file description to the
	// canonical position before handing the descriptor to another subsystem.
	seek(0, libc::SEEK_SET)?;
	let Ok(end) = usize::try_from(end) else {
		return Ok(None);
	};
	Ok(end.checked_sub(map_offset as usize).filter(|size| *size > 0))
}

fn seek_fd(fd: std::os::fd::RawFd, offset: libc::off_t, whence: libc::c_int) -> std::io::Result<libc::off_t> {
	loop {
		// SAFETY: the caller keeps `fd` open for the duration of this syscall.
		let result = unsafe { libc::lseek(fd, offset, whence) };
		if result >= 0 {
			return Ok(result);
		}
		let error = std::io::Error::last_os_error();
		if error.kind() != std::io::ErrorKind::Interrupted {
			return Err(error);
		}
	}
}

/// Read-only mmap of one linear DMA-BUF allocation.
struct Mapping {
	ptr: *mut libc::c_void,
	len: usize,
}

impl Mapping {
	fn new(fd: &OwnedFd, offset: u32, len: usize) -> Result<Self, Error> {
		if len == 0 {
			return Err(Error::Codec(anyhow::anyhow!("cannot map an empty DMA-BUF")));
		}
		// SAFETY: `fd` stays open for the mapping's lifetime, `len` is non-zero,
		// and the kernel validates the PipeWire-provided offset and allocation.
		let ptr = unsafe {
			libc::mmap(
				std::ptr::null_mut(),
				len,
				libc::PROT_READ,
				libc::MAP_SHARED,
				std::os::fd::AsRawFd::as_raw_fd(fd),
				offset as libc::off_t,
			)
		};
		if ptr == libc::MAP_FAILED {
			return Err(Error::Codec(anyhow::anyhow!(
				"DMA-BUF mmap: {}",
				std::io::Error::last_os_error()
			)));
		}
		Ok(Self { ptr, len })
	}

	fn as_slice(&self) -> &[u8] {
		// SAFETY: `ptr` and `len` came from a successful `mmap`, and `self`
		// keeps that mapping alive for the returned slice's lifetime.
		unsafe { std::slice::from_raw_parts(self.ptr.cast(), self.len) }
	}
}

impl Drop for Mapping {
	fn drop(&mut self) {
		// SAFETY: this exact pointer and length came from the successful `mmap`
		// in `Mapping::new`, and the mapping is released exactly once here.
		unsafe {
			libc::munmap(self.ptr, self.len);
		}
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

	let mut packed = vec![0; width * height * 3 / 2];
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

/// Queues a raw PipeWire buffer unless ownership is transferred to a DMA-BUF
/// surface. This mirrors `pipewire::buffer::Buffer` while allowing the surface
/// to return the buffer asynchronously on the PipeWire loop thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Lease {
	buffer: usize,
	generation: u64,
}

impl Lease {
	fn current(self, generation: u64) -> Option<*mut pw::sys::pw_buffer> {
		(self.generation == generation).then_some(self.buffer as *mut pw::sys::pw_buffer)
	}
}

struct Dequeued<'a> {
	stream: &'a pw::stream::Stream,
	raw: *mut pw::sys::pw_buffer,
	queue: bool,
}

impl<'a> Dequeued<'a> {
	unsafe fn new(stream: &'a pw::stream::Stream) -> Option<Self> {
		// SAFETY: PipeWire permits one dequeue from this process callback. The
		// wrapper below returns the pointer to this same stream unless leased.
		let raw = unsafe { stream.dequeue_raw_buffer() };
		(!raw.is_null()).then_some(Self {
			stream,
			raw,
			queue: true,
		})
	}

	fn datas_mut(&mut self) -> &mut [spa::buffer::Data] {
		// SAFETY: `raw` is a non-null buffer dequeued from `stream` and remains
		// out of PipeWire's pool while this wrapper owns it.
		let buffer = unsafe { (*self.raw).buffer };
		if buffer.is_null() || unsafe { (*buffer).n_datas == 0 || (*buffer).datas.is_null() } {
			return &mut [];
		}
		// SAFETY: PipeWire owns this SPA buffer and guarantees its `datas` array
		// has `n_datas` entries until the buffer is queued back.
		unsafe {
			std::slice::from_raw_parts_mut((*buffer).datas.cast::<spa::buffer::Data>(), (*buffer).n_datas as usize)
		}
	}

	fn lease(mut self, generation: u64) -> Lease {
		self.queue = false;
		Lease {
			buffer: self.raw as usize,
			generation,
		}
	}
}

impl Drop for Dequeued<'_> {
	fn drop(&mut self) {
		if self.queue {
			// SAFETY: the pointer came from `self.stream` and has not previously
			// been returned or transferred to a leased surface.
			unsafe { self.stream.queue_raw_buffer(self.raw) };
		}
	}
}

struct CaptureLoop {
	fd: OwnedFd,
	node_id: u32,
	framerate: u32,
	chan: Arc<FrameChannel>,
	state: Rc<RefCell<State>>,
	quit_rx: pw::channel::Receiver<()>,
	return_rx: pw::channel::Receiver<Lease>,
	return_tx: pw::channel::Sender<Lease>,
}

#[derive(Debug, PartialEq, Eq)]
enum NegotiatedMemory {
	Fixating,
	SharedMemory,
	DmaBuf(u64),
}

fn negotiated_memory(param: &spa::pod::Pod, format: VideoInfoRaw) -> Option<NegotiatedMemory> {
	let object = param.as_object().ok()?;
	let modifier = object.find_prop(spa::utils::Id(
		spa::param::format::FormatProperties::VideoModifier.as_raw(),
	));
	Some(match modifier {
		Some(modifier) if modifier.flags().contains(spa::pod::PodPropFlags::DONT_FIXATE) => NegotiatedMemory::Fixating,
		Some(_) => NegotiatedMemory::DmaBuf(format.modifier()),
		None => NegotiatedMemory::SharedMemory,
	})
}

fn fixate_modifier(param: &spa::pod::Pod, modifier: u64) -> Option<Vec<u8>> {
	let (_, value) = spa::pod::deserialize::PodDeserializer::deserialize_any_from(param.as_bytes()).ok()?;
	let spa::pod::Value::Object(mut object) = value else {
		return None;
	};
	let property = object
		.properties
		.iter_mut()
		.find(|property| property.key == spa::param::format::FormatProperties::VideoModifier.as_raw())?;
	property.flags.remove(spa::pod::PropertyFlags::from_bits_retain(
		spa::sys::SPA_POD_PROP_FLAG_DONT_FIXATE,
	));
	property.value = spa::pod::Value::Long(modifier as i64);
	spa::pod::serialize::PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &spa::pod::Value::Object(object))
		.ok()
		.map(|serialized| serialized.0.into_inner())
}

/// Connect to the portal's PipeWire node and run until the stream ends, the
/// consumer drops, or the format changes.
fn run_loop(args: CaptureLoop) -> Result<(), Error> {
	let CaptureLoop {
		fd,
		node_id,
		framerate,
		chan,
		state,
		quit_rx,
		return_rx,
		return_tx,
	} = args;
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

	// DMA-BUF surfaces return their dequeued PipeWire buffer here on the loop
	// thread. A surface may drop on an encoder or renderer task, where calling
	// `pw_stream_queue_buffer` directly would violate PipeWire's threading model.
	let _returns = return_rx.attach(mainloop.loop_(), {
		let stream = stream.downgrade();
		let state = state.clone();
		move |lease| {
			if let Some(stream) = stream.upgrade()
				&& let Some(raw) = lease.current(state.borrow().generation)
			{
				// SAFETY: leased surfaces send each pointer exactly once, and every
				// current-generation pointer was dequeued from this stream's pool.
				unsafe { stream.queue_raw_buffer(raw) };
			}
		}
	});

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
			move |stream, _, id, param| {
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
				let Some(memory) = negotiated_memory(param, state.format) else {
					return;
				};
				let dmabuf_modifier = match memory {
					NegotiatedMemory::DmaBuf(modifier) => Some(modifier),
					NegotiatedMemory::SharedMemory => None,
					NegotiatedMemory::Fixating => {
						let Some(fixed) = fixate_modifier(param, state.format.modifier()) else {
							tracing::warn!("failed to fixate the PipeWire DMA-BUF modifier");
							return;
						};
						let Some(param) = spa::pod::Pod::from_bytes(&fixed) else {
							return;
						};
						let mut params = [param];
						if let Err(e) = stream.update_params(&mut params) {
							tracing::warn!(error = %e, "failed to fixate the PipeWire DMA-BUF modifier");
						}
						return;
					}
				};
				state.last = None;
				state.dmabuf_modifier = dmabuf_modifier;
				let buffers = buffer_offer(state.dmabuf_modifier.is_some());
				if let Some(param) = spa::pod::Pod::from_bytes(&buffers) {
					let mut params = [param];
					if let Err(e) = stream.update_params(&mut params) {
						tracing::warn!(error = %e, "DMA-BUF buffer negotiation failed; capture may use shared memory");
					} else {
						state.generation = state.generation.wrapping_add(1);
					}
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
			let return_tx = return_tx.clone();
			move |stream, _| {
				let mut state = state.borrow_mut();
				let Some((width, height)) = state.geometry else { return };
				let source_height = state.format.size().height;
				let color = state.color;
				// SAFETY: this is PipeWire's process callback for `stream`; `Dequeued`
				// returns the buffer on drop unless a DMA-BUF surface leases it.
				let Some(mut buffer) = (unsafe { Dequeued::new(stream) }) else {
					return;
				};
				let datas = buffer.datas_mut();
				let [data] = datas else {
					tracing::warn!(
						blocks = datas.len(),
						"pipewire ignored the negotiated single-block layout"
					);
					return;
				};

				let chunk_offset = data.chunk().offset();
				let (map_offset, maxsize) = {
					let raw = data.as_raw();
					(raw.mapoffset, raw.maxsize)
				};
				let dmabuf = data.type_() == DataType::DmaBuf;
				let chunk_size = data.chunk().size();
				let size = if dmabuf {
					dma_buf_chunk_size(chunk_size, maxsize)
				} else {
					clamp_chunk_size(chunk_size, maxsize)
				};
				let stride = match u32::try_from(data.chunk().stride()) {
					Ok(stride) if stride > 0 => stride,
					_ => match state.format.format() {
						VideoFormat::NV12 => state.format.size().width,
						VideoFormat::BGRx | VideoFormat::BGRA | VideoFormat::RGBx | VideoFormat::RGBA => {
							state.format.size().width.saturating_mul(4)
						}
						_ => 0,
					},
				};
				let layout = FrameLayout {
					stride,
					width,
					height,
					source_height,
				};
				match chunk_kind(size, data.chunk().flags(), dmabuf) {
					ChunkKind::Data => {}
					// An empty screencast chunk means there is no new damage. Keep
					// the previous frame for the pacing timer instead of blanking it.
					ChunkKind::Empty => return,
					ChunkKind::Invalid => return,
				}
				let Some(required) = frame_data_size(state.format.format(), layout) else {
					tracing::warn!("pipewire frame layout overflows its buffer");
					return;
				};
				if dmabuf {
					let Some(format) = drm_format(state.format.format()) else {
						tracing::warn!(format = ?state.format.format(), "unsupported DMA-BUF pixel format");
						return;
					};
					if data.fd() < 0 || stride == 0 {
						tracing::warn!("DMA-BUF has no valid fd or row stride");
						return;
					}
					// SAFETY: PipeWire owns this fd while the buffer is dequeued. It is
					// duplicated immediately, before the buffer can be queued again.
					let fd = unsafe { BorrowedFd::borrow_raw(data.fd()) };
					let Ok(fd) = fd.try_clone_to_owned() else {
						return;
					};
					let offset = normalize_dma_buf_offset(chunk_offset, maxsize);
					if !dma_buf_chunk_contains(required, chunk_size, maxsize) {
						tracing::warn!(
							required,
							available = size,
							"DMA-BUF chunk does not contain a complete frame"
						);
						return;
					}
					let allocation_size = match dma_buf_allocation_size(fd.as_fd(), map_offset) {
						Ok(Some(size)) if offset.checked_add(required).is_some_and(|end| end <= size) => Some(size),
						Ok(Some(size)) => {
							tracing::warn!(
								required,
								available = size,
								"DMA-BUF allocation is shorter than its frame layout"
							);
							return;
						}
						Ok(None) => None,
						Err(error) => {
							tracing::debug!(%error, "could not query DMA-BUF allocation size; CPU fallback is unavailable");
							None
						}
					};
					let Some(base) = map_offset.checked_add(offset as u32) else {
						tracing::warn!("DMA-BUF plane offset overflow");
						return;
					};
					let planes = if format == DrmFormat::NV12 {
						let Some(uv) = layout
							.stride
							.checked_mul(layout.source_height)
							.and_then(|size| base.checked_add(size))
						else {
							tracing::warn!("DMA-BUF chroma plane offset overflow");
							return;
						};
						vec![
							DmaBufPlane::new(base, layout.stride),
							DmaBufPlane::new(uv, layout.stride),
						]
					} else {
						vec![DmaBufPlane::new(base, layout.stride)]
					};
					let Some(modifier) = state.dmabuf_modifier else {
						tracing::warn!("received a DMA-BUF without a negotiated modifier");
						return;
					};
					let lease = buffer.lease(state.generation);
					let inner = Arc::new(PipeWireDmaBuf {
						fd,
						return_tx: return_tx.clone(),
						lease,
						map_offset,
						allocation_size,
						data_offset: offset,
						layout,
						format,
						modifier,
						color,
					});
					match DmaBuf::new(format, modifier, layout.width, layout.height, planes, color, inner) {
						Ok(frame) => {
							chan.push(Surface::DmaBuf(frame.clone()));
							state.last = Some(Last::DmaBuf(frame));
							state.fresh = true;
						}
						Err(e) => tracing::warn!(error = %e, "invalid PipeWire DMA-BUF"),
					}
					return;
				}
				let allocation_size = maxsize as usize;
				let Some(offset) = normalize_chunk_offset(chunk_offset, maxsize) else {
					tracing::warn!("pipewire buffer has zero maximum size");
					return;
				};
				if required > size || required > allocation_size {
					tracing::warn!(
						required,
						available = size,
						"pipewire chunk does not contain a complete frame"
					);
					return;
				}
				// MAP_BUFFERS maps MemPtr/MemFd for the CPU fallback. `None` here
				// means the producer forced an unsupported buffer representation.
				let Some(bytes) = data.data() else {
					tracing::warn!("pipewire buffer is not CPU-mapped; stopping capture");
					if let Some(mainloop) = mainloop.upgrade() {
						mainloop.quit();
					}
					return;
				};
				let Some(allocation) = bytes.get(..allocation_size) else {
					return;
				};
				let Some(bytes) = chunk_bytes(allocation, offset, required) else {
					return;
				};
				match convert(state.format.format(), bytes.as_ref(), layout, color) {
					Ok(i420) => {
						chan.push(Surface::I420(i420.clone()));
						state.last = Some(Last::I420(i420));
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

	// DMA-BUF formats come first so PipeWire prefers them. Each has a matching
	// shared-memory offer without a modifier as the required fallback.
	let offers = format_offers(framerate);
	let mut params = offers
		.iter()
		.map(|offer| {
			spa::pod::Pod::from_bytes(offer)
				.ok_or_else(|| Error::Codec(anyhow::anyhow!("failed to build pipewire format offer")))
		})
		.collect::<Result<Vec<_>, _>>()?;
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
				chan.push(last.surface());
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

fn normalize_chunk_offset(offset: u32, maxsize: u32) -> Option<usize> {
	(maxsize != 0).then(|| (offset % maxsize) as usize)
}

fn normalize_dma_buf_offset(offset: u32, maxsize: u32) -> usize {
	normalize_chunk_offset(offset, maxsize).unwrap_or(offset as usize)
}

fn clamp_chunk_size(size: u32, maxsize: u32) -> usize {
	size.min(maxsize) as usize
}

fn dma_buf_chunk_size(size: u32, maxsize: u32) -> usize {
	if maxsize == 0 {
		size as usize
	} else {
		clamp_chunk_size(size, maxsize)
	}
}

fn dma_buf_chunk_contains(required: usize, size: u32, maxsize: u32) -> bool {
	size == 0 || required <= dma_buf_chunk_size(size, maxsize)
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
		VideoFormat::NV12 => (layout.source_height as usize)
			.checked_add(height / 2)?
			.checked_sub(1)?
			.checked_mul(stride)?
			.checked_add(row_size),
		_ => height.checked_sub(1)?.checked_mul(stride)?.checked_add(row_size),
	}
}

#[derive(Debug, PartialEq, Eq)]
enum ChunkKind {
	Data,
	Empty,
	Invalid,
}

fn chunk_kind(size: usize, flags: spa::buffer::ChunkFlags, dmabuf: bool) -> ChunkKind {
	if flags.contains(spa::buffer::ChunkFlags::CORRUPTED) {
		ChunkKind::Invalid
	} else if flags.bits() & CHUNK_FLAG_EMPTY != 0 {
		ChunkKind::Empty
	} else if size == 0 && !dmabuf {
		ChunkKind::Invalid
	} else {
		ChunkKind::Data
	}
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

/// Map the negotiated SPA layout to its DRM fourcc.
fn drm_format(format: VideoFormat) -> Option<DrmFormat> {
	match format {
		VideoFormat::NV12 => Some(DrmFormat::NV12),
		VideoFormat::BGRx => Some(DrmFormat::XRGB8888),
		VideoFormat::BGRA => Some(DrmFormat::ARGB8888),
		VideoFormat::RGBx => Some(DrmFormat::XBGR8888),
		VideoFormat::RGBA => Some(DrmFormat::ABGR8888),
		_ => None,
	}
}

const PIPEWIRE_FORMATS: [VideoFormat; 5] = [
	VideoFormat::BGRx,
	VideoFormat::BGRA,
	VideoFormat::RGBx,
	VideoFormat::RGBA,
	VideoFormat::NV12,
];

/// Serialize one `EnumFormat` pod for a concrete pixel format.
fn format_offer(framerate: u32, format: VideoFormat, dmabuf: bool) -> Vec<u8> {
	let mut obj = spa::pod::object!(
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
		spa::pod::property!(spa::param::format::FormatProperties::VideoFormat, Id, format),
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
	if dmabuf {
		obj.properties.push(spa::pod::Property {
			key: spa::param::format::FormatProperties::VideoModifier.as_raw(),
			flags: spa::pod::PropertyFlags::from_bits_retain(
				spa::sys::SPA_POD_PROP_FLAG_MANDATORY | spa::sys::SPA_POD_PROP_FLAG_DONT_FIXATE,
			),
			value: spa::pod::Value::Choice(spa::pod::ChoiceValue::Long(spa::utils::Choice(
				spa::utils::ChoiceFlags::empty(),
				spa::utils::ChoiceEnum::Enum {
					default: 0,
					alternatives: vec![0],
				},
			))),
		});
	}
	spa::pod::serialize::PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &spa::pod::Value::Object(obj))
		.expect("serializing a static format pod cannot fail")
		.0
		.into_inner()
}

/// Serialize DMA-BUF formats first and shared-memory fallbacks second.
fn format_offers(framerate: u32) -> Vec<Vec<u8>> {
	PIPEWIRE_FORMATS
		.into_iter()
		.map(|format| format_offer(framerate, format, true))
		.chain(
			PIPEWIRE_FORMATS
				.into_iter()
				.map(|format| format_offer(framerate, format, false)),
		)
		.collect()
}

/// Serialize `SPA_PARAM_Buffers` for the negotiated memory representation.
fn buffer_offer(dmabuf: bool) -> Vec<u8> {
	let mem_ptr = 1 << DataType::MemPtr.as_raw();
	let mem_fd = 1 << DataType::MemFd.as_raw();
	let dma_buf = 1 << DataType::DmaBuf.as_raw();
	let data_types = if dmabuf { dma_buf } else { mem_fd | mem_ptr };

	let obj = spa::pod::Object {
		type_: spa::utils::SpaTypes::ObjectParamBuffers.as_raw(),
		id: spa::param::ParamType::Buffers.as_raw(),
		properties: vec![
			spa::pod::Property::new(
				spa::sys::SPA_PARAM_BUFFERS_buffers,
				spa::pod::Value::Choice(spa::pod::ChoiceValue::Int(spa::utils::Choice(
					spa::utils::ChoiceFlags::empty(),
					spa::utils::ChoiceEnum::Range {
						default: 8,
						min: 2,
						max: 64,
					},
				))),
			),
			spa::pod::Property::new(spa::sys::SPA_PARAM_BUFFERS_blocks, spa::pod::Value::Int(1)),
			spa::pod::Property::new(
				spa::sys::SPA_PARAM_BUFFERS_dataType,
				spa::pod::Value::Choice(spa::pod::ChoiceValue::Int(spa::utils::Choice(
					spa::utils::ChoiceFlags::empty(),
					spa::utils::ChoiceEnum::Flags {
						default: data_types,
						flags: Vec::new(),
					},
				))),
			),
		],
	};
	spa::pod::serialize::PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &spa::pod::Value::Object(obj))
		.expect("serializing a static buffer pod cannot fail")
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
		let offers = format_offers(30);
		assert_eq!(offers.len(), PIPEWIRE_FORMATS.len() * 2);
		for (index, bytes) in offers.iter().enumerate() {
			let (remaining, value) = spa::pod::deserialize::PodDeserializer::deserialize_any_from(bytes)
				.expect("format offer did not round-trip");
			assert!(remaining.is_empty());
			let spa::pod::Value::Object(object) = value else {
				panic!("format offer is not an object");
			};
			let property = |key| object.properties.iter().find(|property| property.key == key);
			let format = PIPEWIRE_FORMATS[index % PIPEWIRE_FORMATS.len()];
			assert_eq!(
				property(spa::param::format::FormatProperties::VideoFormat.as_raw()).map(|p| &p.value),
				Some(&spa::pod::Value::Id(spa::utils::Id(format.as_raw())))
			);
			let modifier = property(spa::param::format::FormatProperties::VideoModifier.as_raw());
			if index < PIPEWIRE_FORMATS.len() {
				let modifier = modifier.expect("DMA-BUF offer has no modifier");
				assert_eq!(
					modifier.flags.bits(),
					spa::sys::SPA_POD_PROP_FLAG_MANDATORY | spa::sys::SPA_POD_PROP_FLAG_DONT_FIXATE
				);
				assert_eq!(
					modifier.value,
					spa::pod::Value::Choice(spa::pod::ChoiceValue::Long(spa::utils::Choice(
						spa::utils::ChoiceFlags::empty(),
						spa::utils::ChoiceEnum::Enum {
							default: 0,
							alternatives: vec![0],
						},
					)))
				);
			} else {
				assert!(modifier.is_none(), "shared-memory offer contains a modifier");
			}
		}
	}

	#[test]
	fn negotiated_modifier_must_be_present_and_fixed() {
		let shared = format_offer(30, VideoFormat::BGRx, false);
		let shared = spa::pod::Pod::from_bytes(&shared).unwrap();
		let mut format = VideoInfoRaw::default();
		format.parse(shared).unwrap();
		assert_eq!(negotiated_memory(shared, format), Some(NegotiatedMemory::SharedMemory));

		let offered = format_offer(30, VideoFormat::BGRx, true);
		let offered = spa::pod::Pod::from_bytes(&offered).unwrap();
		format.parse(offered).unwrap();
		assert_eq!(negotiated_memory(offered, format), Some(NegotiatedMemory::Fixating));

		let fixed = fixate_modifier(offered, format.modifier()).unwrap();
		let fixed = spa::pod::Pod::from_bytes(&fixed).unwrap();
		replace_video_format(&mut format, |format| format.parse(fixed)).unwrap();
		assert_eq!(negotiated_memory(fixed, format), Some(NegotiatedMemory::DmaBuf(0)));
	}

	#[test]
	fn buffer_offer_is_valid_pod() {
		let dma_buf = 1 << DataType::DmaBuf.as_raw();
		let mem_fd = 1 << DataType::MemFd.as_raw();
		let mem_ptr = 1 << DataType::MemPtr.as_raw();
		for (dmabuf, data_types) in [(true, dma_buf), (false, mem_fd | mem_ptr)] {
			let bytes = buffer_offer(dmabuf);
			let (remaining, value) = spa::pod::deserialize::PodDeserializer::deserialize_any_from(&bytes)
				.expect("buffer offer did not round-trip");
			assert!(remaining.is_empty());
			let spa::pod::Value::Object(object) = value else {
				panic!("buffer offer is not an object");
			};
			let property = |key| {
				&object
					.properties
					.iter()
					.find(|property| property.key == key)
					.unwrap_or_else(|| panic!("missing buffer property {key}"))
					.value
			};
			assert_eq!(
				property(spa::sys::SPA_PARAM_BUFFERS_buffers),
				&spa::pod::Value::Choice(spa::pod::ChoiceValue::Int(spa::utils::Choice(
					spa::utils::ChoiceFlags::empty(),
					spa::utils::ChoiceEnum::Range {
						default: 8,
						min: 2,
						max: 64,
					},
				)))
			);
			assert_eq!(property(spa::sys::SPA_PARAM_BUFFERS_blocks), &spa::pod::Value::Int(1));
			assert_eq!(
				property(spa::sys::SPA_PARAM_BUFFERS_dataType),
				&spa::pod::Value::Choice(spa::pod::ChoiceValue::Int(spa::utils::Choice(
					spa::utils::ChoiceFlags::empty(),
					spa::utils::ChoiceEnum::Flags {
						default: data_types,
						flags: Vec::new(),
					},
				)))
			);
		}
	}

	#[test]
	fn chunk_offset_wraps_to_the_allocation() {
		assert_eq!(normalize_chunk_offset(18, 16), Some(2));
		assert_eq!(normalize_chunk_offset(0, 0), None);
		assert_eq!(normalize_dma_buf_offset(18, 16), 2);
		assert_eq!(normalize_dma_buf_offset(18, 0), 18);
	}

	#[test]
	fn chunk_size_is_clamped_to_the_allocation() {
		assert_eq!(clamp_chunk_size(18, 16), 16);
		assert_eq!(clamp_chunk_size(8, 16), 8);
		assert_eq!(dma_buf_chunk_size(8, 16), 8);
		assert_eq!(dma_buf_chunk_size(8, 0), 8);
		assert!(dma_buf_chunk_contains(16, 0, 0));
		assert!(!dma_buf_chunk_contains(16, 8, 0));
		assert!(dma_buf_chunk_contains(16, 16, 16));
	}

	#[test]
	fn frame_data_size_covers_every_sampled_row() {
		let packed = FrameLayout {
			stride: 20,
			width: 4,
			height: 2,
			source_height: 2,
		};
		assert_eq!(frame_data_size(VideoFormat::BGRx, packed), Some(36));

		let nv12 = FrameLayout {
			stride: 6,
			width: 4,
			height: 2,
			source_height: 3,
		};
		assert_eq!(frame_data_size(VideoFormat::NV12, nv12), Some(22));
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
	fn chunk_flags_distinguish_empty_and_invalid_frames() {
		let empty = spa::buffer::ChunkFlags::from_bits_retain(CHUNK_FLAG_EMPTY);
		assert_eq!(
			chunk_kind(0, spa::buffer::ChunkFlags::empty(), false),
			ChunkKind::Invalid
		);
		assert_eq!(chunk_kind(0, spa::buffer::ChunkFlags::empty(), true), ChunkKind::Data);
		assert_eq!(
			chunk_kind(1, spa::buffer::ChunkFlags::CORRUPTED, false),
			ChunkKind::Invalid
		);
		assert_eq!(chunk_kind(1, empty, false), ChunkKind::Empty);
		assert_eq!(chunk_kind(0, empty, true), ChunkKind::Empty);
		assert_eq!(chunk_kind(1, spa::buffer::ChunkFlags::empty(), false), ChunkKind::Data);
	}

	#[test]
	fn dmabuf_size_comes_from_seek_end_not_stat() {
		let mut calls = Vec::new();
		let size = dma_buf_allocation_size_with_seek(1024, |offset, whence| {
			calls.push((offset, whence));
			match whence {
				libc::SEEK_END => Ok(4096),
				libc::SEEK_SET => Ok(0),
				_ => panic!("unexpected seek mode {whence}"),
			}
		})
		.unwrap();

		// This models a DMA-BUF anon inode: no stat size is available, and the
		// allocation length comes exclusively from its SEEK_END operation.
		assert_eq!(size, Some(3072));
		assert_eq!(calls, [(0, libc::SEEK_END), (0, libc::SEEK_SET)]);
		assert_eq!(
			dma_buf_allocation_size_with_seek(4096, |_, whence| match whence {
				libc::SEEK_END => Ok(4096),
				libc::SEEK_SET => Ok(0),
				_ => unreachable!(),
			})
			.unwrap(),
			None
		);
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
	fn stale_pool_lease_is_not_current() {
		let buffer = std::ptr::dangling_mut::<pw::sys::pw_buffer>();
		let lease = Lease {
			buffer: buffer as usize,
			generation: 2,
		};
		assert_eq!(lease.current(1), None);
		assert_eq!(lease.current(2), Some(buffer));
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

	/// A duplicated fd does not protect the pixels from producer reuse. The
	/// dequeued PipeWire buffer must be returned only after the last surface
	/// clone drops, and its return crosses back onto the PipeWire loop.
	#[test]
	fn dmabuf_returns_on_last_drop() {
		use std::cell::Cell;
		use std::io::Write;
		use std::os::fd::OwnedFd;
		use std::os::unix::net::UnixStream;
		use std::rc::Rc;

		pw::init();
		let mainloop = pw::main_loop::MainLoopRc::new(None).expect("main loop");
		let (return_tx, return_rx) = pw::channel::channel::<Lease>();
		let returned = Rc::new(Cell::new(None));
		let _returns = return_rx.attach(mainloop.loop_(), {
			let mainloop = mainloop.downgrade();
			let returned = returned.clone();
			move |raw| {
				returned.set(Some(raw));
				mainloop.upgrade().expect("live loop").quit();
			}
		});
		let timer = mainloop.loop_().add_timer({
			let mainloop = mainloop.downgrade();
			move |_| mainloop.upgrade().expect("live loop").quit()
		});

		let (socket, mut peer) = UnixStream::pair().expect("fd pair");
		peer.write_all(&[0]).expect("signal readable");
		let inner = Arc::new(PipeWireDmaBuf {
			fd: OwnedFd::from(socket),
			return_tx,
			lease: Lease {
				buffer: 7,
				generation: 1,
			},
			map_offset: 0,
			allocation_size: Some(6),
			data_offset: 0,
			layout: FrameLayout {
				stride: 2,
				width: 2,
				height: 2,
				source_height: 2,
			},
			format: DrmFormat::NV12,
			modifier: 0,
			color: Some(Color::Bt709Full),
		});
		let frame = DmaBuf::new(
			DrmFormat::NV12,
			0,
			2,
			2,
			vec![DmaBufPlane::new(0, 2), DmaBufPlane::new(4, 2)],
			Some(Color::Bt709Full),
			inner,
		)
		.expect("DMA-BUF");
		assert_eq!(frame.format(), DrmFormat::NV12);
		assert_eq!(frame.modifier(), 0);
		assert_eq!((frame.width(), frame.height()), (2, 2));
		assert_eq!(frame.planes()[1], DmaBufPlane::new(4, 2));
		assert_eq!(Surface::DmaBuf(frame.clone()).color(), Some(Color::Bt709Full));
		let export = frame.export().expect("exported fd");
		let clone = frame.clone();

		drop(frame);
		timer
			.update_timer(Some(Duration::from_millis(10)), None)
			.into_result()
			.expect("timer");
		mainloop.run();
		assert_eq!(returned.get(), None, "one live clone still owns the lease");

		drop(clone);
		timer
			.update_timer(Some(Duration::from_millis(10)), None)
			.into_result()
			.expect("timer");
		mainloop.run();
		assert_eq!(returned.get(), None, "one live export still owns the lease");

		drop(export);
		timer
			.update_timer(Some(Duration::from_secs(1)), None)
			.into_result()
			.expect("timeout timer");
		mainloop.run();
		assert_eq!(
			returned.get(),
			Some(Lease {
				buffer: 7,
				generation: 1
			})
		);
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
