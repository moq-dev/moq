//! Native X11 display and window capture.
//!
//! Wayland capture goes through the desktop portal because direct global
//! capture is intentionally forbidden there. An X11 session does expose stable
//! monitor and window ids, so this backend enumerates them with RandR and pulls
//! pixels through MIT-SHM, falling back to `GetImage` without local shared
//! memory. It is also the fallback when the `pipewire` feature is disabled
//! on an X11 desktop.

use std::os::fd::AsRawFd;
use std::ptr::NonNull;
use std::sync::Arc;
use std::time::{Duration, Instant};

use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::Event;
use x11rb::protocol::randr::{self, ConnectionExt as _};
use x11rb::protocol::shm::{self, ConnectionExt as _};
use x11rb::protocol::xfixes::{ConnectionExt as _, GetCursorImageReply};
use x11rb::protocol::xproto::{
	AtomEnum, ChangeWindowAttributesAux, ConnectionExt as _, Drawable, EventMask, ImageFormat, ImageOrder, MapState,
	VisualClass, Window as XWindow,
};
use x11rb::rust_connection::RustConnection;

use super::channel::FrameChannel;
use super::pump::{self, Geometry};
use super::settle::{Settle, Settled};
use super::{Config, Display, Stream, Window};
use crate::Error;
use crate::frame::{I420, Surface};

const DEFAULT_FRAMERATE: u32 = 30;

/// Whether a display selector belongs to X11. A desktop without Wayland also
/// selects X11 by default, making it the no-portal fallback.
pub(super) fn selected(selector: Option<&str>) -> bool {
	selects_x11(selector, std::env::var_os("WAYLAND_DISPLAY").is_some())
}

fn selects_x11(selector: Option<&str>, wayland: bool) -> bool {
	selector.is_some_and(|selector| selector.starts_with("x11:")) || !wayland
}

fn available() -> bool {
	is_available(
		std::env::var_os("WAYLAND_DISPLAY").is_some(),
		std::env::var_os("DISPLAY").is_some(),
	)
}

fn is_available(wayland: bool, x11: bool) -> bool {
	x11 || !wayland
}

pub(super) fn displays() -> Result<Vec<Display>, Error> {
	if !available() {
		return Err(Error::Unsupported(
			"listing displays on Wayland without XWayland; the desktop portal owns selection".to_string(),
		));
	}
	let (connection, screen) = connect()?;
	let root = connection.setup().roots[screen].root;
	let monitors = monitors(&connection, root)?;
	Ok(monitors
		.into_iter()
		.enumerate()
		.map(|(index, monitor)| Display {
			id: format!("x11:{index}"),
			name: monitor.name,
			width: monitor.width,
			height: monitor.height,
		})
		.collect())
}

pub(super) fn windows() -> Result<Vec<Window>, Error> {
	if !available() {
		return Err(Error::Unsupported(
			"listing windows on Wayland without XWayland; the desktop portal does not expose them".to_string(),
		));
	}
	let (connection, screen) = connect()?;
	let root = connection.setup().roots[screen].root;
	let name_atom = intern(&connection, b"_NET_WM_NAME")?;
	let utf8_atom = intern(&connection, b"UTF8_STRING")?;

	let mut result = Vec::new();
	for id in client_windows(&connection, root)? {
		match connection.get_window_attributes(id).map_err(codec)?.reply() {
			Ok(attributes) if attributes.map_state == MapState::VIEWABLE => {}
			_ => continue,
		}
		let geometry = match connection.get_geometry(id).map_err(codec)?.reply() {
			Ok(geometry) if geometry.width > 1 && geometry.height > 1 => geometry,
			_ => continue,
		};
		let title = property(&connection, id, name_atom, utf8_atom)
			.or_else(|| property(&connection, id, AtomEnum::WM_NAME.into(), AtomEnum::STRING.into()))
			.unwrap_or_default();
		if title.is_empty() {
			continue;
		}
		let app = property(&connection, id, AtomEnum::WM_CLASS.into(), AtomEnum::STRING.into())
			.map(|value| wm_class(&value))
			.unwrap_or_default();
		result.push(Window {
			id: format!("x11:{id}"),
			title,
			app,
			width: geometry.width.into(),
			height: geometry.height.into(),
		});
	}
	Ok(result)
}

pub(super) async fn open_display(config: &Config, selector: Option<&str>) -> Result<Stream, Error> {
	if !selected(selector) {
		return Err(Error::Unsupported(
			"screen capture on Wayland without the `pipewire` feature".to_string(),
		));
	}
	let selector = selector.map(|selector| parse_selector(selector, "x11:")).transpose()?;
	open(config, Target::Display(selector)).await
}

pub(super) async fn open_window(config: &Config, selector: &str) -> Result<Stream, Error> {
	if !selected(Some(selector)) {
		return Err(Error::Unsupported("window capture on Wayland".to_string()));
	}
	let id = parse_selector(selector, "x11:")?;
	open(config, Target::Window(id)).await
}

async fn open(config: &Config, target: Target) -> Result<Stream, Error> {
	let config = config.clone();
	let chan = FrameChannel::new();
	let (geometry, guard) = pump::spawn(
		chan.clone(),
		move || {
			let capture = Capture::open(&config, target)?;
			let geometry = Geometry {
				width: capture.width,
				height: capture.height,
				framerate: Some(capture.framerate),
				label: capture.name.clone(),
			};
			Ok((capture, geometry))
		},
		Capture::read,
	)
	.await?;

	Ok(Stream::new(
		chan,
		geometry.width,
		geometry.height,
		geometry.framerate,
		geometry.label,
		None,
		Box::new(guard),
	))
}

#[derive(Clone, Copy)]
enum Target {
	Display(Option<usize>),
	Window(usize),
}

