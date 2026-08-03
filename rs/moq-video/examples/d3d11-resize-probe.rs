//! Instrumented probe for the Direct3D11 GPU-resize wedge (Windows only).
//!
//! Direct3D11 GPU resizing is opt-in because a `VideoProcessorBlt` scaler wedged
//! on the device a DXVA decoder was running on. This binary is the harness for
//! finding out which call blocks and under which ordering: it runs one scaler
//! under several orderings and logs every Direct3D call around it, so the last
//! line before a wedge names the call that blocked.
//!
//! ```text
//! cargo run -p moq-video --example d3d11-resize-probe -- live
//! ```
//!
//! | Mode | Ordering |
//! | --- | --- |
//! | `bindflags` | Which input bind flags a video processor accepts. Not a wedge question, but the fix depends on it. |
//! | `cold` | Scaler on a device with no decoder. The control: proves the scaler itself works. |
//! | `warm` | Scaler built and blitted first, decoder bound to the same device after. |
//! | `live` | Decoder bound and decoding first, scaler built after. The reported failure. |
//! | `concurrent` | Decode loop on one thread, scaler on another, one shared device. |
//! | `crate` | Like `live`, but through the real `decode::Decoder`, to confirm the probe's decoder matches it. |
//! | `ladder` | One decoder, four hardware encoders, and a scaler on one device. The full `moq-transcode` shape. |
//!
//! Run one mode per process: a wedge leaves the device unusable, so a second
//! mode in the same process would measure the wreckage rather than itself.
//!
//! Every mode passes on an NVIDIA RTX 3070 Ti (driver 32.0.15.9186), including
//! `ladder` at 200 blits against four live encode sessions, so the wedge needs
//! hardware, a driver, or an ingredient this does not yet reproduce. Run it on a
//! machine that does wedge and the log names the call.
//!
//! `bindflags` did turn up a constraint that holds everywhere: a video processor
//! rejects an input view over a resource whose bind flags are
//! `D3D11_BIND_SHADER_RESOURCE` alone. It needs one of `DECODER`,
//! `VIDEO_ENCODER`, `RENDER_TARGET`, `UNORDERED_ACCESS`, or none at all. What
//! `frame::d3d11::bind_flags` asks for is accepted, but only because the driver
//! granted `RENDER_TARGET` on NV12; a driver that grants only shader sampling
//! leaves a texture that cannot be scaled by a video processor at all.
//!
//! The probe prints its PID on startup and a watchdog line every two seconds
//! naming the call in flight, so a wedge is one `procdump -ma <pid>` away from
//! a stack. Pass `--debug-layer` to add `D3D11_CREATE_DEVICE_DEBUG`, which
//! often names the problem before the wedge does.
//!
//! The decode session here is a deliberately minimal reimplementation of
//! `decode::backend::mediafoundation`. The question is which ordering of device
//! creation, MFT binding, and scaler creation wedges, and the crate's backend
//! creates its own device and binds the MFT to it before any caller can see it.
//! The `crate` mode cross-checks the two against each other.

#[cfg(not(target_os = "windows"))]
fn main() {
	eprintln!("d3d11-resize-probe only runs on Windows");
}

#[cfg(target_os = "windows")]
fn main() -> anyhow::Result<()> {
	probe::main()
}

#[cfg(target_os = "windows")]
mod probe {
	use std::ffi::c_void;
	use std::mem::{ManuallyDrop, size_of};
	use std::ptr;
	use std::sync::Mutex;
	use std::sync::atomic::{AtomicBool, Ordering};
	use std::time::{Duration, Instant};

