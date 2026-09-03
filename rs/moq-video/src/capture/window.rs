//! Native Windows single-window capture.
//!
//! Desktop Duplication is monitor-scoped, so selected windows use GDI. The
//! backend enumerates visible top-level windows, copies the selected window DC
//! at the requested frame rate, and converts BGRA to I420. The shared
//! latest-frame channel keeps the GDI loop independent from encoder latency.

use std::ffi::c_void;
use std::mem::size_of;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, E_ACCESSDENIED, HANDLE, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
	BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC,
	DeleteObject, GetDIBits, GetWindowDC, HBITMAP, HDC, HGDIOBJ, ReleaseDC, SRCCOPY, SelectObject,
};
use windows::Win32::System::Threading::{
	OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::HiDpi::{
	DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE, SetThreadDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::{
	CURSOR_SHOWING, CURSORINFO, DI_NORMAL, DrawIconEx, EnumWindows, GetCursorInfo, GetIconInfo, GetPropW,
	GetWindowRect, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, HICON, ICONINFO, IsWindow,
	IsWindowVisible, PRF_CHILDREN, PRF_CLIENT, PRF_ERASEBKGND, PRF_NONCLIENT, RemovePropW, SMTO_ABORTIFHUNG,
	SMTO_BLOCK, SendMessageTimeoutW, SetPropW, WM_PRINT,
};
use windows::core::{BOOL, PCWSTR, PWSTR};

use super::channel::FrameChannel;
use super::pump::{self, Geometry};
use super::{Config, Stream, Window};
use crate::Error;
use crate::frame::{I420, Surface};

const DEFAULT_FRAMERATE: u32 = 30;
const PRINT_TIMEOUT_MS: u32 = 50;
static NEXT_WINDOW_IDENTITY: AtomicUsize = AtomicUsize::new(1);

/// List visible, titled top-level windows by native HWND.
pub(super) fn windows() -> Result<Vec<Window>, Error> {
	let _dpi = DpiContext::enter()?;
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
			app: app_name(handle),
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

struct Capture {
	handle: HWND,
	identity: WindowIdentity,
	width: u32,
	height: u32,
	framerate: u32,
	interval: Duration,
	next: Instant,
	name: String,
	wm_print: bool,
	cursor: bool,
	_dpi: DpiContext,
}

impl Capture {
	fn open(config: &Config, handle: HWND) -> Result<Self, Error> {
		let dpi = DpiContext::enter()?;
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
		let identity = WindowIdentity::new(handle)?;
		let framerate = config.framerate.unwrap_or(DEFAULT_FRAMERATE).max(1);
		Ok(Self {
			handle,
			identity,
			width,
			height,
			framerate,
			interval: Duration::from_micros(1_000_000 / u64::from(framerate)),
			next: Instant::now(),
			name: format!("window:{}", handle.0 as usize),
			wm_print: true,
			cursor: config.cursor,
			_dpi: dpi,
		})
	}

	fn read(&mut self) -> Result<Option<Surface>, Error> {
		let now = Instant::now();
		if self.next > now {
			std::thread::sleep(self.next - now);
		}
		self.next = Instant::now() + self.interval;

		if !self.identity.matches() {
			return Err(Error::SourceUnavailable(format!(
				"{} was closed or replaced",
				self.name
			)));
		}
		let mut rect = RECT::default();
		unsafe { GetWindowRect(self.handle, &mut rect) }
			.map_err(|error| Error::SourceUnavailable(format!("read window geometry: {error}")))?;
		let width = ((rect.right - rect.left).max(0) as u32) & !1;
		let height = ((rect.bottom - rect.top).max(0) as u32) & !1;
		if (width, height) != (self.width, self.height) {
			// The encoder's geometry is fixed at open, so end the stream and let
			// the caller reopen against the new size.
			tracing::info!(
				source = %self.name,
				from = %format_args!("{}x{}", self.width, self.height),
				to = %format_args!("{width}x{height}"),
				"window resized; ending capture"
			);
			return Ok(None);
		}
		let (bgra, printed) = snapshot(self.handle, self.width, self.height, self.wm_print, self.cursor)?;
		if !self.identity.matches() {
			return Err(Error::SourceUnavailable(format!(
				"{} was closed or replaced",
				self.name
			)));
		}
		self.wm_print &= printed;
		Ok(Some(Surface::I420(I420::from_bgra(
			&bgra,
			self.width * 4,
			self.width,
			self.height,
		)?)))
	}
}

struct DpiContext(DPI_AWARENESS_CONTEXT);

impl DpiContext {
	fn enter() -> Result<Self, Error> {
		let previous = unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE) };
		if previous.0.is_null() {
			return Err(last_capture_error("enable per-monitor DPI awareness"));
		}
		Ok(Self(previous))
	}
}

impl Drop for DpiContext {
	fn drop(&mut self) {
		unsafe { SetThreadDpiAwarenessContext(self.0) };
	}
}

struct WindowIdentity {
	handle: HWND,
	name: Vec<u16>,
	token: HANDLE,
}

impl WindowIdentity {
	fn new(handle: HWND) -> Result<Self, Error> {
		let value = NEXT_WINDOW_IDENTITY.fetch_add(1, Ordering::Relaxed).max(1);
		let token = HANDLE(value as *mut c_void);
		let name = format!("moq.capture.identity.{}.{value}\0", std::process::id())
			.encode_utf16()
			.collect::<Vec<_>>();
		unsafe { SetPropW(handle, PCWSTR(name.as_ptr()), Some(token)) }
			.map_err(|error| capture_error("mark window identity", error))?;
		Ok(Self { handle, name, token })
	}

	fn matches(&self) -> bool {
		unsafe { GetPropW(self.handle, PCWSTR(self.name.as_ptr())).0 == self.token.0 }
	}
}

impl Drop for WindowIdentity {
	fn drop(&mut self) {
		if self.matches() {
			let _ = unsafe { RemovePropW(self.handle, PCWSTR(self.name.as_ptr())) };
		}
	}
}

fn snapshot(handle: HWND, width: u32, height: u32, print: bool, cursor: bool) -> Result<(Vec<u8>, bool), Error> {
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
			SRCCOPY,
		)
		.map_err(|error| capture_error("copy window pixels", error))?;
	}
	// WM_PRINT keeps occluded windows useful but runs on the target UI thread.
	// Bound that request, then keep using the copied DC if the target times out.
	let printed = !print || print_window(handle, memory.0);
	if cursor {
		draw_cursor(handle, memory.0);
	}
	drop(selected);

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
	if rows != height as i32 {
		return Err(last_capture_error("read window bitmap"));
	}
	Ok((pixels, printed))
}

