//! One WGC session for either a monitor or a window, owned by its MTA thread.

use std::ffi::c_void;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::Foundation::{Metadata::ApiInformation, TypedEventHandler};
use windows::Graphics::Capture::{
	Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureAccess, GraphicsCaptureAccessKind,
	GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::{Direct3D11::IDirect3DDevice, DirectXPixelFormat};
use windows::Security::Authorization::AppCapabilityAccess::AppCapabilityAccessStatus;
use windows::Win32::Foundation::{CloseHandle, E_ACCESSDENIED, HANDLE, HWND, LPARAM, RECT};
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFOEXW};
use windows::Win32::System::Threading::{
	OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::System::WinRT::Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize};
use windows::Win32::UI::HiDpi::{
	DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::{
	EnumWindows, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
};
use windows::core::{IInspectable, Interface, PWSTR, factory, h};

use super::super::settle::{Settle, Settled};
use super::super::{Config, Display, FrameChannel, Source, Stream, Window};
use super::{Delivery, Event, Signal, Startup, display_index, window_handle};
use crate::frame::{Surface, d3d11};
use crate::{Error, Rate, Size};

const BUFFERS: i32 = 2;

pub(in crate::capture) async fn open(config: &Config) -> Result<Stream, Error> {
	let chan = FrameChannel::new();
	let signal = Arc::new(Signal::default());
	let label = config.source.label();
	let rate = config.framerate.unwrap_or(Rate::integer(30));
	let handle = std::thread::Builder::new()
		.name("moq-wgc".into())
		.spawn({
			let chan = chan.clone();
			let signal = signal.clone();
			let config = config.clone();
			move || {
				// A panic must also release a reader awaiting its first frame.
				let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&config, &chan, &signal)));
				match result {
					Ok(Ok(())) => chan.close(),
					Ok(Err(error)) => chan.fail(error),
					Err(_) => chan.fail(Error::Codec(anyhow::anyhow!("WGC capture thread panicked"))),
				}
			}
		})
		.map_err(|error| Error::Codec(anyhow::anyhow!("start WGC thread: {error}")))?;
	let guard = Guard {
		signal,
		handle: Some(handle),
	};
	let first = chan
		.recv()
		.await?
		.ok_or_else(|| Error::SourceUnavailable(format!("{label} ended before its first frame")))?;
	let Size { width, height } = first.size();
	tracing::info!(source = %label, width, height, "opened screen capture (WGC)");
	Ok(Stream::new(
		chan,
		width,
		height,
		Some(rate),
		label,
		Some(first),
		Box::new(guard),
	))
}

struct Guard {
	signal: Arc<Signal>,
	handle: Option<JoinHandle<()>>,
}

impl Drop for Guard {
	fn drop(&mut self) {
		self.signal.notify(Event::Stop);
		if let Some(handle) = self.handle.take() {
			let _ = handle.join();
		}
	}
}

struct Apartment;

impl Drop for Apartment {
	fn drop(&mut self) {
		unsafe { RoUninitialize() };
	}
}

fn run(config: &Config, chan: &Arc<FrameChannel>, signal: &Arc<Signal>) -> Result<(), Error> {
	unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.map_err(|e| error("initialize WinRT", e))?;
	let _apartment = Apartment;
	if !ApiInformation::IsPropertyPresent(
		h!("Windows.Graphics.Capture.GraphicsCaptureSession"),
		h!("IsCursorCaptureEnabled"),
	)
	.map_err(|e| error("query WGC cursor support", e))?
		|| !GraphicsCaptureSession::IsSupported().map_err(|e| error("query WGC support", e))?
	{
		return Err(Error::Unsupported(
			"Windows.Graphics.Capture requires Windows 10 2004 (build 19041) or newer".into(),
		));
	}
	let mut capture = Capture::new(config, chan, signal)?;
	// Never block this owner thread on a consent dialog: stop must remain usable.
	// The session keeps its border until access completes successfully.
	let access = if ApiInformation::IsPropertyPresent(
		h!("Windows.Graphics.Capture.GraphicsCaptureSession"),
		h!("IsBorderRequired"),
	)
	.map_err(|e| error("query WGC border support", e))?
	{
		match GraphicsCaptureAccess::RequestAccessAsync(GraphicsCaptureAccessKind::Borderless) {
			Ok(access) => {
				let signal = signal.clone();
				if let Err(err) = access.when(move |result| match result {
					Ok(AppCapabilityAccessStatus::Allowed) => signal.notify(Event::Borderless),
					Ok(status) => tracing::debug!(?status, "WGC borderless access not granted; keeping border"),
					Err(err) => tracing::debug!(%err, "WGC borderless request failed; keeping border"),
				}) {
					let _ = access.Cancel();
					tracing::debug!(%err, "WGC borderless callback unavailable; keeping border");
				}
				Some(access)
			}
			Err(err) => {
				tracing::debug!(%err, "WGC borderless unavailable; keeping border");
				None
			}
		}
	} else {
		None
	};
	let result = capture.run(config, chan, signal);
	if let Some(access) = access {
		let _ = access.Cancel();
	}
	result
}