struct Capture {
	connection: Arc<RustConnection>,
	drawable: Drawable,
	x: i16,
	y: i16,
	width: u32,
	height: u32,
	format: PixelFormat,
	shm: Option<SharedMemory>,
	rgb: Vec<u8>,
	observed: Observed,
	framerate: crate::Rate,
	interval: Duration,
	next: Instant,
	name: String,
	root: XWindow,
	cursor: bool,
	display: Option<DisplayTarget>,
	settle: Settle<Observed>,
	/// Whether the window was unmapped on the previous read, so the hold is
	/// logged when it starts and ends rather than once per frame.
	unmapped: bool,
}

/// What the backend watches for a change: window size or destruction, or
/// whether the selected monitor is still the one the stream opened with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Observed {
	Size(u32, u32),
	Monitor(bool),
	Destroyed,
}

impl Capture {
	fn open(config: &Config, target: Target) -> Result<Self, Error> {
		let (connection, screen_index) = connect()?;
		let connection = Arc::new(connection);
		let (root, root_depth, root_visual) = {
			let screen = &connection.setup().roots[screen_index];
			(screen.root, screen.root_depth, screen.root_visual)
		};
		let (drawable, x, y, width, height, depth, visual, name, display) = match target {
			Target::Display(selector) => {
				watch_display(&connection, root)?;
				let monitors = monitors(&connection, root)?;
				let index = select_monitor(&monitors, selector).ok_or_else(|| match selector {
					Some(index) => {
						Error::SourceUnavailable(format!("no X11 display at index {index} ({} found)", monitors.len()))
					}
					None => Error::SourceUnavailable("no X11 displays".to_string()),
				})?;
				let monitor = &monitors[index];
				(
					root,
					monitor.x,
					monitor.y,
					monitor.width,
					monitor.height,
					root_depth,
					root_visual,
					format!("x11:{index}"),
					Some(DisplayTarget {
						selector,
						index,
						monitor: monitor.clone(),
					}),
				)
			}
			Target::Window(id) => {
				let drawable =
					u32::try_from(id).map_err(|_| Error::SourceUnavailable(format!("invalid X11 window id {id}")))?;
				// An id is not an identity: X clients allocate window ids out of their
				// own range and reuse freed ones, so a destroyed window can be replaced
				// by one wearing the same id, which then answers `get_geometry` as if
				// nothing happened. StructureNotify is what pins capture to the window
				// the user picked, since DestroyNotify arrives whatever becomes of the
				// id afterwards. Event masks are per-client, so the window's owner sees
				// nothing. Subscribe before reading the window back, so everything
				// below describes a window we are already watching.
				connection
					.change_window_attributes(
						drawable,
						&ChangeWindowAttributesAux::new().event_mask(EventMask::STRUCTURE_NOTIFY),
					)
					.map_err(source)?
					.check()
					.map_err(source)?;
				let attributes = connection
					.get_window_attributes(drawable)
					.map_err(source)?
					.reply()
					.map_err(source)?;
				let geometry = connection
					.get_geometry(drawable)
					.map_err(source)?
					.reply()
					.map_err(source)?;
				(
					drawable,
					0,
					0,
					geometry.width.into(),
					geometry.height.into(),
					geometry.depth,
					attributes.visual,
					format!("x11:{id}"),
					None,
				)
			}
		};
		let width = width & !1;
		let height = height & !1;
		if width == 0 || height == 0 {
			return Err(Error::SourceUnavailable(format!("{name} has no capturable area")));
		}
		let format = PixelFormat::new(connection.setup(), depth, visual)?;
		let shm = SharedMemory::open(connection.clone(), format.required_len(width, height)?)?;
		tracing::debug!(source = %name, shared_memory = shm.is_some(), "X11 capture opened");
		let framerate = config.framerate.unwrap_or(crate::Rate::integer(DEFAULT_FRAMERATE));
		let interval = Duration::from_secs_f64(1.0 / framerate.as_f64());
		let cursor = config.cursor
			&& connection
				.xfixes_query_version(5, 0)
				.ok()
				.and_then(|cookie| cookie.reply().ok())
				.is_some();
		let opened = if display.is_some() {
			Observed::Monitor(true)
		} else {
			Observed::Size(width, height)
		};
		Ok(Self {
			connection,
			drawable,
			x,
			y,
			width,
			height,
			format,
			shm,
			rgb: Vec::new(),
			observed: opened,
			framerate,
			interval,
			next: Instant::now(),
			name,
			root,
			cursor,
			display,
			settle: Settle::new(opened),
			unmapped: false,
		})
	}

	fn read(&mut self) -> Result<pump::Read, Error> {
		let now = Instant::now();
		if self.next > now {
			std::thread::sleep(self.next - now);
		}
		self.next = Instant::now() + self.interval;
		// The encoder's geometry is fixed at open, so a settled change ends the
		// stream and the caller reopens against the new size.
		match self.settled()? {
			Settled::Open => {}
			Settled::Waiting => return Ok(pump::Read::Idle),
			Settled::Changed => return Ok(pump::Read::Done),
		}

		let width = u16::try_from(self.width).map_err(|_| Error::Codec(anyhow::anyhow!("X11 width is too large")))?;
		let height =
			u16::try_from(self.height).map_err(|_| Error::Codec(anyhow::anyhow!("X11 height is too large")))?;
		if let Err(error) = self.capture_image(width, height) {
			// A resize can land after the state check but before the image request.
			// The reply has queued the preceding events; only a confirmed source
			// change can turn a failed read into a hold or an orderly close.
			return match self.settled() {
				Ok(Settled::Waiting) => Ok(pump::Read::Idle),
				Ok(Settled::Changed) => Ok(pump::Read::Done),
				_ => Err(error),
			};
		}
		if self.cursor
			&& let Some(cursor) = self
				.connection
				.xfixes_get_cursor_image()
				.ok()
				.and_then(|cookie| cookie.reply().ok())
		{
			let (x, y) = self.origin()?;
			blend_cursor(&mut self.rgb, self.width, self.height, x, y, &cursor);
		}
		Ok(pump::Read::Frame(Surface::I420(I420::from_rgb(
			&self.rgb,
			crate::Size::new(self.width, self.height),
		)?)))
	}

