//! Native Windows display capture via DXGI Desktop Duplication.
//!
//! Duplicates a monitor's output on a Direct3D11 device and pulls each desktop
//! frame off the GPU as a BGRA texture, copies it to a CPU staging texture, and
//! converts it to packed [`I420`] for the encoder. Whole-monitor capture only
//! (the Windows analogue of macOS ScreenCaptureKit display capture); per-window
//! capture would need Windows.Graphics.Capture instead.
//!
//! Runs on the shared blocking [`pump`] thread: `AcquireNextFrame` is a blocking
//! call with no async form, and the [`IDXGIOutputDuplication`] handle is `!Send`,
//! so building and driving it on one thread is the natural fit. The read loop
//! paces itself to the target frame rate, coalescing bursts of desktop updates
//! into one frame and re-emitting the last frame while the screen is static, so a
//! still desktop still produces a steady stream.

use std::time::{Duration, Instant};

use windows::Win32::Graphics::Direct3D11::{
	D3D11_CPU_ACCESS_READ, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
	ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::{
	DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO, IDXGIAdapter, IDXGIDevice, IDXGIOutput1,
	IDXGIOutputDuplication, IDXGIResource,
};
use windows::core::Interface;

use super::channel::FrameChannel;
use super::pump::{self, Geometry};
use super::{Config, FrameStream};
use crate::Error;
use crate::frame::{Frame, I420, d3d11};

const DEFAULT_FRAMERATE: u32 = 30;

fn err(ctx: &str, e: windows::core::Error) -> Error {
	Error::Codec(anyhow::anyhow!("{ctx}: {e}"))
}

/// Open a display capture and stream its frames over a pump thread.
pub(super) async fn open(config: &Config, device: Option<&str>) -> Result<FrameStream, Error> {
	let config = config.clone();
	// The device opens on the pump thread, so the selector has to be owned.
	let device = device.map(str::to_string);
	let chan = FrameChannel::new();
	let (geo, guard) = pump::spawn(
		chan.clone(),
		move || {
			let cap = Duplicator::open(&config, device.as_deref())?;
			let geometry = Geometry {
				width: cap.width,
				height: cap.height,
				framerate: Some(cap.framerate),
				device: cap.device_name.clone(),
			};
			Ok((cap, geometry))
		},
		Duplicator::read,
	)
	.await?;

	Ok(FrameStream::new(
		chan,
		geo.width,
		geo.height,
		geo.framerate,
		geo.device,
		None,
		Box::new(guard),
	))
}

/// An open monitor duplication, read frame-by-frame on the pump thread.
struct Duplicator {
	device: ID3D11Device,
	context: ID3D11DeviceContext,
	/// The duplicated output, kept so the duplication can be rebuilt after a
	/// `DXGI_ERROR_ACCESS_LOST` (a mode switch or fullscreen transition).
	output: IDXGIOutput1,
	dupl: IDXGIOutputDuplication,
	/// Reused CPU-readable copy of the desktop texture (constant size).
	staging: Option<ID3D11Texture2D>,
	/// Even-clamped capture size (I420 chroma is 2x2, so dimensions must be even).
	width: u32,
	height: u32,
	framerate: u32,
	interval: Duration,
	/// When the next frame is due; paces `read` to `framerate`.
	next_deadline: Option<Instant>,
	/// Most recent decoded frame, re-emitted while the screen is static.
	last: Option<I420>,
	device_name: String,
}

impl Duplicator {
	fn open(config: &Config, selector: Option<&str>) -> Result<Self, Error> {
		let device = d3d11::create_device()?;
		let context = unsafe {
			device
				.GetImmediateContext()
				.map_err(|e| err("GetImmediateContext", e))?
		};

		let index = select_output(selector)?;
		let output = enumerate_output(&device, index)?;
		let device_name = format!("display:{index}");
		let dupl = duplicate(&output, &device)?;

		let desc = unsafe { dupl.GetDesc() };
		// I420 needs even dimensions; clamp down (drop the last odd row/column).
		let width = desc.ModeDesc.Width & !1;
		let height = desc.ModeDesc.Height & !1;
		if width == 0 || height == 0 {
			return Err(Error::Codec(anyhow::anyhow!(
				"display {index} reported an unusable size {}x{}",
				desc.ModeDesc.Width,
				desc.ModeDesc.Height
			)));
		}

		let framerate = config.framerate.unwrap_or(DEFAULT_FRAMERATE).max(1);
		let mut cap = Self {
			device,
			context,
			output,
			dupl,
			staging: None,
			width,
			height,
			framerate,
			interval: Duration::from_micros(1_000_000 / framerate as u64),
			next_deadline: None,
			last: None,
			device_name,
		};

		// Seed the first frame so a static screen still has something to emit, and
		// so a broken duplication path fails here at open rather than mid-stream.
		// The desktop image may not be ready instantly, so retry briefly.
		for _ in 0..20 {
			if cap.capture_once(100)? {
				break;
			}
		}
		if cap.last.is_none() {
			return Err(Error::Codec(anyhow::anyhow!("no desktop frame within timeout")));
		}

		tracing::info!(
			display = %cap.device_name,
			width = cap.width,
			height = cap.height,
			framerate = cap.framerate,
			"opened Desktop Duplication capture"
		);
		Ok(cap)
	}

	/// Rebuild the duplication after `DXGI_ERROR_ACCESS_LOST` (e.g. a resolution
	/// change or a fullscreen exclusive app grabbing/releasing the output).
	fn reduplicate(&mut self) -> Result<(), Error> {
		self.dupl = duplicate(&self.output, &self.device)?;
		self.staging = None;
		Ok(())
	}

	/// Acquire at most one desktop frame, waiting up to `timeout_ms`. Returns
	/// `true` if a frame was captured into `last`, `false` on timeout (no update).
	fn capture_once(&mut self, timeout_ms: u32) -> Result<bool, Error> {
		let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
		let mut resource: Option<IDXGIResource> = None;
		match unsafe { self.dupl.AcquireNextFrame(timeout_ms, &mut info, &mut resource) } {
			Ok(()) => {}
			Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(false),
			Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => {
				self.reduplicate()?;
				return Ok(false);
			}
			Err(e) => return Err(err("AcquireNextFrame", e)),
		}

		let resource = resource.ok_or_else(|| Error::Codec(anyhow::anyhow!("AcquireNextFrame returned no surface")))?;
		let texture = resource
			.cast::<ID3D11Texture2D>()
			.map_err(|e| err("desktop surface is not a texture", e))?;

		// Always release the frame, even if the copy fails, or the next acquire
		// deadlocks (only one frame may be held at a time).
		let result = self.copy_to_last(&texture);
		unsafe {
			let _ = self.dupl.ReleaseFrame();
		}
		result?;
		Ok(true)
	}

	/// Copy the desktop texture to the staging texture and convert BGRA -> I420.
	fn copy_to_last(&mut self, texture: &ID3D11Texture2D) -> Result<(), Error> {
		let staging = self.ensure_staging(texture)?;
		unsafe { self.context.CopyResource(&staging, texture) };

		let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
		unsafe {
			self.context
				.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
				.map_err(|e| err("Map (staging)", e))?;
		}
		let _guard = UnmapGuard {
			context: &self.context,
			resource: &staging,
		};

		let pitch = mapped.RowPitch;
		let len = pitch as usize * self.height as usize;
		let bgra = unsafe { std::slice::from_raw_parts(mapped.pData as *const u8, len) };
		self.last = Some(I420::from_bgra(bgra, pitch, self.width, self.height)?);
		Ok(())
	}

	/// Lazily create (and cache) a CPU-readable staging texture matching the
	/// desktop texture's format and size.
	fn ensure_staging(&mut self, texture: &ID3D11Texture2D) -> Result<ID3D11Texture2D, Error> {
		if let Some(staging) = &self.staging {
			return Ok(staging.clone());
		}
		let mut desc = D3D11_TEXTURE2D_DESC::default();
		unsafe { texture.GetDesc(&mut desc) };
		desc.Usage = D3D11_USAGE_STAGING;
		desc.BindFlags = 0;
		desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
		desc.MiscFlags = 0;
		desc.ArraySize = 1;
		desc.MipLevels = 1;

		let mut staging: Option<ID3D11Texture2D> = None;
		unsafe {
			self.device
				.CreateTexture2D(&desc, None, Some(&mut staging))
				.map_err(|e| err("CreateTexture2D (staging)", e))?;
		}
		let staging = staging.ok_or_else(|| Error::Codec(anyhow::anyhow!("CreateTexture2D returned null")))?;
		self.staging = Some(staging.clone());
		Ok(staging)
	}

	/// Capture the next frame, paced to the target frame rate. Coalesces a burst
	/// of desktop updates into the latest frame, and re-emits the last frame when
	/// the screen hasn't changed, so the output rate stays steady.
	fn read(&mut self) -> Result<Option<Frame>, Error> {
		let deadline = *self.next_deadline.get_or_insert_with(|| Instant::now() + self.interval);

		loop {
			let now = Instant::now();
			if now >= deadline {
				break;
			}
			let remaining = (deadline - now).as_millis().max(1) as u32;
			// Blocks up to `remaining`; a new frame returns early, a static screen
			// times out at the deadline. Either way we exit the loop at the deadline.
			self.capture_once(remaining)?;
		}

		// Schedule the next frame; if we fell badly behind (a long stall), reset to
		// now so we don't then burst to catch up.
		let next = deadline + self.interval;
		self.next_deadline = Some(next.max(Instant::now()));

		Ok(self.last.clone().map(Frame::I420))
	}
}

struct UnmapGuard<'a> {
	context: &'a ID3D11DeviceContext,
	resource: &'a ID3D11Texture2D,
}