struct Capture {
	item: GraphicsCaptureItem,
	pool: Direct3D11CaptureFramePool,
	session: Option<GraphicsCaptureSession>,
	device: ID3D11Device,
	frame_token: Option<i64>,
	closed_token: Option<i64>,
	window: Option<HWND>,
}

impl Capture {
	fn new(config: &Config, chan: &Arc<FrameChannel>, signal: &Arc<Signal>) -> Result<Self, Error> {
		let _dpi = DpiContext::enter()?;
		let interop =
			factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>().map_err(|e| error("WGC item factory", e))?;
		let (item, window): (GraphicsCaptureItem, _) = match &config.source {
			Source::Display(selector) => {
				let index = display_index(selector.as_deref())?;
				let monitors = monitors()?;
				let monitor = monitors
					.get(index)
					.ok_or_else(|| Error::SourceUnavailable(format!("no display at index {index}")))?;
				(
					unsafe { interop.CreateForMonitor(monitor.handle) }
						.map_err(|e| error("create WGC monitor item", e))?,
					None,
				)
			}
			Source::Window(selector) => {
				let handle = HWND(window_handle(selector)? as *mut c_void);
				if unsafe { IsIconic(handle) }.as_bool() {
					return Err(Error::SourceUnavailable(
						"cannot start capture from a minimized window".into(),
					));
				}
				(
					unsafe { interop.CreateForWindow(handle) }.map_err(|e| error("create WGC window item", e))?,
					Some(handle),
				)
			}
			_ => return Err(Error::Unsupported("WGC captures one display or window".into())),
		};
		let size = item.Size().map_err(|e| error("get WGC item size", e))?;
		if size.Width < 2 || size.Height < 2 {
			return Err(Error::SourceUnavailable("WGC item has no capturable area".into()));
		}
		let device = d3d11::create_device()?;
		let dxgi: IDXGIDevice = device.cast().map_err(|e| error("query capture DXGI device", e))?;
		let winrt: IDirect3DDevice = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi) }
			.and_then(|device| device.cast())
			.map_err(|e| error("create WinRT D3D device", e))?;
		let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
			&winrt,
			DirectXPixelFormat::B8G8R8A8UIntNormalized,
			BUFFERS,
			size,
		)
		.map_err(|e| error("create WGC frame pool", e))?;
		let mut capture = Self {
			item,
			pool,
			session: None,
			device,
			frame_token: None,
			closed_token: None,
			window,
		};
		capture.closed_token = Some(
			capture
				.item
				.Closed(&TypedEventHandler::<GraphicsCaptureItem, IInspectable>::new({
					let signal = signal.clone();
					let chan = chan.clone();
					let label = config.source.label();
					move |_, _| {
						chan.fail(Error::SourceUnavailable(format!("{label} closed")));
						signal.notify(Event::Closed);
						Ok(())
					}
				}))
				.map_err(|e| error("register WGC item closed", e))?,
		);
		capture.frame_token = Some(
			capture
				.pool
				.FrameArrived(&TypedEventHandler::<Direct3D11CaptureFramePool, IInspectable>::new({
					let signal = signal.clone();
					move |_, _| {
						signal.notify(Event::Frame);
						Ok(())
					}
				}))
				.map_err(|e| error("register WGC frames", e))?,
		);
		let session = capture
			.pool
			.CreateCaptureSession(&capture.item)
			.map_err(|e| error("create WGC session", e))?;
		capture.session = Some(session);
		capture
			.session
			.as_ref()
			.unwrap()
			.SetIsCursorCaptureEnabled(config.cursor)
			.map_err(|e| error("configure WGC cursor", e))?;
		Ok(capture)
	}

	fn run(&mut self, config: &Config, chan: &FrameChannel, signal: &Signal) -> Result<(), Error> {
		self.session
			.as_ref()
			.unwrap()
			.StartCapture()
			.map_err(|e| error("start WGC", e))?;
		let mut startup = Startup::new(Instant::now());
		let rate = config.framerate.unwrap_or(Rate::integer(30));
		let interval =
			Duration::from_nanos(1_000_000_000 * u64::from(rate.denominator()) / u64::from(rate.numerator()));
		let mut opened = None;
		let mut settle = None;
		let mut pending: Option<Captured> = None;
		let mut delivery = Delivery::new(interval, Instant::now());
		loop {
			let resize_deadline = settle.as_ref().and_then(Settle::deadline);
			let deadline = resize_deadline
				.into_iter()
				.chain(startup.deadline)
				.chain(delivery.deadline())
				.chain(pending.as_ref().map(|_| delivery.next))
				.min();
			match signal.wait(deadline) {
				Event::Stop => return Ok(()),
				Event::Closed => return Err(Error::SourceUnavailable(format!("{} closed", config.source.label()))),
				Event::Borderless => {
					if let Err(err) = self.session.as_ref().unwrap().SetIsBorderRequired(false) {
						tracing::debug!(%err, "WGC borderless denied; keeping border");
					}
				}
				Event::Frame => {
					// Bound each drain so an active desktop cannot starve shutdown.
					for _ in 0..BUFFERS {
						let Some(frame) = self.next()? else { break };
						pending = Some(frame);
					}
				}
				Event::Deadline => {}
			}
			let now = Instant::now();
			startup.check(now)?;
			if self.window.is_some_and(|window| unsafe { IsIconic(window) }.as_bool()) {
				if opened.is_none() {
					return Err(Error::SourceUnavailable(
						"cannot start capture from a minimized window".into(),
					));
				}
				pending = None;
				delivery.clear();
				settle = opened.map(Settle::new);
				continue;
			}
			if let Some(frame) = pending.as_ref() {
				let native = frame.0.ContentSize().map_err(|e| error("read WGC content size", e))?;
				if native.Width < 2 || native.Height < 2 {
					pending = None;
					delivery.clear();
					settle = opened.map(Settle::new);
					continue;
				}
				let size = Size::new(native.Width as u32, native.Height as u32);
				if opened.is_none() {
					opened = Some(size);
					settle = Some(Settle::new(size));
				}
				match settle.as_mut().unwrap().observe(&size, now) {
					Settled::Open => {}
					Settled::Waiting => {
						pending = None;
						delivery.clear();
					}
					Settled::Changed => return Ok(()),
				}
			}
			if settle
				.as_ref()
				.and_then(Settle::deadline)
				.is_some_and(|deadline| now >= deadline)
			{
				tracing::info!(source = %config.source.label(), "WGC source resized; ending capture for reopen");
				return Ok(());
			}
			if now >= delivery.next
				&& let Some(frame) = pending.take()
			{
				let ticks = frame
					.0
					.SystemRelativeTime()
					.map_err(|e| error("read WGC timestamp", e))?
					.Duration;
				let micros =
					u64::try_from(ticks).map_err(|_| Error::Codec(anyhow::anyhow!("negative WGC timestamp")))? / 10;
				let timestamp = moq_net::Timestamp::from_micros(micros)?;
				let surface = frame.0.Surface().map_err(|e| error("read WGC surface", e))?;
				let access: IDirect3DDxgiInterfaceAccess = surface.cast().map_err(|e| error("query WGC surface", e))?;
				let texture: ID3D11Texture2D =
					unsafe { access.GetInterface() }.map_err(|e| error("read WGC texture", e))?;
				let texture = d3d11::Texture::capture(&self.device, &texture, opened.unwrap())?;
				delivery.replace(texture, timestamp, now);
			}
			if let Some((texture, timestamp)) = delivery.next(now)? {
				// Only AddRef the owned NV12 output. The WGC pool frame was released
				// after conversion, and the cached texture is never written again.
				let texture = d3d11::Texture {
					device: texture.device.clone(),
					texture: texture.texture.clone(),
					width: texture.width,
					height: texture.height,
					color: texture.color,
				};
				chan.push_native(Surface::Texture(texture), timestamp);
				startup.delivered();
			}
		}
	}

	fn next(&self) -> Result<Option<Captured>, Error> {
		let mut frame = std::ptr::null_mut();
		// The projection rejects S_OK + null, but WGC uses it to mean an empty pool.
		unsafe {
			(self.pool.vtable().TryGetNextFrame)(self.pool.as_raw(), &mut frame)
				.ok()
				.map_err(|e| error("read WGC frame", e))?;
		}
		Ok((!frame.is_null()).then(|| Captured(unsafe { Direct3D11CaptureFrame::from_raw(frame) })))
	}
}