	fn capture_image(&mut self, width: u16, height: u16) -> Result<(), Error> {
		if let Some(shm) = &mut self.shm {
			let reply = self
				.connection
				.shm_get_image(
					self.drawable,
					self.x,
					self.y,
					width,
					height,
					u32::MAX,
					u8::from(ImageFormat::Z_PIXMAP),
					shm.segment,
					0,
				)
				.map_err(source)?
				.reply()
				.map_err(source)?;
			if reply.size as usize != shm.len {
				return Err(codec("unexpected XShm image size"));
			}
			// The reply fences the server's write. No further SHM request is sent
			// until conversion finishes, and no slice escapes this capture read.
			let data = unsafe { std::slice::from_raw_parts(shm.ptr.as_ptr(), shm.len) };
			self.format.rgb(data, self.width, self.height, &mut self.rgb)
		} else {
			let image = self
				.connection
				.get_image(
					ImageFormat::Z_PIXMAP,
					self.drawable,
					self.x,
					self.y,
					width,
					height,
					u32::MAX,
				)
				.map_err(source)?
				.reply()
				.map_err(source)?;
			self.format.rgb(&image.data, self.width, self.height, &mut self.rgb)
		}
	}

	/// Whether the frame that is due now can be captured, and if not, whether the
	/// source is coming back. Checked before each frame, and again when a read
	/// fails, so a resize or a minimize landing between those two round trips is
	/// absorbed rather than reported as a dead source.
	fn settled(&mut self) -> Result<Settled, Error> {
		// An unmapped window (minimized, or on a workspace that isn't showing) has
		// nothing to copy and GetImage on it fails outright. Hold the capture
		// rather than ending it, so the broadcast resumes when the window returns.
		let hidden = self.hidden()?;
		let current = self.observe()?;
		if current == Observed::Destroyed {
			tracing::info!(source = %self.name, reason = "window destroyed", "X11 source changed; ending capture");
			return Ok(Settled::Changed);
		}
		if hidden {
			if !self.unmapped {
				self.unmapped = true;
				tracing::info!(source = %self.name, "X11 window unmapped; holding capture");
			}
			return Ok(Settled::Waiting);
		}
		if self.unmapped {
			self.unmapped = false;
			tracing::info!(source = %self.name, "X11 window mapped; resuming capture");
		}

		let settled = self.settle.observe(&current, Instant::now());
		if settled == Settled::Changed {
			let reason = match current {
				Observed::Size(width, height) => {
					format!("resized from {}x{} to {width}x{height}", self.width, self.height)
				}
				Observed::Monitor(_) => "monitor layout changed".to_string(),
				Observed::Destroyed => unreachable!("handled above"),
			};
			tracing::info!(source = %self.name, %reason, "X11 source changed; ending capture");
		}
		Ok(settled)
	}

	/// Whether a captured window is currently unmapped. A display target draws
	/// from the root window, which is always mapped. Query attributes rather
	/// than trusting MapNotify: an ancestor may become unmapped without a
	/// notification on the captured child. Its reply also fences queued events.
	fn hidden(&self) -> Result<bool, Error> {
		if self.display.is_some() {
			return Ok(false);
		}
		let attributes = self
			.connection
			.get_window_attributes(self.drawable)
			.map_err(source)?
			.reply()
			.map_err(source)?;
		Ok(attributes.map_state != MapState::VIEWABLE)
	}

	/// Drain every frame, but re-query monitors only after a layout event.
	fn observe(&mut self) -> Result<Observed, Error> {
		let mut layout_changed = false;
		while let Some(event) = self.connection.poll_for_event().map_err(source)? {
			layout_changed |= self.observed.event(&event, self.drawable);
		}
		if layout_changed && let Some(display) = &self.display {
			self.observed = Observed::Monitor(display.matches(&monitors(&self.connection, self.root)?));
		}
		Ok(self.observed)
	}

	fn origin(&self) -> Result<(i32, i32), Error> {
		if self.drawable == self.root {
			return Ok((self.x.into(), self.y.into()));
		}
		let translated = self
			.connection
			.translate_coordinates(self.drawable, self.root, self.x, self.y)
			.map_err(source)?
			.reply()
			.map_err(source)?;
		Ok((translated.dst_x.into(), translated.dst_y.into()))
	}
}

/// The server owns the fd-backed segment; dropping capture detaches it and
/// unmaps our view even when opening or conversion fails.
struct SharedMemory {
	connection: Arc<RustConnection>,
	segment: shm::Seg,
	ptr: NonNull<u8>,
	len: usize,
}

