//! Native X11 display and window capture.
//!
//! Wayland capture goes through the desktop portal because direct global
//! capture is intentionally forbidden there. An X11 session does expose stable
//! monitor and window ids, so this backend enumerates them with RandR and pulls
//! pixels with `GetImage`. It is also the fallback when the `pipewire` feature
//! is disabled on an X11 desktop.

use std::time::{Duration, Instant};

use x11rb::connection::Connection;
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::xfixes::{ConnectionExt as _, GetCursorImageReply};
use x11rb::protocol::xproto::{
	AtomEnum, ConnectionExt as _, Drawable, ImageFormat, ImageOrder, MapState, VisualClass, Window as XWindow,
};
use x11rb::rust_connection::RustConnection;

use super::channel::FrameChannel;
use super::pump::{self, Geometry};
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
	connection: RustConnection,
	drawable: Drawable,
	x: i16,
	y: i16,
	width: u32,
	height: u32,
	format: PixelFormat,
	framerate: u32,
	interval: Duration,
	next: Instant,
	name: String,
	root: XWindow,
	cursor: bool,
	display: Option<DisplayTarget>,
}

impl Capture {
	fn open(config: &Config, target: Target) -> Result<Self, Error> {
		let (connection, screen_index) = connect()?;
		let (root, root_depth, root_visual) = {
			let screen = &connection.setup().roots[screen_index];
			(screen.root, screen.root_depth, screen.root_visual)
		};
		let (drawable, x, y, width, height, depth, visual, name, display) = match target {
			Target::Display(selector) => {
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
		let framerate = config.framerate.unwrap_or(DEFAULT_FRAMERATE).max(1);
		let interval = Duration::from_micros(1_000_000 / u64::from(framerate));
		let cursor = config.cursor
			&& connection
				.xfixes_query_version(5, 0)
				.ok()
				.and_then(|cookie| cookie.reply().ok())
				.is_some();
		Ok(Self {
			connection,
			drawable,
			x,
			y,
			width,
			height,
			format,
			framerate,
			interval,
			next: Instant::now(),
			name,
			root,
			cursor,
			display,
		})
	}

	fn read(&mut self) -> Result<Option<Surface>, Error> {
		let now = Instant::now();
		if self.next > now {
			std::thread::sleep(self.next - now);
		}
		self.next = Instant::now() + self.interval;
		// The encoder's geometry is fixed at open, so a change ends the stream and
		// the caller reopens against the new size.
		if let Some(reason) = self.changed()? {
			tracing::info!(source = %self.name, %reason, "X11 source changed; ending capture");
			return Ok(None);
		}

		let width = u16::try_from(self.width).map_err(|_| Error::Codec(anyhow::anyhow!("X11 width is too large")))?;
		let height =
			u16::try_from(self.height).map_err(|_| Error::Codec(anyhow::anyhow!("X11 height is too large")))?;
		let image = match self
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
		{
			Ok(image) => image,
			// The check above and this request are separate round trips, so an
			// interactive resize lands between them often enough to matter. Only a
			// geometry change that we can still confirm is recoverable: anything
			// else stays terminal, so a persistent failure surfaces instead of
			// spinning the caller's reopen loop.
			Err(error) => {
				return match self.changed() {
					Ok(Some(reason)) => {
						tracing::info!(source = %self.name, %reason, "X11 source changed mid-frame; ending capture");
						Ok(None)
					}
					_ => Err(Error::SourceUnavailable(format!("X11 source: {error}"))),
				};
			}
		};
		let mut rgb = self.format.rgb(&image.data, self.width, self.height)?;
		if self.cursor
			&& let Some(cursor) = self
				.connection
				.xfixes_get_cursor_image()
				.ok()
				.and_then(|cookie| cookie.reply().ok())
		{
			let (x, y) = self.origin()?;
			blend_cursor(&mut rgb, self.width, self.height, x, y, &cursor);
		}
		Ok(Some(Surface::I420(I420::from_rgb(&rgb, self.width, self.height)?)))
	}

	/// How the source's geometry moved out from under the open stream, if it
	/// did. Checked before each frame and again when a read fails.
	fn changed(&self) -> Result<Option<String>, Error> {
		match &self.display {
			Some(display) => {
				let monitors = monitors(&self.connection, self.root)?;
				Ok((!display.matches(&monitors)).then(|| "monitor layout changed".to_string()))
			}
			None => {
				let geometry = self
					.connection
					.get_geometry(self.drawable)
					.map_err(source)?
					.reply()
					.map_err(source)?;
				let width = u32::from(geometry.width) & !1;
				let height = u32::from(geometry.height) & !1;
				Ok(((width, height) != (self.width, self.height))
					.then(|| format!("resized from {}x{} to {width}x{height}", self.width, self.height)))
			}
		}
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

	fn rgb(&self, data: &[u8], width: u32, height: u32) -> Result<Vec<u8>, Error> {
		let pixel_bytes = usize::from(self.bits_per_pixel.div_ceil(8));
		if !(2..=4).contains(&pixel_bytes) {
			return Err(Error::Codec(anyhow::anyhow!(
				"unsupported X11 pixel depth: {} bits per pixel",
				self.bits_per_pixel
			)));
		}
		let row_bits = usize::try_from(width)
			.ok()
			.and_then(|width| width.checked_mul(usize::from(self.bits_per_pixel)))
			.ok_or_else(|| Error::Codec(anyhow::anyhow!("X11 row size overflow")))?;
		let pad = usize::from(self.scanline_pad);
		let stride = row_bits.div_ceil(pad) * pad / 8;
		let required = stride
			.checked_mul(height as usize)
			.ok_or_else(|| Error::Codec(anyhow::anyhow!("X11 frame size overflow")))?;
		if data.len() < required {
			return Err(Error::Codec(anyhow::anyhow!(
				"short X11 frame: got {} bytes, need {required}",
				data.len()
			)));
		}

		let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
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
		Ok(rgb)
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
		let rgb = format.rgb(&[0x00, 0xf8, 0xe0, 0x07], 2, 1).unwrap();
		assert_eq!(rgb, [255, 0, 0, 0, 255, 0]);
	}
}