impl Drop for Capture {
	fn drop(&mut self) {
		// No notification mutex is held while calling into callbacks/COM teardown.
		if let Some(token) = self.frame_token.take() {
			let _ = self.pool.RemoveFrameArrived(token);
		}
		if let Some(token) = self.closed_token.take() {
			let _ = self.item.RemoveClosed(token);
		}
		if let Some(session) = self.session.take() {
			let _ = session.Close();
		}
		let _ = self.pool.Close();
	}
}

struct Captured(Direct3D11CaptureFrame);
impl Drop for Captured {
	fn drop(&mut self) {
		let _ = self.0.Close();
	}
}

struct Monitor {
	handle: HMONITOR,
	info: MONITORINFOEXW,
}

fn monitors() -> Result<Vec<Monitor>, Error> {
	let mut handles = Vec::new();
	unsafe {
		EnumDisplayMonitors(
			None,
			None,
			Some(collect_monitor),
			LPARAM((&mut handles as *mut Vec<HMONITOR>) as isize),
		)
	}
	.ok()
	.map_err(|e| error("enumerate monitors", e))?;
	let mut result = Vec::new();
	for handle in handles {
		let mut info = MONITORINFOEXW::default();
		info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
		unsafe { GetMonitorInfoW(handle, &mut info.monitorInfo) }
			.ok()
			.map_err(|e| error("get monitor info", e))?;
		result.push(Monitor { handle, info });
	}
	// MONITORINFOF_PRIMARY = 1. Keep the default display first on hybrid systems.
	result.sort_by_key(|monitor| monitor.info.monitorInfo.dwFlags & 1 == 0);
	Ok(result)
}