impl SharedMemory {
	fn open(connection: Arc<RustConnection>, len: usize) -> Result<Option<Self>, Error> {
		// DISPLAY's spelling is not a transport check (localhost may be SSH
		// forwarding). FD segments work only across a Unix-domain connection.
		let mut domain: libc::c_int = 0;
		let mut size = std::mem::size_of_val(&domain) as libc::socklen_t;
		// SAFETY: both output pointers name live, correctly sized integers.
		let status = unsafe {
			libc::getsockopt(
				connection.stream().as_raw_fd(),
				libc::SOL_SOCKET,
				libc::SO_DOMAIN,
				(&mut domain as *mut libc::c_int).cast(),
				&mut size,
			)
		};
		if status != 0 {
			return Err(codec(std::io::Error::last_os_error()));
		}
		if domain != libc::AF_UNIX
			|| connection
				.extension_information(shm::X11_EXTENSION_NAME)
				.map_err(source)?
				.is_none()
		{
			return Ok(None);
		}
		let version = connection
			.shm_query_version()
			.map_err(source)?
			.reply()
			.map_err(source)?;
		if (version.major_version, version.minor_version) < (1, 2) {
			return Ok(None);
		}
		let size = u32::try_from(len).map_err(|_| codec("XShm frame is too large"))?;
		let segment = connection.generate_id().map_err(source)?;
		let reply = connection
			.shm_create_segment(segment, size, false)
			.map_err(source)?
			.reply()
			.map_err(source)?;
		// SAFETY: the server returned an owned fd for this size. The mapping is
		// read only here; shm_get_image writes through the server's mapping.
		let mapped = unsafe {
			libc::mmap(
				std::ptr::null_mut(),
				len,
				libc::PROT_READ,
				libc::MAP_SHARED,
				reply.shm_fd.as_raw_fd(),
				0,
			)
		};
		if mapped == libc::MAP_FAILED {
			let error = std::io::Error::last_os_error();
			let _ = connection.shm_detach(segment);
			let _ = connection.flush();
			return Err(codec(error));
		}
		let Some(ptr) = NonNull::new(mapped.cast::<u8>()) else {
			// SAFETY: even address zero is a successful mapping, but Rust slices
			// cannot represent it. Release both sides before refusing capture.
			unsafe {
				libc::munmap(mapped, len);
			}
			let _ = connection.shm_detach(segment);
			let _ = connection.flush();
			return Err(codec("XShm mapped a null address"));
		};
		Ok(Some(Self {
			connection,
			segment,
			ptr,
			len,
		}))
	}
}

impl Drop for SharedMemory {
	fn drop(&mut self) {
		// All reads wait for their reply, so no server write is outstanding.
		let _ = self.connection.shm_detach(self.segment);
		let _ = self.connection.flush();
		// SAFETY: this object uniquely owns the mapping returned by mmap.
		unsafe {
			libc::munmap(self.ptr.as_ptr().cast(), self.len);
		}
	}
}

struct DisplayTarget {
	selector: Option<usize>,
	index: usize,
	monitor: Monitor,
}

impl DisplayTarget {
	fn matches(&self, monitors: &[Monitor]) -> bool {
		let Some(index) = select_monitor(monitors, self.selector) else {
			return false;
		};
		index == self.index
			&& monitors
				.get(index)
				.is_some_and(|monitor| self.monitor.same_region(monitor))
	}
}

#[derive(Clone)]
struct Monitor {
	x: i16,
	y: i16,
	width: u32,
	height: u32,
	name: String,
	primary: bool,
}

impl Monitor {
	fn same_region(&self, other: &Self) -> bool {
		(self.x, self.y, self.width, self.height, &self.name)
			== (other.x, other.y, other.width, other.height, &other.name)
	}
}

fn monitors(connection: &RustConnection, root: XWindow) -> Result<Vec<Monitor>, Error> {
	let reply = connection.randr_get_monitors(root, true).map_err(codec)?.reply();
	if let Ok(reply) = reply {
		let mut result = Vec::new();
		for monitor in reply.monitors {
			let name = connection
				.get_atom_name(monitor.name)
				.map_err(codec)?
				.reply()
				.map_err(codec)
				.map(|reply| String::from_utf8_lossy(&reply.name).into_owned())
				.unwrap_or_else(|_| format!("Monitor {}", result.len() + 1));
			result.push(Monitor {
				x: monitor.x,
				y: monitor.y,
				width: monitor.width.into(),
				height: monitor.height.into(),
				name,
				primary: monitor.primary,
			});
		}
		if !result.is_empty() {
			return Ok(result);
		}
	}

	let geometry = connection.get_geometry(root).map_err(codec)?.reply().map_err(codec)?;
	Ok(vec![Monitor {
		x: 0,
		y: 0,
		width: geometry.width.into(),
		height: geometry.height.into(),
		name: "X11 display".to_string(),
		primary: true,
	}])
}

fn select_monitor(monitors: &[Monitor], selector: Option<usize>) -> Option<usize> {
	selector
		.filter(|index| *index < monitors.len())
		.or_else(|| {
			selector
				.is_none()
				.then(|| monitors.iter().position(|monitor| monitor.primary))
				.flatten()
		})
		.or_else(|| selector.is_none().then_some(0).filter(|_| !monitors.is_empty()))
}

impl Observed {
	/// Update window state, or mark the monitor snapshot dirty. Destruction is
	/// terminal even if the server reuses the XID before the next capture read.
	fn event(&mut self, event: &Event, drawable: Drawable) -> bool {
		if *self == Self::Destroyed {
			return false;
		}
		match event {
			Event::DestroyNotify(event) if event.window == drawable => *self = Self::Destroyed,
			Event::ConfigureNotify(event) if event.window == drawable => {
				if matches!(self, Self::Monitor(_)) {
					return true;
				}
				*self = Self::Size(u32::from(event.width) & !1, u32::from(event.height) & !1);
			}
			Event::RandrScreenChangeNotify(_) | Event::RandrNotify(_) => {
				return matches!(self, Self::Monitor(_));
			}
			_ => {}
		}
		false
	}
}

fn watch_display(connection: &RustConnection, root: XWindow) -> Result<(), Error> {
	// Also watch the root for servers without RandR. Subscribe before reading
	// the initial geometry, so a change during open cannot go unnoticed.
	connection
		.change_window_attributes(
			root,
			&ChangeWindowAttributesAux::new().event_mask(EventMask::STRUCTURE_NOTIFY),
		)
		.map_err(source)?
		.check()
		.map_err(source)?;
	if connection
		.extension_information(randr::X11_EXTENSION_NAME)
		.map_err(source)?
		.is_some()
	{
		let version = connection
			.randr_query_version(1, 5)
			.map_err(source)?
			.reply()
			.map_err(source)?;
		let mut mask = randr::NotifyMask::SCREEN_CHANGE;
		if (version.major_version, version.minor_version) >= (1, 2) {
			mask |= randr::NotifyMask::CRTC_CHANGE | randr::NotifyMask::OUTPUT_CHANGE;
		}
		if (version.major_version, version.minor_version) >= (1, 4) {
			mask |= randr::NotifyMask::RESOURCE_CHANGE;
		}
		connection
			.randr_select_input(root, mask)
			.map_err(source)?
			.check()
			.map_err(source)?;
	}
	Ok(())
}

