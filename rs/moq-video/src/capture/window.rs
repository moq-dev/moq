//! Native Windows single-window capture.
//!
//! Desktop Duplication is monitor-scoped, so selected windows use GDI. The
//! backend enumerates visible top-level windows, copies the selected window DC
//! at the requested frame rate, and converts BGRA to I420. The shared
//! latest-frame channel keeps the GDI loop independent from encoder latency.

use std::ffi::c_void;
use std::mem::size_of;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{E_ACCESSDENIED, HWND, LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
	BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleBitmap, CreateCompatibleDC,
	DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDIBits, GetWindowDC, HBITMAP, HDC, HGDIOBJ, ReleaseDC, SRCCOPY,
	SelectObject,
};
use windows::Win32::Storage::Xps::{PRINT_WINDOW_FLAGS, PrintWindow};
use windows::Win32::UI::WindowsAndMessaging::{
	EnumWindows, GetClassNameW, GetWindowRect, GetWindowTextLengthW, GetWindowTextW, IsWindow, IsWindowVisible,
};
use windows::core::BOOL;

use super::channel::FrameChannel;
use super::pump::{self, Geometry};
use super::{Config, Stream, Window};
use crate::Error;
use crate::frame::{I420, Surface};

const DEFAULT_FRAMERATE: u32 = 30;

/// List visible, titled top-level windows by native HWND.
pub(super) fn windows() -> Result<Vec<Window>, Error> {
	let mut handles = Vec::<HWND>::new();
	unsafe {
		EnumWindows(
			Some(collect_window),
			LPARAM((&mut handles as *mut Vec<HWND>).cast::<c_void>() as isize),
		)
		.map_err(|error| Error::Codec(anyhow::anyhow!("enumerate Windows windows: {error}")))?;
	}

	let mut result = Vec::new();
	for handle in handles {
		let mut rect = RECT::default();
		if unsafe { GetWindowRect(handle, &mut rect) }.is_err() {
			continue;
		}
		let width = (rect.right - rect.left).max(0) as u32;
		let height = (rect.bottom - rect.top).max(0) as u32;
		if width < 2 || height < 2 {
			continue;
		}
		let title = title(handle);
		if title.is_empty() {
			continue;
		}
		result.push(Window {
			id: format!("window:{}", handle.0 as usize),
			title,
			app: class_name(handle),
			width,
			height,
		});
	}
	Ok(result)
}

unsafe extern "system" fn collect_window(handle: HWND, data: LPARAM) -> BOOL {
	if unsafe { IsWindowVisible(handle) }.as_bool() && unsafe { GetWindowTextLengthW(handle) } > 0 {
		let handles = unsafe { &mut *(data.0 as *mut Vec<HWND>) };
		handles.push(handle);
	}
	true.into()
}