	use anyhow::{Context, Result, anyhow, bail};
	use moq_video::windows::Win32::Foundation::{HMODULE, RECT};
	use moq_video::windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
	use moq_video::windows::Win32::Graphics::Direct3D10::ID3D10Multithread;
	use moq_video::windows::Win32::Graphics::Direct3D11::{
		D3D11_BIND_DECODER, D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_BIND_VIDEO_ENCODER, D3D11_BOX,
		D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_DEBUG,
		D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_MESSAGE, D3D11_SDK_VERSION,
		D3D11_SUBRESOURCE_DATA, D3D11_TEX2D_VPIV, D3D11_TEX2D_VPOV, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
		D3D11_USAGE_STAGING, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE, D3D11_VIDEO_PROCESSOR_COLOR_SPACE,
		D3D11_VIDEO_PROCESSOR_CONTENT_DESC, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC,
		D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC,
		D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_STREAM, D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
		D3D11_VPIV_DIMENSION_TEXTURE2D, D3D11_VPOV_DIMENSION_TEXTURE2D, D3D11CreateDevice, ID3D11Device,
		ID3D11DeviceContext, ID3D11InfoQueue, ID3D11Texture2D, ID3D11VideoContext, ID3D11VideoDevice,
		ID3D11VideoProcessor, ID3D11VideoProcessorEnumerator, ID3D11VideoProcessorInputView,
		ID3D11VideoProcessorOutputView,
	};
	use moq_video::windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_RATIONAL, DXGI_SAMPLE_DESC};
	use moq_video::windows::Win32::Media::MediaFoundation::{
		IMFActivate, IMFDXGIBuffer, IMFDXGIDeviceManager, IMFSample, IMFTransform, MF_E_NO_MORE_TYPES,
		MF_E_NOTACCEPTING, MF_E_TRANSFORM_NEED_MORE_INPUT, MF_E_TRANSFORM_STREAM_CHANGE, MF_LOW_LATENCY,
		MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_VERSION, MFCreateDXGIDeviceManager, MFCreateMediaType,
		MFCreateMemoryBuffer, MFCreateSample, MFMediaType_Video, MFSTARTUP_FULL, MFShutdown, MFStartup,
		MFT_CATEGORY_VIDEO_DECODER, MFT_ENUM_FLAG_SORTANDFILTER, MFT_ENUM_FLAG_SYNCMFT,
		MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_SET_D3D_MANAGER, MFT_OUTPUT_DATA_BUFFER,
		MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MFT_REGISTER_TYPE_INFO, MFTEnumEx, MFVideoFormat_H264, MFVideoFormat_NV12,
	};
	use moq_video::windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize};
	use moq_video::windows::core::Interface;
	use moq_video::{Frame, I420, Size, Surface};

	/// The source stream the probe decodes, and the size it scales down to.
	const SOURCE: Size = Size {
		width: 320,
		height: 240,
	};
	const TARGET: Size = Size {
		width: 160,
		height: 120,
	};
	/// Frames to decode before the scaler runs. More than the decoder's pool has
	/// slices, so it is recycling by the time the scaler shows up.
	const FRAMES: u64 = 12;

	// ---------------------------------------------------------------- tracing

	/// The Direct3D or Media Foundation call currently in flight, and when it
	/// started. The watchdog reads it; a wedge leaves the culprit here.
	static IN_FLIGHT: Mutex<Option<(&'static str, Instant)>> = Mutex::new(None);

	/// The debug layer's message queue, when `--debug-layer` is on. Drained after
	/// every step, so a complaint is attributed to the call that provoked it.
	static INFO_QUEUE: Mutex<Option<ID3D11InfoQueue>> = Mutex::new(None);

	/// Run `f` as a named step, logging entry and exit. The last `-->` printed
	/// with no matching `<--` is the call that blocked.
	fn step<T>(name: &'static str, f: impl FnOnce() -> T) -> T {
		eprintln!("--> {name}");
		*IN_FLIGHT.lock().unwrap() = Some((name, Instant::now()));
		let started = Instant::now();
		let out = f();
		*IN_FLIGHT.lock().unwrap() = None;
		eprintln!("<-- {name} ({:.1?})", started.elapsed());
		drain_info_queue();
		out
	}

	/// Print and clear whatever the debug layer has to say. A no-op without
	/// `--debug-layer`.
	fn drain_info_queue() {
		let guard = INFO_QUEUE.lock().unwrap();
		let Some(queue) = guard.as_ref() else { return };

		for index in 0..unsafe { queue.GetNumStoredMessages() } {
			// Two passes: the first sizes the message, the second fills it in.
			let mut len = 0usize;
			if unsafe { queue.GetMessage(index, None, &mut len) }.is_err() || len == 0 {
				continue;
			}
			// The message contains pointers, so its backing allocation must carry
			// pointer alignment rather than the byte vector's alignment of one.
			let mut buffer = vec![0u64; len.div_ceil(size_of::<u64>())];
			let message = buffer.as_mut_ptr().cast::<D3D11_MESSAGE>();
			if unsafe { queue.GetMessage(index, Some(message), &mut len) }.is_err() {
				continue;
			}
			// SAFETY: the queue just filled `buffer` with a D3D11_MESSAGE whose
			// description points into the same allocation.
			let message = unsafe { &*message };
			let text = unsafe {
				std::slice::from_raw_parts(
					message.pDescription.cast::<u8>(),
					message.DescriptionByteLength.saturating_sub(1),
				)
			};
			eprintln!("    [d3d11] {}", String::from_utf8_lossy(text));
		}
		unsafe { queue.ClearStoredMessages() };
	}

	/// Report the in-flight call every two seconds, so a wedged run says what it
	/// is wedged on without a debugger attached.
	fn watchdog() {
		std::thread::spawn(|| {
			loop {
				std::thread::sleep(Duration::from_secs(2));
				if let Some((name, since)) = *IN_FLIGHT.lock().unwrap() {
					eprintln!("[watchdog] blocked {:.0?} in {name}", since.elapsed());
				}
			}
		});
	}

	// ------------------------------------------------------------------ setup

	/// COM (MTA) plus Media Foundation for this thread, undone on drop.
	struct ComGuard;

	impl ComGuard {
		fn new() -> Result<Self> {
			unsafe {
				CoInitializeEx(None, COINIT_MULTITHREADED)
					.ok()
					.context("CoInitializeEx")?;
				MFStartup(MF_VERSION, MFSTARTUP_FULL).context("MFStartup")?;
			}
			Ok(Self)
		}
	}

	impl Drop for ComGuard {
		fn drop(&mut self) {
			unsafe {
				let _ = MFShutdown();
				CoUninitialize();
			}
		}
	}

	/// A hardware Direct3D11 device shaped exactly like `frame::d3d11::create_device`:
	/// video support on, multithread protection on.
	fn create_device(mut debug_layer: bool) -> Result<ID3D11Device> {
		let mut flags = D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT;
		if debug_layer {
			flags |= D3D11_CREATE_DEVICE_DEBUG;
		}

		let create = |flags| {
			let mut device: Option<ID3D11Device> = None;
			step("D3D11CreateDevice", || unsafe {
				D3D11CreateDevice(
					None,
					D3D_DRIVER_TYPE_HARDWARE,
					HMODULE::default(),
					flags,
					None,
					D3D11_SDK_VERSION,
					Some(&mut device),
					None,
					None,
				)
			})
			.map(|()| device)
		};

		let device = match create(flags) {
			Ok(device) => device,
			// The debug layer needs the Graphics Tools optional feature; without
			// it the flag is the only reason this failed, so say so and go on.
			Err(e) if debug_layer => {
				eprintln!("debug layer unavailable ({e}); retrying without it");
				*INFO_QUEUE.lock().unwrap() = None;
				debug_layer = false;
				create(flags & !D3D11_CREATE_DEVICE_DEBUG).context("D3D11CreateDevice")?
			}
			Err(e) => return Err(e).context("D3D11CreateDevice"),
		};
		let device = device.ok_or_else(|| anyhow!("D3D11CreateDevice returned null"))?;

		let multithread = device.cast::<ID3D10Multithread>().context("query ID3D10Multithread")?;
		unsafe {
			let _ = multithread.SetMultithreadProtected(true);
		}

		if debug_layer {
			match device.cast::<ID3D11InfoQueue>() {
				Ok(queue) => *INFO_QUEUE.lock().unwrap() = Some(queue),
				Err(e) => eprintln!("no ID3D11InfoQueue ({e}); debug-layer messages will not be printed"),
			}
		}
		Ok(device)
	}

	// ----------------------------------------------------------------- scaler

	/// A `VideoProcessorBlt` scaler: the thing that wedges. Every call it makes
	/// is a named step, so the log pins which one blocked.
	struct Scaler {
		context: ID3D11VideoContext,
		enumerator: ID3D11VideoProcessorEnumerator,
		processor: ID3D11VideoProcessor,
		device: ID3D11VideoDevice,
	}

	impl Scaler {
		fn new(device: &ID3D11Device, src: Size, dst: Size) -> Result<Self> {
			let video_device = step("QI ID3D11VideoDevice", || device.cast::<ID3D11VideoDevice>())
				.context("device is not an ID3D11VideoDevice")?;
			let immediate = step("GetImmediateContext", || unsafe { device.GetImmediateContext() })
				.context("GetImmediateContext")?;
			let video_context = step("QI ID3D11VideoContext", || immediate.cast::<ID3D11VideoContext>())
				.context("context is not an ID3D11VideoContext")?;

			let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
				InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
				InputFrameRate: DXGI_RATIONAL {
					Numerator: 30,
					Denominator: 1,
				},
				InputWidth: src.width,
				InputHeight: src.height,
				OutputFrameRate: DXGI_RATIONAL {
					Numerator: 30,
					Denominator: 1,
				},
				OutputWidth: dst.width,
				OutputHeight: dst.height,
				Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
			};

			let enumerator = step("CreateVideoProcessorEnumerator", || unsafe {
				video_device.CreateVideoProcessorEnumerator(&desc)
			})
			.context("CreateVideoProcessorEnumerator")?;

			let processor = step("CreateVideoProcessor", || unsafe {
				video_device.CreateVideoProcessor(&enumerator, 0)
			})
			.context("CreateVideoProcessor")?;

			step("VideoProcessorSetStreamFrameFormat", || unsafe {
				video_context.VideoProcessorSetStreamFrameFormat(&processor, 0, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
			});
			step("VideoProcessorSetStreamAutoProcessingMode", || unsafe {
				video_context.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
			});
			step("VideoProcessorSetColorSpace", || unsafe {
				let space = D3D11_VIDEO_PROCESSOR_COLOR_SPACE::default();
				video_context.VideoProcessorSetStreamColorSpace(&processor, 0, &space);
				video_context.VideoProcessorSetOutputColorSpace(&processor, &space);
			});

			Ok(Self {
				context: video_context,
				enumerator,
				processor,
				device: video_device,
			})
		}

		/// A video-processor input view over `texture`. Split out because which
		/// textures this accepts is a question on its own; see `mode_bindflags`.
		fn input_view(&self, texture: &ID3D11Texture2D) -> Result<ID3D11VideoProcessorInputView> {
			let desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
				FourCC: 0,
				ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
				Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
					Texture2D: D3D11_TEX2D_VPIV {
						MipSlice: 0,
						ArraySlice: 0,
					},
				},
			};
			let mut view: Option<ID3D11VideoProcessorInputView> = None;
			step("CreateVideoProcessorInputView", || unsafe {
				self.device
					.CreateVideoProcessorInputView(texture, &self.enumerator, &desc, Some(&mut view))
			})
			.context("CreateVideoProcessorInputView")?;
			view.ok_or_else(|| anyhow!("input view is null"))
		}

		/// A video-processor output view over `texture`.
		fn output_view(&self, texture: &ID3D11Texture2D) -> Result<ID3D11VideoProcessorOutputView> {
			let desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
				ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
				Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
					Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
				},
			};
			let mut view: Option<ID3D11VideoProcessorOutputView> = None;
			step("CreateVideoProcessorOutputView", || unsafe {
				self.device
					.CreateVideoProcessorOutputView(texture, &self.enumerator, &desc, Some(&mut view))
			})
			.context("CreateVideoProcessorOutputView")?;
			view.ok_or_else(|| anyhow!("output view is null"))
		}

		/// Scale `src` into a freshly allocated `dst`-sized NV12 texture.
		fn scale(
			&self,
			device: &ID3D11Device,
			src: &ID3D11Texture2D,
			src_size: Size,
			dst: Size,
		) -> Result<ID3D11Texture2D> {
			let output = alloc_nv12(device, dst, D3D11_BIND_RENDER_TARGET.0 as u32)?;
			let input_view = self.input_view(src)?;
			let output_view = self.output_view(&output)?;

			// Whole source into the whole destination: the scale itself.
			let source_rect = RECT {
				left: 0,
				top: 0,
				right: src_size.width as i32,
				bottom: src_size.height as i32,
			};
			let dest_rect = RECT {
				left: 0,
				top: 0,
				right: dst.width as i32,
				bottom: dst.height as i32,
			};
			step("VideoProcessorSetStreamSourceRect", || unsafe {
				self.context
					.VideoProcessorSetStreamSourceRect(&self.processor, 0, true, Some(&source_rect));
			});
			step("VideoProcessorSetStreamDestRect", || unsafe {
				self.context
					.VideoProcessorSetStreamDestRect(&self.processor, 0, true, Some(&dest_rect));
			});

			let streams = [D3D11_VIDEO_PROCESSOR_STREAM {
				Enable: true.into(),
				OutputIndex: 0,
				InputFrameOrField: 0,
				PastFrames: 0,
				FutureFrames: 0,
				ppPastSurfaces: ptr::null_mut(),
				pInputSurface: ManuallyDrop::new(Some(input_view)),
				ppFutureSurfaces: ptr::null_mut(),
				ppPastSurfacesRight: ptr::null_mut(),
				pInputSurfaceRight: ManuallyDrop::new(None),
				ppFutureSurfacesRight: ptr::null_mut(),
			}];

			let result = step("VideoProcessorBlt", || unsafe {
				self.context
					.VideoProcessorBlt(&self.processor, &output_view, 0, &streams)
			});
			// The stream borrowed the view; drop it by hand since the struct can't.
			let mut streams = streams;
			let _ = ManuallyDrop::into_inner(unsafe { ptr::read(&streams[0].pInputSurface) });
			unsafe { ptr::write(&mut streams[0].pInputSurface, ManuallyDrop::new(None)) };
			result.context("VideoProcessorBlt")?;

			// The wedge has been reported at the Blt, which is asynchronous: a
			// flush is where a queued command actually lands, so it is a candidate
			// too and gets its own step.
			let immediate = unsafe { device.GetImmediateContext() }.context("GetImmediateContext")?;
			step("Flush", || unsafe { immediate.Flush() });

			Ok(output)
		}
	}

	// --------------------------------------------------------------- textures

	/// A plain NV12 texture bound for exactly `bind`.
	///
	/// Exactly, not "at least": a video processor rejects an input view over a
	/// resource carrying `D3D11_BIND_SHADER_RESOURCE`, so the flags a texture is
	/// allocated with decide whether it can be scaled at all.
	fn alloc_nv12(device: &ID3D11Device, size: Size, bind: u32) -> Result<ID3D11Texture2D> {
		let desc = D3D11_TEXTURE2D_DESC {
			Width: size.width,
			Height: size.height,
			MipLevels: 1,
			ArraySize: 1,
			Format: DXGI_FORMAT_NV12,
			SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
			Usage: D3D11_USAGE_DEFAULT,
			BindFlags: bind,
			CPUAccessFlags: 0,
			MiscFlags: 0,
		};
		let mut texture: Option<ID3D11Texture2D> = None;
		unsafe {
			device
				.CreateTexture2D(&desc, None, Some(&mut texture))
				.context("CreateTexture2D")?;
		}
		texture.ok_or_else(|| anyhow!("CreateTexture2D returned null"))
	}

	/// Upload packed I420 as an NV12 texture with exactly `bind`, for the modes
	/// that need a picture without a decoder having produced one.
	fn upload_nv12(device: &ID3D11Device, i420: &I420, bind: u32) -> Result<ID3D11Texture2D> {
		let size = Size::new(i420.width(), i420.height());
		let (w, h) = (size.width as usize, size.height as usize);
		let (cw, ch) = (w / 2, h / 2);

		let mut nv12 = vec![0u8; w * h * 3 / 2];
		nv12[..w * h].copy_from_slice(i420.y());
		let (u, v) = (i420.u(), i420.v());
		for row in 0..ch {
			for col in 0..cw {
				nv12[w * h + row * w + col * 2] = u[row * cw + col];
				nv12[w * h + row * w + col * 2 + 1] = v[row * cw + col];
			}
		}

		let desc = D3D11_TEXTURE2D_DESC {
			Width: size.width,
			Height: size.height,
			MipLevels: 1,
			ArraySize: 1,
			Format: DXGI_FORMAT_NV12,
			SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
			Usage: D3D11_USAGE_DEFAULT,
			BindFlags: bind,
			CPUAccessFlags: 0,
			MiscFlags: 0,
		};
		let initial = D3D11_SUBRESOURCE_DATA {
			pSysMem: nv12.as_ptr().cast::<c_void>(),
			SysMemPitch: size.width,
			SysMemSlicePitch: 0,
		};
		let mut texture: Option<ID3D11Texture2D> = None;
		unsafe {
			device
				.CreateTexture2D(&desc, Some(&initial), Some(&mut texture))
				.context("CreateTexture2D (upload)")?;
		}
		texture.ok_or_else(|| anyhow!("CreateTexture2D returned null"))
	}

	/// Download an NV12 texture to packed I420, so a scaled result can be checked
	/// against a CPU resize of the same picture.
	fn download_i420(device: &ID3D11Device, texture: &ID3D11Texture2D, size: Size) -> Result<I420> {
		let context = unsafe { device.GetImmediateContext() }.context("GetImmediateContext")?;

		let mut desc = D3D11_TEXTURE2D_DESC::default();
		unsafe { texture.GetDesc(&mut desc) };
		let tex_height = desc.Height as usize;
		desc.ArraySize = 1;
		desc.MipLevels = 1;
		desc.Usage = D3D11_USAGE_STAGING;
		desc.BindFlags = 0;
		desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
		desc.MiscFlags = 0;

		let mut staging: Option<ID3D11Texture2D> = None;
		unsafe {
			device
				.CreateTexture2D(&desc, None, Some(&mut staging))
				.context("CreateTexture2D (staging)")?;
		}
		let staging = staging.ok_or_else(|| anyhow!("staging texture is null"))?;

		unsafe { context.CopySubresourceRegion(&staging, 0, 0, 0, 0, texture, 0, None) };

		let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
		step("Map (staging)", || unsafe {
			context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
		})
		.context("Map")?;
		let _guard = UnmapGuard {
			context: &context,
			resource: &staging,
		};

		let (w, h) = (size.width as usize, size.height as usize);
		let (cw, ch) = (w / 2, h / 2);
		let pitch = mapped.RowPitch as usize;
		let base = mapped.pData as *const u8;

		let mut data = vec![0u8; I420::len(size.width, size.height)];
		let (luma, chroma) = data.split_at_mut(w * h);
		let (u_plane, v_plane) = chroma.split_at_mut(cw * ch);
		for row in 0..h {
			unsafe { ptr::copy_nonoverlapping(base.add(row * pitch), luma[row * w..].as_mut_ptr(), w) };
		}
		let uv = unsafe { base.add(pitch * tex_height) };
		for row in 0..ch {
			let src = unsafe { uv.add(row * pitch) };
			for col in 0..cw {
				unsafe {
					u_plane[row * cw + col] = *src.add(col * 2);
					v_plane[row * cw + col] = *src.add(col * 2 + 1);
				}
			}
		}

		I420::new(size.width, size.height, data).context("I420::new")
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

	// ------------------------------------------------------------ mft decoder

	/// A minimal DXVA H.264 decode session on a device the caller owns, so the
	/// probe controls when the MFT binds relative to the scaler.
	struct Decoder {
		transform: IMFTransform,
		device: ID3D11Device,
		size: Size,
		configured: bool,
		provides_samples: bool,
		index: i64,
		_manager: IMFDXGIDeviceManager,
	}

	// Touched from one thread at a time; `concurrent` moves the whole session to
	// its decode thread rather than sharing it.
	unsafe impl Send for Decoder {}

	impl Decoder {
		/// Bind a hardware H.264 decoder MFT to `device`.
		fn open(device: &ID3D11Device) -> Result<Self> {
			let transform = step("MFTEnumEx + ActivateObject", enumerate_decoder)?;

			let mut token = 0u32;
			let mut manager: Option<IMFDXGIDeviceManager> = None;
			unsafe {
				MFCreateDXGIDeviceManager(&mut token, &mut manager).context("MFCreateDXGIDeviceManager")?;
			}
			let manager = manager.ok_or_else(|| anyhow!("MFCreateDXGIDeviceManager returned null"))?;
			step("IMFDXGIDeviceManager::ResetDevice", || unsafe {
				manager.ResetDevice(device, token)
			})
			.context("ResetDevice")?;

			step("MFT_MESSAGE_SET_D3D_MANAGER", || unsafe {
				transform.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)
			})
			.context("set D3D manager")?;

			let attrs = unsafe { transform.GetAttributes() }.context("MFT GetAttributes")?;
			unsafe {
				let _ = attrs.SetUINT32(&MF_LOW_LATENCY, 1);
			}

			let media = unsafe { MFCreateMediaType() }.context("MFCreateMediaType")?;
			unsafe {
				media
					.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
					.context("major type")?;
				media.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264).context("subtype")?;
				transform.SetInputType(0, &media, 0).context("SetInputType")?;
				transform
					.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
					.context("begin streaming")?;
			}

			Ok(Self {
				transform,
				device: device.clone(),
				size: Size::new(0, 0),
				configured: false,
				provides_samples: false,
				index: 0,
				_manager: manager,
			})
		}

		/// Feed one Annex-B access unit, returning a texture per decoded picture.
		fn decode(&mut self, access_unit: &[u8]) -> Result<Vec<ID3D11Texture2D>> {
			let sample = self.build_sample(access_unit)?;
			let mut out = Vec::new();

			loop {
				match unsafe { self.transform.ProcessInput(0, &sample, 0) } {
					Ok(()) => break,
					Err(e) if e.code() == MF_E_NOTACCEPTING => {
						if !self.drain(&mut out)? {
							bail!("decoder rejected the access unit with no output to drain");
						}
					}
					Err(e) => return Err(anyhow!("ProcessInput: {e}")),
				}
			}
			self.index += 1;

			if !self.configured {
				self.configure_output()?;
			}
			self.drain(&mut out)?;
			Ok(out)
		}

		fn build_sample(&self, access_unit: &[u8]) -> Result<IMFSample> {
			let buffer = unsafe { MFCreateMemoryBuffer(access_unit.len() as u32) }.context("MFCreateMemoryBuffer")?;
			let mut dst: *mut u8 = ptr::null_mut();
			unsafe { buffer.Lock(&mut dst, None, None) }.context("lock input buffer")?;
			unsafe { std::slice::from_raw_parts_mut(dst, access_unit.len()) }.copy_from_slice(access_unit);
			unsafe {
				let _ = buffer.Unlock();
				buffer
					.SetCurrentLength(access_unit.len() as u32)
					.context("SetCurrentLength")?;
			}

			let sample = unsafe { MFCreateSample() }.context("MFCreateSample")?;
			// 30fps ticks in Media Foundation's 100ns units: the decoder only needs
			// them to increase.
			let tick = 10_000_000 / 30;
			unsafe {
				sample.AddBuffer(&buffer).context("AddBuffer")?;
				sample.SetSampleTime(self.index * tick).context("SetSampleTime")?;
				sample.SetSampleDuration(tick).context("SetSampleDuration")?;
			}
			Ok(sample)
		}

		fn configure_output(&mut self) -> Result<()> {
			let mut index = 0;
			loop {
				let media = match unsafe { self.transform.GetOutputAvailableType(0, index) } {
					Ok(media) => media,
					Err(e) if e.code() == MF_E_NO_MORE_TYPES => bail!("decoder offers no NV12 output type"),
					Err(e) => return Err(anyhow!("GetOutputAvailableType: {e}")),
				};
				let subtype = unsafe { media.GetGUID(&MF_MT_SUBTYPE) }.context("output subtype")?;
				if subtype == MFVideoFormat_NV12 {
					unsafe { self.transform.SetOutputType(0, &media, 0) }.context("SetOutputType")?;
					let packed = unsafe { media.GetUINT64(&MF_MT_FRAME_SIZE) }.context("frame size")?;
					self.size = Size::new((packed >> 32) as u32, packed as u32);
					let info = unsafe { self.transform.GetOutputStreamInfo(0) }.context("GetOutputStreamInfo")?;
					self.provides_samples = info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0;
					if !self.provides_samples {
						bail!("decoder is not DXVA-backed (no GPU textures), so there is nothing to probe");
					}
					self.configured = true;
					return Ok(());
				}
				index += 1;
			}
		}

		fn drain(&mut self, out: &mut Vec<ID3D11Texture2D>) -> Result<bool> {
			if !self.configured {
				return Ok(false);
			}
			let mut progressed = false;
			while self.process_output(out)? {
				progressed = true;
			}
			Ok(progressed)
		}

		fn process_output(&mut self, out: &mut Vec<ID3D11Texture2D>) -> Result<bool> {
			let mut data = [MFT_OUTPUT_DATA_BUFFER {
				dwStreamID: 0,
				pSample: ManuallyDrop::new(None),
				dwStatus: 0,
				pEvents: ManuallyDrop::new(None),
			}];
			let mut status = 0u32;
			let result = unsafe { self.transform.ProcessOutput(0, &mut data, &mut status) };
			let sample = ManuallyDrop::into_inner(unsafe { ptr::read(&data[0].pSample) });
			let _events = ManuallyDrop::into_inner(unsafe { ptr::read(&data[0].pEvents) });

			match result {
				Ok(()) => {
					if let Some(sample) = sample {
						out.push(self.copy_out(&sample)?);
					}
					Ok(true)
				}
				Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => Ok(false),
				Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
					self.configure_output()?;
					Ok(true)
				}
				Err(e) => Err(anyhow!("ProcessOutput: {e}")),
			}
		}

		/// Blit the picture out of the decoder's pool into a texture of ours, the
		/// same move `Texture::copy_from_sample` makes and for the same reason.
		fn copy_out(&self, sample: &IMFSample) -> Result<ID3D11Texture2D> {
			let buffer = unsafe { sample.GetBufferByIndex(0) }.context("GetBufferByIndex")?;
			let dxgi = buffer.cast::<IMFDXGIBuffer>().context("sample is not DXGI-backed")?;

			let mut raw: *mut c_void = ptr::null_mut();
			unsafe { dxgi.GetResource(&ID3D11Texture2D::IID, &mut raw) }.context("GetResource")?;
			let source = unsafe { ID3D11Texture2D::from_raw(raw) };
			let subresource = unsafe { dxgi.GetSubresourceIndex() }.context("GetSubresourceIndex")?;

			let owned = alloc_nv12(&self.device, SOURCE, D3D11_BIND_RENDER_TARGET.0 as u32)?;
			let region = D3D11_BOX {
				left: 0,
				top: 0,
				front: 0,
				right: SOURCE.width,
				bottom: SOURCE.height,
				back: 1,
			};
			let context = unsafe { self.device.GetImmediateContext() }.context("GetImmediateContext")?;
			unsafe { context.CopySubresourceRegion(&owned, 0, 0, 0, 0, &source, subresource, Some(&region)) };
			Ok(owned)
		}
	}

	/// The first synchronous hardware H.264 decoder MFT.
	fn enumerate_decoder() -> Result<IMFTransform> {
		let input = MFT_REGISTER_TYPE_INFO {
			guidMajorType: MFMediaType_Video,
			guidSubtype: MFVideoFormat_H264,
		};
		let output = MFT_REGISTER_TYPE_INFO {
			guidMajorType: MFMediaType_Video,
			guidSubtype: MFVideoFormat_NV12,
		};

		let mut activates: *mut Option<IMFActivate> = ptr::null_mut();
		let mut count = 0u32;
		unsafe {
			MFTEnumEx(
				MFT_CATEGORY_VIDEO_DECODER,
				MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER,
				Some(&input),
				Some(&output),
				&mut activates,
				&mut count,
			)
			.context("MFTEnumEx")?;
		}
		if count == 0 {
			bail!("no H.264 decoder MFT on this machine");
		}

		let entries = unsafe { std::slice::from_raw_parts_mut(activates, count as usize) };
		let mut transform = None;
		for slot in entries.iter_mut() {
			let Some(activate) = slot.take() else { continue };
			if transform.is_none()
				&& let Ok(mft) = unsafe { activate.ActivateObject::<IMFTransform>() }
			{
				transform = Some(mft);
			}
		}
		unsafe { CoTaskMemFree(Some(activates as *const c_void)) };
		transform.ok_or_else(|| anyhow!("failed to activate a decoder MFT"))
	}

	// ------------------------------------------------------------ test stream

	/// `FRAMES` access units of a gradient, encoded by openh264 (Annex-B with
	/// inline parameter sets, which is what the MFT wants).
	fn stream() -> Result<Vec<Vec<u8>>> {
		let mut config = moq_video::encode::Config::new(SOURCE.width, SOURCE.height, 30);
		config.kind = moq_video::encode::Kind::Software;
		let mut encoder = moq_video::encode::Encoder::new(&config).context("openh264 encoder")?;

		let mut out = Vec::new();
		for index in 0..FRAMES {
			if index == 0 {
				encoder.keyframe();
			}
			let frame = Frame::new(
				Surface::I420(gradient(index)),
				moq_net::Timestamp::from_micros(index * 33_333).unwrap(),
			);
			for encoded in encoder.encode(&frame).context("encode")? {
				out.push(encoded.payload.to_vec());
			}
		}
		Ok(out)
	}

	/// A gradient that shifts with the frame index, so a scaled result is checkable
	/// and two frames are never identical.
	fn gradient(index: u64) -> I420 {
		let (w, h) = (SOURCE.width as usize, SOURCE.height as usize);
		let mut data = vec![128u8; I420::len(SOURCE.width, SOURCE.height)];
		for y in 0..h {
			for x in 0..w {
				data[y * w + x] = ((x * 255 / w + y * 255 / h) / 2 + index as usize * 4) as u8;
			}
		}
		I420::new(SOURCE.width, SOURCE.height, data).expect("gradient")
	}

	/// Mean absolute error between two planes, the same check the CUDA and
	/// pixel-buffer resize tests use.
	fn mae(a: &[u8], b: &[u8]) -> u64 {
		a.iter().zip(b).map(|(&x, &y)| x.abs_diff(y) as u64).sum::<u64>() / a.len() as u64
	}

	/// Compare a scaled texture against a CPU downscale of the same source, so a
	/// mode that does not wedge still says whether it produced a picture.
	fn report(device: &ID3D11Device, scaled: &ID3D11Texture2D, source: &I420) -> Result<()> {
		let got = download_i420(device, scaled, TARGET)?;
		let expected = Frame::new(Surface::I420(source.clone()), moq_net::Timestamp::ZERO)
			.resize(TARGET)
			.context("cpu resize")?
			.surface
			.into_i420()
			.context("cpu resize download")?;
		eprintln!(
			"    scaled {}x{} -> {}x{}, luma MAE vs the CPU resize: {}",
			SOURCE.width,
			SOURCE.height,
			TARGET.width,
			TARGET.height,
			mae(got.y(), expected.y())
		);
		Ok(())
	}

	// ------------------------------------------------------------------ modes

	/// Which bind flags a video processor will accept as input, and whether the
	/// combination the crate's own `bind_flags` picks is among them.
	///
	/// Not a deadlock question, but the fix depends on the answer: a decoded
	/// texture that cannot be an input view has to be reallocated or re-bound
	/// before any of this matters.
	fn mode_bindflags(debug_layer: bool) -> Result<()> {
		eprintln!("[bindflags] which input bind flags a video processor accepts");
		let device = create_device(debug_layer)?;
		let source = gradient(0);
		let scaler = Scaler::new(&device, SOURCE, TARGET)?;

		let shader = D3D11_BIND_SHADER_RESOURCE.0 as u32;
		let target = D3D11_BIND_RENDER_TARGET.0 as u32;
		let encoder = D3D11_BIND_VIDEO_ENCODER.0 as u32;
		let decoder = D3D11_BIND_DECODER.0 as u32;
		let candidates = [
			("none", 0),
			("SHADER_RESOURCE", shader),
			("RENDER_TARGET", target),
			("VIDEO_ENCODER", encoder),
			("DECODER", decoder),
			("SHADER_RESOURCE | RENDER_TARGET", shader | target),
			("RENDER_TARGET | VIDEO_ENCODER", target | encoder),
			// What frame::d3d11::bind_flags asks for today, driver permitting.
			(
				"SHADER_RESOURCE | RENDER_TARGET | VIDEO_ENCODER (the crate's)",
				shader | target | encoder,
			),
		];

		for (label, bind) in candidates {
			let outcome =
				upload_nv12(&device, &source, bind).and_then(|texture| scaler.input_view(&texture).map(|_| ()));
			match outcome {
				Ok(()) => eprintln!("    accepted: {label}"),
				Err(e) => eprintln!("    rejected: {label} ({})", e.root_cause()),
			}
		}
		Ok(())
	}

	fn mode_cold(debug_layer: bool) -> Result<()> {
		eprintln!("[cold] scaler on a device with no decoder bound");
		let device = create_device(debug_layer)?;
		let source = gradient(0);
		let texture = upload_nv12(&device, &source, D3D11_BIND_RENDER_TARGET.0 as u32)?;
		let scaler = Scaler::new(&device, SOURCE, TARGET)?;
		let scaled = scaler.scale(&device, &texture, SOURCE, TARGET)?;
		report(&device, &scaled, &source)
	}

	fn mode_warm(debug_layer: bool) -> Result<()> {
		eprintln!("[warm] scaler built and blitted before the decoder binds");
		let device = create_device(debug_layer)?;
		let source = gradient(0);
		let warmup = upload_nv12(&device, &source, D3D11_BIND_RENDER_TARGET.0 as u32)?;
		let scaler = Scaler::new(&device, SOURCE, TARGET)?;
		scaler.scale(&device, &warmup, SOURCE, TARGET)?;
		eprintln!("[warm] scaler is warm; binding the decoder now");

		let mut decoder = Decoder::open(&device)?;
		let decoded = decode_all(&mut decoder)?;
		eprintln!("[warm] decoded {} frames; scaling one", decoded.len());
		let scaled = scaler.scale(&device, &decoded[0], SOURCE, TARGET)?;
		report(&device, &scaled, &source)
	}

	fn mode_live(debug_layer: bool) -> Result<()> {
		eprintln!("[live] decoder bound and decoding before the scaler is built");
		let device = create_device(debug_layer)?;
		let mut decoder = Decoder::open(&device)?;
		let decoded = decode_all(&mut decoder)?;
		eprintln!("[live] decoded {} frames; building the scaler now", decoded.len());

		let scaler = Scaler::new(&device, SOURCE, TARGET)?;
		let scaled = scaler.scale(&device, &decoded[0], SOURCE, TARGET)?;
		report(&device, &scaled, &gradient(0))
	}

	/// The `moq-transcode` shape: decode on one thread while a scaler runs on
	/// another, both against the one device the decoder owns.
	fn mode_concurrent(debug_layer: bool) -> Result<()> {
		eprintln!("[concurrent] decode loop and scaler on separate threads, one device");
		let device = create_device(debug_layer)?;
		let mut decoder = Decoder::open(&device)?;
		let first = decode_all(&mut decoder)?;

		let stop = &AtomicBool::new(false);
		std::thread::scope(|scope| -> Result<()> {
			let decode_device = device.clone();
			scope.spawn(move || {
				// Its own COM/MF lifetime: the guard is per thread.
				let _com = ComGuard::new().expect("decode thread COM");
				let mut decoder = decoder;
				let units = stream().expect("test stream");
				while !stop.load(Ordering::Relaxed) {
					// A fresh session per pass, so the decoder keeps hitting the
					// device rather than idling once the stream runs out.
					for unit in &units {
						if stop.load(Ordering::Relaxed) {
							break;
						}
						if let Err(e) = decoder.decode(unit) {
							eprintln!("[concurrent] decode thread stopped: {e}");
							return;
						}
					}
					match Decoder::open(&decode_device) {
						Ok(next) => decoder = next,
						Err(e) => {
							eprintln!("[concurrent] could not reopen the decoder: {e}");
							return;
						}
					}
				}
			});

			eprintln!("[concurrent] decode is running; building the scaler now");
			let result = (|| -> Result<()> {
				let scaler = Scaler::new(&device, SOURCE, TARGET)?;
				for pass in 0..8 {
					eprintln!("[concurrent] scale pass {pass}");
					scaler.scale(&device, &first[0], SOURCE, TARGET)?;
				}
				Ok(())
			})();
			stop.store(true, Ordering::Relaxed);
			result
		})
	}

	/// The same ordering as `live`, but the pictures come from the crate's own
	/// decoder, so the probe's minimal MFT can be checked against the real one.
	fn mode_crate(debug_layer: bool) -> Result<()> {
		let _ = debug_layer;
		eprintln!("[crate] decode via moq_video::decode::Decoder, then scale its device");

		let mut catalog = hang::catalog::VideoConfig::new(hang::catalog::H264 {
			inline: true,
			profile: 0x42,
			constraints: 0,
			level: 0x1f,
		});
		catalog.coded_width = Some(SOURCE.width);
		catalog.coded_height = Some(SOURCE.height);

		let mut config = moq_video::decode::Config::new();
		config.kind = moq_video::decode::Kind::Named("mediafoundation".into());
		let mut decoder = moq_video::decode::Decoder::new(&catalog, &config).context("Media Foundation decoder")?;

		let mut frames = Vec::new();
		for (index, unit) in stream()?.into_iter().enumerate() {
			let timestamp = moq_net::Timestamp::from_micros(index as u64 * 33_333).unwrap();
			frames.extend(decoder.decode(&unit.into(), timestamp, index == 0).context("decode")?);
		}
		let Some(Surface::Texture(texture)) = frames.first().map(|f| &f.surface) else {
			bail!("the crate's decoder did not produce a GPU texture on this machine");
		};
		eprintln!("[crate] decoded {} frames; building the scaler now", frames.len());

		let device = texture.device().clone();
		let scaler = Scaler::new(&device, SOURCE, TARGET)?;
		let scaled = scaler.scale(&device, texture.texture(), SOURCE, TARGET)?;
		report(&device, &scaled, &gradient(0))
	}

	/// The full `moq-transcode` shape: one DXVA decoder, `RUNGS` hardware encoder
	/// MFTs, and a scaler, all on the single device the decoded textures belong
	/// to, all on separate threads.
	///
	/// The encoders are the ingredient the other modes lack. `encode::Encoder`
	/// binds the device of the texture it is handed, so a ladder puts one decode
	/// session and one encode session per rung on the same device, and every one
	/// of them wants the GPU's video engine.
	fn mode_ladder(debug_layer: bool) -> Result<()> {
		const RUNGS: usize = 4;
		const PASSES: usize = 200;
		let _ = debug_layer;
		eprintln!("[ladder] one decoder + {RUNGS} hardware encoders + a scaler on one device");

		let mut catalog = hang::catalog::VideoConfig::new(hang::catalog::H264 {
			inline: true,
			profile: 0x42,
			constraints: 0,
			level: 0x1f,
		});
		catalog.coded_width = Some(SOURCE.width);
		catalog.coded_height = Some(SOURCE.height);

		let mut config = moq_video::decode::Config::new();
		config.kind = moq_video::decode::Kind::Named("mediafoundation".into());
		let mut decoder = moq_video::decode::Decoder::new(&catalog, &config).context("Media Foundation decoder")?;

		let mut frames = Vec::new();
		for (index, unit) in stream()?.into_iter().enumerate() {
			let timestamp = moq_net::Timestamp::from_micros(index as u64 * 33_333).unwrap();
			frames.extend(decoder.decode(&unit.into(), timestamp, index == 0).context("decode")?);
		}
		let Some(Surface::Texture(first)) = frames.first().map(|f| &f.surface) else {
			bail!("the decoder did not produce a GPU texture on this machine");
		};
		let device = first.device().clone();
		let source = first.texture().clone();
		eprintln!("[ladder] decoded {} frames; starting the rungs", frames.len());

		let stop = &AtomicBool::new(false);
		let frames = &frames;
		std::thread::scope(|scope| -> Result<()> {
			for rung in 0..RUNGS {
				scope.spawn(move || {
					let _com = match ComGuard::new() {
						Ok(com) => com,
						Err(e) => return eprintln!("[ladder] rung {rung} has no COM: {e}"),
					};
					let mut config = moq_video::encode::Config::new(SOURCE.width, SOURCE.height, 30);
					config.kind = moq_video::encode::Kind::Named("mediafoundation".into());
					let mut encoder = match moq_video::encode::Encoder::new(&config) {
						Ok(encoder) => encoder,
						Err(e) => return eprintln!("[ladder] rung {rung} has no hardware encoder: {e}"),
					};
					while !stop.load(Ordering::Relaxed) {
						for frame in frames {
							if stop.load(Ordering::Relaxed) {
								return;
							}
							if let Err(e) = encoder.encode(frame) {
								return eprintln!("[ladder] rung {rung} stopped encoding: {e}");
							}
						}
					}
				});
			}

			// Let the encoders bind the device and get busy before the scaler
			// shows up, so the blit lands mid-session rather than before one.
			std::thread::sleep(Duration::from_millis(500));
			eprintln!("[ladder] rungs are encoding; building the scaler now");

			let result = (|| -> Result<()> {
				let scaler = Scaler::new(&device, SOURCE, TARGET)?;
				for pass in 0..PASSES {
					if pass % 25 == 0 {
						eprintln!("[ladder] scale pass {pass}/{PASSES}");
					}
					scaler.scale(&device, &source, SOURCE, TARGET)?;
				}
				Ok(())
			})();
			stop.store(true, Ordering::Relaxed);
			result
		})
	}

	fn decode_all(decoder: &mut Decoder) -> Result<Vec<ID3D11Texture2D>> {
		let mut out = Vec::new();
		for unit in stream()? {
			out.extend(decoder.decode(&unit)?);
		}
		if out.is_empty() {
			bail!("the decoder produced no frames");
		}
		Ok(out)
	}

	// ------------------------------------------------------------------- main

	pub fn main() -> Result<()> {
		let args: Vec<String> = std::env::args().skip(1).collect();
		let debug_layer = args.iter().any(|a| a == "--debug-layer");
		let mode = args.iter().find(|a| !a.starts_with("--")).cloned();

		eprintln!(
			"pid {} (attach with: procdump -ma {})",
			std::process::id(),
			std::process::id()
		);
		watchdog();
		let _com = ComGuard::new()?;

		match mode.as_deref() {
			Some("bindflags") => mode_bindflags(debug_layer),
			Some("cold") => mode_cold(debug_layer),
			Some("warm") => mode_warm(debug_layer),
			Some("live") => mode_live(debug_layer),
			Some("concurrent") => mode_concurrent(debug_layer),
			Some("crate") => mode_crate(debug_layer),
			Some("ladder") => mode_ladder(debug_layer),
			other => {
				if let Some(other) = other {
					eprintln!("unknown mode: {other}");
				}
				eprintln!(
					"usage: d3d11-resize-probe <bindflags|cold|warm|live|concurrent|crate|ladder> [--debug-layer]"
				);
				eprintln!("run one mode per process; a wedge leaves the device unusable");
				std::process::exit(2);
			}
		}
	}
}