fn client_windows(connection: &RustConnection, root: XWindow) -> Result<Vec<XWindow>, Error> {
	let clients_atom = intern(connection, b"_NET_CLIENT_LIST")?;
	let clients = connection
		.get_property(false, root, clients_atom, AtomEnum::WINDOW, 0, u32::MAX)
		.map_err(codec)?
		.reply()
		.map_err(codec)?
		.value32()
		.map(|windows| windows.collect::<Vec<_>>())
		.unwrap_or_default();
	if !clients.is_empty() {
		return Ok(clients);
	}

	let mut windows = Vec::new();
	let mut pending = connection
		.query_tree(root)
		.map_err(codec)?
		.reply()
		.map_err(codec)?
		.children;
	while let Some(window) = pending.pop() {
		windows.push(window);
		if let Ok(cookie) = connection.query_tree(window)
			&& let Ok(tree) = cookie.reply()
		{
			pending.extend(tree.children);
		}
	}
	Ok(windows)
}

fn blend_cursor(rgb: &mut [u8], width: u32, height: u32, origin_x: i32, origin_y: i32, cursor: &GetCursorImageReply) {
	let cursor_x = i32::from(cursor.x) - i32::from(cursor.xhot);
	let cursor_y = i32::from(cursor.y) - i32::from(cursor.yhot);
	for (index, pixel) in cursor.cursor_image.iter().copied().enumerate() {
		let x = cursor_x + (index % usize::from(cursor.width)) as i32 - origin_x;
		let y = cursor_y + (index / usize::from(cursor.width)) as i32 - origin_y;
		if x < 0 || y < 0 || x >= width as i32 || y >= height as i32 {
			continue;
		}
		let alpha = (pixel >> 24) as u8;
		if alpha == 0 {
			continue;
		}
		let offset = (y as usize * width as usize + x as usize) * 3;
		let source = [
			((pixel >> 16) & 0xff) as u8,
			((pixel >> 8) & 0xff) as u8,
			(pixel & 0xff) as u8,
		];
		for (target, source) in rgb[offset..offset + 3].iter_mut().zip(source) {
			*target = source.saturating_add(blend_background(*target, alpha));
		}
	}
}

fn blend_background(value: u8, alpha: u8) -> u8 {
	((u16::from(value) * u16::from(255 - alpha) + 127) / 255) as u8
}

struct PixelFormat {
	byte_order: ImageOrder,
	bits_per_pixel: u8,
	scanline_pad: u8,
	red_mask: u32,
	green_mask: u32,
	blue_mask: u32,
}

impl PixelFormat {
	fn new(setup: &x11rb::protocol::xproto::Setup, depth: u8, visual_id: u32) -> Result<Self, Error> {
		let visual = setup
			.roots
			.iter()
			.flat_map(|screen| &screen.allowed_depths)
			.flat_map(|depth| &depth.visuals)
			.find(|visual| visual.visual_id == visual_id)
			.ok_or_else(|| Error::Codec(anyhow::anyhow!("X11 root visual is missing")))?;
		if visual.class != VisualClass::TRUE_COLOR {
			return Err(Error::Codec(anyhow::anyhow!(
				"unsupported X11 visual class: {:?}",
				visual.class
			)));
		}
		let format = setup
			.pixmap_formats
			.iter()
			.find(|format| format.depth == depth)
			.ok_or_else(|| Error::Codec(anyhow::anyhow!("X11 pixel format for depth {depth} is missing")))?;
		Ok(Self {
			byte_order: setup.image_byte_order,
			bits_per_pixel: format.bits_per_pixel,
			scanline_pad: format.scanline_pad,
			red_mask: visual.red_mask,
			green_mask: visual.green_mask,
			blue_mask: visual.blue_mask,
		})
	}

	fn stride(&self, width: u32) -> Result<usize, Error> {
		if !matches!(self.bits_per_pixel, 16 | 24 | 32) || !matches!(self.scanline_pad, 8 | 16 | 32) {
			return Err(codec("unsupported X11 pixel layout"));
		}
		let row_bits = (width as usize)
			.checked_mul(usize::from(self.bits_per_pixel))
			.ok_or_else(|| codec("X11 row size overflow"))?;
		let pad = usize::from(self.scanline_pad);
		row_bits
			.div_ceil(pad)
			.checked_mul(pad / 8)
			.ok_or_else(|| codec("X11 row size overflow"))
	}

	fn required_len(&self, width: u32, height: u32) -> Result<usize, Error> {
		self.stride(width)?
			.checked_mul(height as usize)
			.filter(|len| *len > 0)
			.ok_or_else(|| codec("invalid X11 frame size"))
	}