pub(super) async fn open(config: &Config, selector: &str) -> Result<Stream, Error> {
	let handle = parse(selector)?;
	let handle = handle.0 as usize;
	let config = config.clone();
	let chan = FrameChannel::new();
	let (geometry, guard) = pump::spawn(
		chan.clone(),
		move || {
			let handle = HWND(handle as *mut c_void);
			let capture = Capture::open(&config, handle)?;
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

struct Capture {
	handle: HWND,
	width: u32,
	height: u32,
	framerate: u32,
	interval: Duration,
	next: Instant,
	name: String,
}

// HWND is only dereferenced through user32 on the pump thread.
unsafe impl Send for Capture {}

impl Capture {
	fn open(config: &Config, handle: HWND) -> Result<Self, Error> {
		if !unsafe { IsWindow(Some(handle)) }.as_bool() {
			return Err(Error::SourceUnavailable(format!(
				"window {} no longer exists",
				handle.0 as usize
			)));
		}
		let mut rect = RECT::default();
		unsafe { GetWindowRect(handle, &mut rect) }
			.map_err(|error| Error::SourceUnavailable(format!("read window geometry: {error}")))?;
		let width = ((rect.right - rect.left).max(0) as u32) & !1;
		let height = ((rect.bottom - rect.top).max(0) as u32) & !1;
		if width == 0 || height == 0 {
			return Err(Error::SourceUnavailable("window has no capturable area".to_string()));
		}
		let framerate = config.framerate.unwrap_or(DEFAULT_FRAMERATE).max(1);
		Ok(Self {
			handle,
			width,
			height,
			framerate,
			interval: Duration::from_micros(1_000_000 / u64::from(framerate)),
			next: Instant::now(),
			name: format!("window:{}", handle.0 as usize),
		})
	}

	fn read(&mut self) -> Result<Option<Surface>, Error> {
		let now = Instant::now();
		if self.next > now {
			std::thread::sleep(self.next - now);
		}
		self.next = Instant::now() + self.interval;

		if !unsafe { IsWindow(Some(self.handle)) }.as_bool() {
			return Err(Error::SourceUnavailable(format!("{} was closed", self.name)));
		}
		let bgra = snapshot(self.handle, self.width, self.height)?;
		Ok(Some(Surface::I420(I420::from_bgra(
			&bgra,
			self.width * 4,
			self.width,
			self.height,
		)?)))
	}
}

fn snapshot(handle: HWND, width: u32, height: u32) -> Result<Vec<u8>, Error> {
	let source = unsafe { GetWindowDC(Some(handle)) };
	if source.is_invalid() {
		return Err(last_capture_error("get window device context"));
	}
	let source = WindowDc(handle, source);
	let memory = unsafe { CreateCompatibleDC(Some(source.1)) };
	if memory.is_invalid() {
		return Err(last_capture_error("create memory device context"));
	}
	let memory = MemoryDc(memory);
	let bitmap = unsafe { CreateCompatibleBitmap(source.1, width as i32, height as i32) };
	if bitmap.is_invalid() {
		return Err(last_capture_error("create window bitmap"));
	}
	let bitmap = Bitmap(bitmap);
	let previous = unsafe { SelectObject(memory.0, HGDIOBJ(bitmap.0.0)) };
	if previous.is_invalid() {
		return Err(last_capture_error("select window bitmap"));
	}
	let selected = Selected { dc: memory.0, previous };
	// Ask the window to draw itself first, which works while it is occluded. Some
	// applications do not implement PrintWindow, so fall back to copying its DC.
	if !unsafe { PrintWindow(handle, memory.0, PRINT_WINDOW_FLAGS(2)) }.as_bool() {
		unsafe {
			BitBlt(
				memory.0,
				0,
				0,
				width as i32,
				height as i32,
				Some(source.1),
				0,
				0,
				SRCCOPY | CAPTUREBLT,
			)
			.map_err(|error| capture_error("copy window pixels", error))?;
		}
	}

	let mut info = BITMAPINFO {
		bmiHeader: BITMAPINFOHEADER {
			biSize: size_of::<BITMAPINFOHEADER>() as u32,
			biWidth: width as i32,
			biHeight: -(height as i32),
			biPlanes: 1,
			biBitCount: 32,
			biCompression: BI_RGB.0,
			..Default::default()
		},
		..Default::default()
	};
	let mut pixels = vec![0u8; width as usize * height as usize * 4];
	let rows = unsafe {
		GetDIBits(
			memory.0,
			bitmap.0,
			0,
			height,
			Some(pixels.as_mut_ptr().cast()),
			&mut info,
			DIB_RGB_COLORS,
		)
	};
	drop(selected);
	if rows != height as i32 {
		return Err(last_capture_error("read window bitmap"));
	}
	Ok(pixels)
}

fn title(handle: HWND) -> String {
	let length = unsafe { GetWindowTextLengthW(handle) };
	if length <= 0 {
		return String::new();
	}
	let mut buffer = vec![0u16; length as usize + 1];
	let copied = unsafe { GetWindowTextW(handle, &mut buffer) }.max(0) as usize;
	String::from_utf16_lossy(&buffer[..copied])
}

fn class_name(handle: HWND) -> String {
	let mut buffer = vec![0u16; 256];
	let copied = unsafe { GetClassNameW(handle, &mut buffer) }.max(0) as usize;
	String::from_utf16_lossy(&buffer[..copied])
}

fn parse(selector: &str) -> Result<HWND, Error> {
	let value = selector
		.strip_prefix("window:")
		.unwrap_or(selector)
		.parse::<usize>()
		.map_err(|_| Error::SourceUnavailable(format!("invalid Windows window selector {selector:?}")))?;
	Ok(HWND(value as *mut c_void))
}

fn last_capture_error(context: &str) -> Error {
	capture_error(context, windows::core::Error::from_thread())
}

fn capture_error(context: &str, error: windows::core::Error) -> Error {
	if error.code() == E_ACCESSDENIED {
		Error::PermissionDenied(format!("{context}: {error}"))
	} else {
		Error::SourceUnavailable(format!("{context}: {error}"))
	}
}

struct WindowDc(HWND, HDC);

impl Drop for WindowDc {
	fn drop(&mut self) {
		unsafe {
			ReleaseDC(Some(self.0), self.1);
		}
	}
}

struct MemoryDc(HDC);

impl Drop for MemoryDc {
	fn drop(&mut self) {
		unsafe {
			let _ = DeleteDC(self.0);
		}
	}
}

struct Bitmap(HBITMAP);

impl Drop for Bitmap {
	fn drop(&mut self) {
		unsafe {
			let _ = DeleteObject(HGDIOBJ(self.0.0));
		}
	}
}

struct Selected {
	dc: HDC,
	previous: HGDIOBJ,
}

impl Drop for Selected {
	fn drop(&mut self) {
		unsafe {
			SelectObject(self.dc, self.previous);
		}
	}
}