unsafe extern "system" fn collect_monitor(
	monitor: HMONITOR,
	_: HDC,
	_: *mut RECT,
	data: LPARAM,
) -> windows::core::BOOL {
	unsafe { &mut *(data.0 as *mut Vec<HMONITOR>) }.push(monitor);
	true.into()
}

pub(in crate::capture) fn displays() -> Result<Vec<Display>, Error> {
	let _dpi = DpiContext::enter()?;
	Ok(monitors()?
		.into_iter()
		.enumerate()
		.map(|(index, monitor)| {
			let rect = monitor.info.monitorInfo.rcMonitor;
			let name = &monitor.info.szDevice;
			Display {
				id: format!("display:{index}"),
				name: String::from_utf16_lossy(&name[..name.iter().position(|v| *v == 0).unwrap_or(name.len())]),
				width: (rect.right - rect.left).max(0) as u32,
				height: (rect.bottom - rect.top).max(0) as u32,
			}
		})
		.collect())
}

pub(in crate::capture) fn windows() -> Result<Vec<Window>, Error> {
	let _dpi = DpiContext::enter()?;
	let mut handles = Vec::new();
	unsafe { EnumWindows(Some(collect_window), LPARAM((&mut handles as *mut Vec<HWND>) as isize)) }
		.map_err(|e| error("enumerate windows", e))?;
	let mut result = Vec::new();
	for handle in handles {
		let mut cloaked = 0u32;
		let mut rect = RECT::default();
		unsafe {
			if DwmGetWindowAttribute(
				handle,
				DWMWA_CLOAKED,
				(&mut cloaked as *mut u32).cast(),
				std::mem::size_of_val(&cloaked) as u32,
			)
			.is_err() || cloaked != 0
				|| DwmGetWindowAttribute(
					handle,
					DWMWA_EXTENDED_FRAME_BOUNDS,
					(&mut rect as *mut RECT).cast(),
					std::mem::size_of_val(&rect) as u32,
				)
				.is_err()
			{
				continue;
			}
		}
		let width = (rect.right - rect.left).max(0) as u32;
		let height = (rect.bottom - rect.top).max(0) as u32;
		let length = unsafe { GetWindowTextLengthW(handle) };
		if width < 2 || height < 2 || length <= 0 {
			continue;
		}
		let mut title = vec![0u16; length as usize + 1];
		let copied = unsafe { GetWindowTextW(handle, &mut title) }.max(0) as usize;
		if copied == 0 {
			continue;
		}
		result.push(Window {
			id: format!("window:{}", handle.0 as usize),
			title: String::from_utf16_lossy(&title[..copied]),
			app: app_name(handle),
			width,
			height,
		});
	}
	Ok(result)
}