	fn rgb(&self, data: &[u8], width: u32, height: u32, rgb: &mut Vec<u8>) -> Result<(), Error> {
		let pixel_bytes = usize::from(self.bits_per_pixel / 8);
		let stride = self.stride(width)?;
		let required = self.required_len(width, height)?;
		if data.len() < required {
			return Err(Error::Codec(anyhow::anyhow!(
				"short X11 frame: got {} bytes, need {required}",
				data.len()
			)));
		}

		let len = (width as usize)
			.checked_mul(height as usize)
			.and_then(|len| len.checked_mul(3))
			.ok_or_else(|| codec("X11 RGB size overflow"))?;
		rgb.clear();
		rgb.reserve(len);
		for row in data[..required].chunks_exact(stride) {
			for bytes in row[..width as usize * pixel_bytes].chunks_exact(pixel_bytes) {
				let mut pixel = 0u32;
				match self.byte_order {
					ImageOrder::LSB_FIRST => {
						for (shift, byte) in bytes.iter().enumerate() {
							pixel |= u32::from(*byte) << (shift * 8);
						}
					}
					_ => {
						for byte in bytes {
							pixel = (pixel << 8) | u32::from(*byte);
						}
					}
				}
				rgb.push(component(pixel, self.red_mask));
				rgb.push(component(pixel, self.green_mask));
				rgb.push(component(pixel, self.blue_mask));
			}
		}
		Ok(())
	}
}

fn component(pixel: u32, mask: u32) -> u8 {
	if mask == 0 {
		return 0;
	}
	let shift = mask.trailing_zeros();
	let maximum = mask >> shift;
	let value = (pixel & mask) >> shift;
	((u64::from(value) * 255 + u64::from(maximum) / 2) / u64::from(maximum)) as u8
}

fn connect() -> Result<(RustConnection, usize), Error> {
	x11rb::connect(None).map_err(|error| Error::SourceUnavailable(format!("X11 display: {error}")))
}

fn parse_selector(selector: &str, prefix: &str) -> Result<usize, Error> {
	selector
		.strip_prefix(prefix)
		.unwrap_or(selector)
		.parse()
		.map_err(|_| Error::SourceUnavailable(format!("invalid X11 selector {selector:?}")))
}

fn intern(connection: &RustConnection, name: &[u8]) -> Result<u32, Error> {
	Ok(connection
		.intern_atom(false, name)
		.map_err(codec)?
		.reply()
		.map_err(codec)?
		.atom)
}

fn property(connection: &RustConnection, window: XWindow, property: u32, kind: u32) -> Option<String> {
	connection
		.get_property(false, window, property, kind, 0, 4096)
		.ok()?
		.reply()
		.ok()
		.map(|reply| String::from_utf8_lossy(&reply.value).into_owned())
}

/// `WM_CLASS` is `instance\0class\0`; the class is the application name, which
/// is what the other platforms report.
fn wm_class(value: &str) -> String {
	let mut parts = value.split('\0').filter(|part| !part.is_empty());
	let instance = parts.next().unwrap_or_default();
	parts.next().unwrap_or(instance).to_string()
}

fn codec(error: impl std::fmt::Display) -> Error {
	Error::Codec(anyhow::anyhow!("X11: {error}"))
}

fn source(error: impl std::fmt::Display) -> Error {
	Error::SourceUnavailable(format!("X11 source: {error}"))
}

#[cfg(test)]
mod tests {
	use x11rb::protocol::xproto::DestroyNotifyEvent;

	use super::*;

	fn monitor(primary: bool) -> Monitor {
		Monitor {
			x: 0,
			y: 0,
			width: 1920,
			height: 1080,
			name: String::new(),
			primary,
		}
	}

	#[test]
	fn default_display_uses_primary_monitor() {
		let monitors = [monitor(false), monitor(true), monitor(false)];
		assert_eq!(select_monitor(&monitors, None), Some(1));
		assert_eq!(select_monitor(&monitors, Some(0)), Some(0));
	}

	#[test]
	fn display_target_detects_geometry_and_selection_changes() {
		let initial = [monitor(true), monitor(false)];
		let target = DisplayTarget {
			selector: None,
			index: 0,
			monitor: initial[0].clone(),
		};
		assert!(target.matches(&initial));

		let mut resized = initial.clone();
		resized[0].width = 1280;
		assert!(!target.matches(&resized));

		let mut primary_changed = initial;
		primary_changed[0].primary = false;
		primary_changed[1].primary = true;
		assert!(!target.matches(&primary_changed));
	}

	#[test]
	fn a_destroyed_window_ends_capture_even_when_its_id_lives_on() {
		let destroy = |window| {
			Event::DestroyNotify(DestroyNotifyEvent {
				event: window,
				window,
				..Default::default()
			})
		};
		let mut observed = Observed::Size(640, 480);
		observed.event(&destroy(0x40_0002), 0x40_0001);
		assert_eq!(observed, Observed::Size(640, 480));
		observed.event(&destroy(0x40_0001), 0x40_0001);
		assert_eq!(observed, Observed::Destroyed);
		observed.event(&Event::Unknown(Vec::new()), 0x40_0001);
		assert_eq!(observed, Observed::Destroyed);
	}

	#[test]
	fn xwayland_is_available_without_becoming_the_default() {
		assert!(is_available(true, true));
		assert!(!selects_x11(None, true));
		assert!(selects_x11(Some("x11:0"), true));
	}

	#[test]
	fn cursor_is_cropped_and_alpha_blended() {
		let mut rgb = vec![100; 2 * 2 * 3];
		let cursor = GetCursorImageReply {
			x: 1,
			y: 1,
			width: 3,
			height: 1,
			xhot: 1,
			yhot: 0,
			cursor_image: vec![0, 0x8080_0000, 0xff00_00ff],
			..Default::default()
		};
		blend_cursor(&mut rgb, 2, 2, 1, 0, &cursor);
		assert_eq!(&rgb[..3], &[100; 3]);
		assert_eq!(&rgb[6..9], &[178, 50, 50]);
		assert_eq!(&rgb[9..12], &[0, 0, 255]);
	}

	#[test]
	fn wm_class_reports_the_application_class() {
		assert_eq!(wm_class("navigator\0Firefox\0"), "Firefox");
		assert_eq!(wm_class("xterm\0"), "xterm");
		assert_eq!(wm_class(""), "");
	}