impl Drop for UnmapGuard<'_> {
	fn drop(&mut self) {
		unsafe { self.context.Unmap(self.resource, 0) };
	}
}

/// Which monitor to capture: a bare index or the `display:{index}` form that
/// [`FrameStream::device`](super::FrameStream) reports; `None` is the first one.
fn select_output(selector: Option<&str>) -> Result<u32, Error> {
	match selector {
		None => Ok(0),
		Some(spec) => spec
			.strip_prefix("display:")
			.unwrap_or(spec)
			.parse::<u32>()
			.map_err(|_| Error::Codec(anyhow::anyhow!("invalid display selector {spec:?}"))),
	}
}

/// Get the `index`th output (monitor) attached to the device's adapter.
fn enumerate_output(device: &ID3D11Device, index: u32) -> Result<IDXGIOutput1, Error> {
	let dxgi = device
		.cast::<IDXGIDevice>()
		.map_err(|e| err("device is not a DXGI device", e))?;
	let adapter: IDXGIAdapter = unsafe { dxgi.GetAdapter().map_err(|e| err("GetAdapter", e))? };
	let output = unsafe {
		adapter
			.EnumOutputs(index)
			.map_err(|_| Error::Codec(anyhow::anyhow!("no display at index {index}")))?
	};
	output
		.cast::<IDXGIOutput1>()
		.map_err(|e| err("output is not IDXGIOutput1", e))
}