fn draw_cursor(handle: HWND, target: HDC) {
	let mut cursor = CURSORINFO {
		cbSize: size_of::<CURSORINFO>() as u32,
		..Default::default()
	};
	if unsafe { GetCursorInfo(&mut cursor) }.is_err() || cursor.flags != CURSOR_SHOWING {
		return;
	}

	let icon = HICON(cursor.hCursor.0);
	let mut info = ICONINFO::default();
	if unsafe { GetIconInfo(icon, &mut info) }.is_err() {
		return;
	}
	let _mask = (!info.hbmMask.is_invalid()).then(|| Bitmap(info.hbmMask));
	let _color = (!info.hbmColor.is_invalid()).then(|| Bitmap(info.hbmColor));

	let mut rect = RECT::default();
	if unsafe { GetWindowRect(handle, &mut rect) }.is_err() {
		return;
	}
	let x = cursor.ptScreenPos.x - rect.left - info.xHotspot as i32;
	let y = cursor.ptScreenPos.y - rect.top - info.yHotspot as i32;
	let _ = unsafe { DrawIconEx(target, x, y, icon, 0, 0, 0, None, DI_NORMAL) };
}

fn print_window(handle: HWND, target: HDC) -> bool {
	let flags = PRF_CLIENT | PRF_NONCLIENT | PRF_ERASEBKGND | PRF_CHILDREN;
	unsafe {
		SendMessageTimeoutW(
			handle,
			WM_PRINT,
			WPARAM(target.0 as usize),
			LPARAM(flags as isize),
			SMTO_BLOCK | SMTO_ABORTIFHUNG,
			PRINT_TIMEOUT_MS,
			None,
		)
		.0 != 0
	}
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

fn app_name(handle: HWND) -> String {
	let mut process_id = 0;
	unsafe { GetWindowThreadProcessId(handle, Some(&mut process_id)) };
	if process_id == 0 {
		return String::new();
	}
	let Ok(process) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process_id) }) else {
		return String::new();
	};
	let process = ProcessHandle(process);
	let mut path = vec![0u16; 32_768];
	let mut length = path.len() as u32;
	if unsafe { QueryFullProcessImageNameW(process.0, PROCESS_NAME_WIN32, PWSTR(path.as_mut_ptr()), &mut length) }
		.is_err()
	{
		return String::new();
	}
	let path = String::from_utf16_lossy(&path[..length as usize]);
	std::path::Path::new(&path)
		.file_stem()
		.and_then(|name| name.to_str())
		.unwrap_or_default()
		.to_string()
}

struct ProcessHandle(HANDLE);

impl Drop for ProcessHandle {
	fn drop(&mut self) {
		let _ = unsafe { CloseHandle(self.0) };
	}
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