	#[test]
	fn rgb_decodes_sixteen_bit_true_color() {
		let format = PixelFormat {
			byte_order: ImageOrder::LSB_FIRST,
			bits_per_pixel: 16,
			scanline_pad: 32,
			red_mask: 0xf800,
			green_mask: 0x07e0,
			blue_mask: 0x001f,
		};
		let mut rgb = Vec::new();
		format.rgb(&[0x00, 0xf8, 0xe0, 0x07], 2, 1, &mut rgb).unwrap();
		assert_eq!(rgb, [255, 0, 0, 0, 255, 0]);
	}
	fn configure(window: XWindow, width: u16, height: u16) -> Event {
		Event::ConfigureNotify(x11rb::protocol::xproto::ConfigureNotifyEvent {
			event: window,
			window,
			width,
			height,
			..Default::default()
		})
	}

	#[test]
	fn window_events_update_geometry_without_reviving_a_destroyed_source() {
		let mut observed = Observed::Size(640, 480);
		assert!(!observed.event(&configure(7, 801, 603), 7));
		assert_eq!(observed, Observed::Size(800, 602));
		observed.event(&configure(8, 320, 240), 7);
		assert_eq!(observed, Observed::Size(800, 602));
		observed.event(
			&Event::DestroyNotify(DestroyNotifyEvent {
				window: 7,
				..Default::default()
			}),
			7,
		);
		observed.event(&configure(7, 640, 480), 7);
		assert_eq!(observed, Observed::Destroyed);
	}

	#[test]
	fn a_window_resize_settles_without_another_event() {
		let now = Instant::now();
		let mut observed = Observed::Size(640, 480);
		let mut settle = Settle::new(observed);
		observed.event(&configure(7, 800, 600), 7);
		assert_eq!(settle.observe(&observed, now), Settled::Waiting);
		assert_eq!(
			settle.observe(&observed, now + super::super::settle::HOLD),
			Settled::Changed
		);
		observed.event(&configure(7, 640, 480), 7);
		assert_eq!(settle.observe(&observed, now), Settled::Open);
	}

	#[test]
	fn only_layout_events_invalidate_the_monitor_snapshot() {
		let screen = Event::RandrScreenChangeNotify(Default::default());
		let resource = Event::RandrNotify(randr::NotifyEvent {
			response_type: 0,
			sequence: 0,
			sub_code: randr::Notify::RESOURCE_CHANGE,
			u: randr::ResourceChange::default().into(),
		});
		let mut display = Observed::Monitor(true);
		assert!(!display.event(&Event::Unknown(Vec::new()), 1));
		assert!(!display.event(&configure(2, 1280, 720), 1));
		assert!(display.event(&configure(1, 1280, 720), 1));
		assert!(display.event(&screen, 1));
		assert!(display.event(&resource, 1));
		assert_eq!(display, Observed::Monitor(true));
		let mut window = Observed::Size(640, 480);
		assert!(!window.event(&screen, 2));
		assert!(!window.event(&resource, 2));
	}

	fn bgra() -> PixelFormat {
		PixelFormat {
			byte_order: ImageOrder::LSB_FIRST,
			bits_per_pixel: 32,
			scanline_pad: 32,
			red_mask: 0xff0000,
			green_mask: 0xff00,
			blue_mask: 0xff,
		}
	}

	#[test]
	fn rgb_reuses_storage_and_overwrites_the_previous_frame() {
		let format = bgra();
		let mut rgb = Vec::new();
		format.rgb(&[0, 0, 255, 0, 0, 255, 0, 0], 2, 1, &mut rgb).unwrap();
		assert_eq!(rgb, [255, 0, 0, 0, 255, 0]);
		let ptr = rgb.as_ptr();
		let capacity = rgb.capacity();
		format.rgb(&[255, 0, 0, 0, 255, 255, 255, 0], 2, 1, &mut rgb).unwrap();
		assert_eq!(rgb, [0, 0, 255, 255, 255, 255]);
		assert_eq!(rgb.as_ptr(), ptr);
		assert_eq!(rgb.capacity(), capacity);
	}

	#[test]
	fn rgb_handles_row_padding_and_big_endian_pixels() {
		let mut format = bgra();
		format.byte_order = ImageOrder::MSB_FIRST;
		format.bits_per_pixel = 24;
		let mut rgb = Vec::new();
		assert_eq!(format.required_len(1, 2).unwrap(), 8);
		format.rgb(&[255, 0, 0, 99, 0, 255, 0, 99], 1, 2, &mut rgb).unwrap();
		assert_eq!(rgb, [255, 0, 0, 0, 255, 0]);
		assert!(format.rgb(&[0; 7], 1, 2, &mut rgb).is_err());
		assert!(format.required_len(0, 1).is_err());
		format.scanline_pad = 0;
		assert!(format.required_len(1, 1).is_err());
	}