/// Start duplicating `output` on `device`.
fn duplicate(output: &IDXGIOutput1, device: &ID3D11Device) -> Result<IDXGIOutputDuplication, Error> {
	unsafe { output.DuplicateOutput(device) }.map_err(|e| err("DuplicateOutput", e))
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::capture::Config;
	use crate::frame::Frame;

	/// Open the primary display, grab a few frames, and check geometry + frame
	/// size. Ignored because Desktop Duplication needs an interactive desktop
	/// session with a GPU output; skips cleanly when that isn't available.
	#[test]
	#[ignore]
	fn duplicates_primary_display() {
		let mut cap = match Duplicator::open(&Config::default(), None) {
			Ok(cap) => cap,
			Err(e) => {
				eprintln!("skipping: no Desktop Duplication available: {e}");
				return;
			}
		};

		assert!(cap.width >= 2 && cap.width % 2 == 0, "bad width {}", cap.width);
		assert!(cap.height >= 2 && cap.height % 2 == 0, "bad height {}", cap.height);

		for i in 0..5 {
			let frame = cap.read().expect("read frame");
			let Some(Frame::I420(i420)) = frame else {
				panic!("frame {i} was not I420");
			};
			assert_eq!(i420.width, cap.width);
			assert_eq!(i420.height, cap.height);
			assert_eq!(i420.data.len(), I420::len(cap.width, cap.height));
		}
		eprintln!("captured 5 frames at {}x{}", cap.width, cap.height);
	}
}
