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
use x11rb::protocol::xproto::{
	AtomEnum, ConnectionExt as _, Drawable, ImageFormat, ImageOrder, MapState, Window as XWindow,
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
	selector.is_some_and(|selector| selector.starts_with("x11:")) || std::env::var_os("WAYLAND_DISPLAY").is_none()
}

pub(super) fn displays() -> Result<Vec<Display>, Error> {
	if !selected(None) {
		return Err(Error::Unsupported(
			"listing displays on Wayland; the desktop portal owns selection".to_string(),
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
	if !selected(None) {
		return Err(Error::Unsupported(
			"listing windows on Wayland; the desktop portal does not expose them".to_string(),
		));
	}
	let (connection, screen) = connect()?;
	let root = connection.setup().roots[screen].root;
	let tree = connection.query_tree(root).map_err(codec)?.reply().map_err(codec)?;
	let name_atom = intern(&connection, b"_NET_WM_NAME")?;
	let utf8_atom = intern(&connection, b"UTF8_STRING")?;

	let mut result = Vec::new();
	for id in tree.children {
		let attributes = match connection.get_window_attributes(id).map_err(codec)?.reply() {
			Ok(attributes) if attributes.map_state == MapState::VIEWABLE => attributes,
			_ => continue,
		};
		let _ = attributes;
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
			.map(|value| value.replace('\0', " ").trim().to_string())
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
	open(config, Target::Display(parse_selector(selector, "x11:")?)).await
}

pub(super) async fn open_window(config: &Config, selector: &str) -> Result<Stream, Error> {
	if !selected(Some(selector)) {
		return Err(Error::Unsupported("window capture on Wayland".to_string()));
	}
	let id = parse_selector(Some(selector), "x11:")?;
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
				device: capture.name.clone(),
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
		geometry.device,
		None,
		Box::new(guard),
	))
}

#[derive(Clone, Copy)]
enum Target {
	Display(usize),
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
}

impl Capture {
	fn open(config: &Config, target: Target) -> Result<Self, Error> {
		let (connection, screen_index) = connect()?;
		let screen = &connection.setup().roots[screen_index];
		let (drawable, x, y, width, height, name) = match target {
			Target::Display(index) => {
				let monitor = monitors(&connection, screen.root)?
					.into_iter()
					.nth(index)
					.ok_or_else(|| Error::SourceUnavailable(format!("no X11 display at index {index}")))?;
				(
					screen.root,
					monitor.x,
					monitor.y,
					monitor.width,
					monitor.height,
					format!("x11:{index}"),
				)
			}
			Target::Window(id) => {
				let drawable =
					u32::try_from(id).map_err(|_| Error::SourceUnavailable(format!("invalid X11 window id {id}")))?;
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
					format!("x11:{id}"),
				)
			}
		};
		let width = width & !1;
		let height = height & !1;
		if width == 0 || height == 0 {
			return Err(Error::SourceUnavailable(format!("{name} has no capturable area")));
		}
		let format = PixelFormat::new(connection.setup(), screen.root_depth, screen.root_visual)?;
		let framerate = config.framerate.unwrap_or(DEFAULT_FRAMERATE).max(1);
		let interval = Duration::from_micros(1_000_000 / u64::from(framerate));
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
		})
	}

	fn read(&mut self) -> Result<Option<Surface>, Error> {
		let now = Instant::now();
		if self.next > now {
			std::thread::sleep(self.next - now);
		}
		self.next = Instant::now() + self.interval;

		let width = u16::try_from(self.width).map_err(|_| Error::Codec(anyhow::anyhow!("X11 width is too large")))?;
		let height =
			u16::try_from(self.height).map_err(|_| Error::Codec(anyhow::anyhow!("X11 height is too large")))?;
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
		let rgb = self.format.rgb(&image.data, self.width, self.height)?;
		Ok(Some(Surface::I420(I420::from_rgb(&rgb, self.width, self.height)?)))
	}
}

struct Monitor {
	x: i16,
	y: i16,
	width: u32,
	height: u32,
	name: String,
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
	}])
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
		if !(3..=4).contains(&pixel_bytes) {
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

fn parse_selector(selector: Option<&str>, prefix: &str) -> Result<usize, Error> {
	selector
		.unwrap_or("0")
		.strip_prefix(prefix)
		.unwrap_or(selector.unwrap_or("0"))
		.parse()
		.map_err(|_| Error::SourceUnavailable(format!("invalid X11 selector {:?}", selector.unwrap_or("0"))))
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

fn codec(error: impl std::fmt::Display) -> Error {
	Error::Codec(anyhow::anyhow!("X11: {error}"))
}

fn source(error: impl std::fmt::Display) -> Error {
	Error::SourceUnavailable(format!("X11 source: {error}"))
}