	// The display must be a dedicated local server with a >=1920x1080 screen.
	// Xvfb exercises the actual protocol and shared segment, not GPU capture.
	#[test]
	#[ignore = "local X11 capture benchmark; run with just rs x11-bench (also run nightly)"]
	fn x11_capture_workload() {
		use x11rb::protocol::xproto::{CreateGCAux, CreateWindowAux, Rectangle, WindowClass};

		const WARMUP: usize = 8;
		const SAMPLES: usize = 41;
		let (drawing, screen) = connect().expect("a dedicated local X11 display is required");
		let screen = &drawing.setup().roots[screen];
		assert!(
			screen.width_in_pixels >= 1920 && screen.height_in_pixels >= 1080,
			"the X11 benchmark needs a screen of at least 1920x1080"
		);
		let config = Config {
			cursor: false,
			..Default::default()
		};
		for (width, height) in [(640u16, 480u16), (1280, 720), (1920, 1080)] {
			let window = drawing.generate_id().unwrap();
			drawing
				.create_window(
					screen.root_depth,
					window,
					screen.root,
					0,
					0,
					width,
					height,
					0,
					WindowClass::INPUT_OUTPUT,
					screen.root_visual,
					&CreateWindowAux::new().override_redirect(1).background_pixel(0),
				)
				.unwrap()
				.check()
				.unwrap();
			drawing.map_window(window).unwrap().check().unwrap();
			let mut shared = Capture::open(&config, Target::Window(window as usize)).unwrap();
			assert!(
				shared.shm.is_some(),
				"the benchmark requires active MIT-SHM, not fallback"
			);
			let mut socket = Capture::open(&config, Target::Window(window as usize)).unwrap();
			// Select the existing production fallback on the same server and drawable.
			drop(socket.shm.take());
			let black = drawing.generate_id().unwrap();
			let white = drawing.generate_id().unwrap();
			drawing
				.create_gc(black, window, &CreateGCAux::new().foreground(0))
				.unwrap()
				.check()
				.unwrap();
			drawing
				.create_gc(
					white,
					window,
					&CreateGCAux::new()
						.foreground(shared.format.red_mask | shared.format.green_mask | shared.format.blue_mask),
				)
				.unwrap()
				.check()
				.unwrap();
			let expected: Vec<_> = [false, true]
				.into_iter()
				.map(|inverted| {
					let rgb: Vec<_> = (0..height)
						.flat_map(|_| (0..width).flat_map(|x| [if (x < width / 2) ^ inverted { 0 } else { 255 }; 3]))
						.collect();
					let frame = I420::from_rgb(&rgb, crate::Size::new(width.into(), height.into())).unwrap();
					(rgb, frame)
				})
				.collect();
			for mode in ["static", "dynamic"] {
				let mut shared_times = Vec::with_capacity(SAMPLES);
				let mut socket_times = Vec::with_capacity(SAMPLES);
				for sample in 0..WARMUP + SAMPLES {
					let inverted = mode == "dynamic" && !sample.is_multiple_of(2);
					// Drawing and its server round-trip are outside both measured reads.
					// Static samples leave the drawable untouched after the first draw.
					if sample == 0 || mode == "dynamic" {
						for (x, gc) in [
							(0, if inverted { white } else { black }),
							(width / 2, if inverted { black } else { white }),
						] {
							drawing
								.poly_fill_rectangle(
									window,
									gc,
									&[Rectangle {
										x: x as i16,
										y: 0,
										width: width / 2,
										height,
									}],
								)
								.unwrap();
						}
						drawing.get_input_focus().unwrap().reply().unwrap();
					}
					let (shared_read, socket_read) = if sample.is_multiple_of(2) {
						(measure_capture(&mut shared), measure_capture(&mut socket))
					} else {
						let socket_read = measure_capture(&mut socket);
						(measure_capture(&mut shared), socket_read)
					};
					let (rgb, frame) = &expected[usize::from(inverted)];
					assert!(&shared.rgb == rgb, "SHM pixels differ from the drawn pattern");
					assert!(&socket.rgb == rgb, "GetImage pixels differ from the drawn pattern");
					assert_eq!(shared_read.1.size(), frame.size());
					assert_eq!(socket_read.1.size(), frame.size());
					assert!(shared_read.1.data() == frame.data(), "SHM output differs");
					assert!(socket_read.1.data() == frame.data(), "GetImage output differs");
					if sample >= WARMUP {
						shared_times.push(shared_read.0.as_nanos());
						socket_times.push(socket_read.0.as_nanos());
					}
				}
				shared_times.sort_unstable();
				socket_times.sort_unstable();
				let shm = shared_times[SAMPLES / 2];
				let get_image = socket_times[SAMPLES / 2];
				// Keep the paired baseline instead of asserting a machine-specific time.
				println!(
					"X11_CAPTURE_BENCH {{\"width\":{width},\"height\":{height},\"mode\":\"{mode}\",\"samples\":{SAMPLES},\"shm_median_ns\":{shm},\"get_image_median_ns\":{get_image},\"shm_over_get_image\":{:.6}}}",
					shm as f64 / get_image as f64
				);
			}
			drop(shared);
			drop(socket);
			drawing.free_gc(black).unwrap().check().unwrap();
			drawing.free_gc(white).unwrap().check().unwrap();
			drawing.destroy_window(window).unwrap().check().unwrap();
		}
	}

	fn measure_capture(capture: &mut Capture) -> (Duration, I420) {
		// Bypass only frame-rate sleeping; geometry checks, transport, buffer reuse
		// and RGB-to-I420 conversion remain on the production Capture::read path.
		capture.next = Instant::now();
		let start = Instant::now();
		let result = capture.read().expect("X11 capture read failed");
		let elapsed = start.elapsed();
		let pump::Read::Frame(Surface::I420(frame)) = result else {
			panic!("the unchanged benchmark window must yield an I420 frame");
		};
		(elapsed, frame)
	}

	#[test]
	#[ignore = "conversion benchmark; run with just rs x11-rgb-bench"]
	fn x11_rgb_workload() {
		use std::hint::black_box;
		let format = bgra();
		for (width, height) in [(640, 480), (1280, 720), (1920, 1080)] {
			let data = [0x12, 0x34, 0x56, 0].repeat(width * height);
			let mut rgb = Vec::new();
			format.rgb(&data, width as u32, height as u32, &mut rgb).unwrap();
			let expected = rgb.clone();
			for reuse in [false, true] {
				let start = Instant::now();
				for _ in 0..32 {
					if !reuse {
						rgb = Vec::new();
					}
					format
						.rgb(black_box(&data), width as u32, height as u32, &mut rgb)
						.unwrap();
					black_box(&rgb);
				}
				let elapsed = start.elapsed();
				assert_eq!(rgb, expected);
				eprintln!("X11 RGB {width}x{height}, reuse={reuse}: 32 frames in {elapsed:?}");
			}
		}
	}
}