unsafe extern "system" fn collect_window(handle: HWND, data: LPARAM) -> windows::core::BOOL {
	if unsafe { IsWindowVisible(handle) }.as_bool() {
		unsafe { &mut *(data.0 as *mut Vec<HWND>) }.push(handle);
	}
	true.into()
}

fn app_name(handle: HWND) -> String {
	let mut id = 0;
	unsafe { GetWindowThreadProcessId(handle, Some(&mut id)) };
	let Ok(process) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, id) }) else {
		return String::new();
	};
	let process = Process(process);
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

struct Process(HANDLE);
impl Drop for Process {
	fn drop(&mut self) {
		let _ = unsafe { CloseHandle(self.0) };
	}
}

struct DpiContext(DPI_AWARENESS_CONTEXT);
impl DpiContext {
	fn enter() -> Result<Self, Error> {
		let previous = unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
		if previous.0.is_null() {
			return Err(error(
				"enable per-monitor DPI awareness",
				windows::core::Error::from_thread(),
			));
		}
		Ok(Self(previous))
	}
}
impl Drop for DpiContext {
	fn drop(&mut self) {
		unsafe { SetThreadDpiAwarenessContext(self.0) };
	}
}

fn error(context: &str, error: windows::core::Error) -> Error {
	if error.code() == E_ACCESSDENIED {
		Error::PermissionDenied(format!("{context}: {error}"))
	} else {
		Error::Codec(anyhow::anyhow!("{context}: {error}"))
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	#[ignore = "requires a Windows desktop; creates a minimized test window"]
	async fn wgc_minimized_window_is_rejected_before_the_first_frame() {
		use windows::Win32::UI::WindowsAndMessaging::{
			CreateWindowExW, DestroyWindow, WS_MINIMIZE, WS_OVERLAPPEDWINDOW,
		};
		use windows::core::w;

		struct TestWindow(HWND);
		impl Drop for TestWindow {
			fn drop(&mut self) {
				let _ = unsafe { DestroyWindow(self.0) };
			}
		}
		let window = TestWindow(unsafe {
			CreateWindowExW(
				Default::default(),
				w!("STATIC"),
				w!("WGC minimized-window test"),
				WS_OVERLAPPEDWINDOW | WS_MINIMIZE,
				0,
				0,
				640,
				480,
				None,
				None,
				None,
				None,
			)
			.expect("create test window")
		});
		assert!(unsafe { IsIconic(window.0) }.as_bool());
		let config = Config {
			source: Source::Window(format!("window:{}", window.0.0 as usize)),
			..Default::default()
		};
		assert!(matches!(
			open(&config).await,
			Err(Error::SourceUnavailable(message)) if message.contains("minimized")
		));
	}

	#[tokio::test]
	#[ignore = "requires an interactive Windows desktop; captures each enumerated monitor"]
	async fn wgc_displays_reopen_with_cursor_on_and_off() {
		let displays = displays().expect("enumerate monitors");
		assert!(!displays.is_empty(), "requires at least one attached monitor");
		for display in displays {
			capture_and_release(display.source()).await;
		}
	}

	#[tokio::test]
	#[ignore = "set MOQ_WGC_WINDOW to an open window:HWND; use an odd-sized visible window"]
	async fn wgc_window_reopens_with_cursor_on_and_off() {
		let selector = std::env::var("MOQ_WGC_WINDOW").expect("set MOQ_WGC_WINDOW=window:HWND from moq devices");
		capture_and_release(Source::Window(selector)).await;
	}

	async fn capture_and_release(source: Source) {
		for cursor in [true, false] {
			let config = Config {
				source: source.clone(),
				cursor,
				..Default::default()
			};
			let mut stream = open(&config).await.expect("open WGC");
			let frame = stream.read().await.unwrap().expect("first WGC frame");
			assert_eq!(frame.size().width % 2, 0);
			assert_eq!(frame.size().height % 2, 0);
			assert!(
				matches!(&frame.surface, Surface::Texture(_)),
				"capture must stay on the GPU"
			);
			let color = stream.color();
			assert_eq!(color, Some(crate::Color::infer(frame.size())));
			// Stop before another frame arrives; the returned pixels must outlive the pool.
			drop(stream);
			let pixels = frame
				.surface
				.to_i420()
				.expect("read owned texture after closing capture");
			assert_eq!(pixels.size(), frame.size());
			assert_eq!(pixels.color(), color);
		}
	}
}
