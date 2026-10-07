//! CUDA imports and completion for neutral external Vulkan slots.
use super::vulkan::{self, Channels, Handles, Image, uuid};
use crate::{Error, Size};
use cudarc::driver::sys;
use cudarc::driver::{CudaContext, CudaStream};
use std::fmt;
use std::os::fd::AsRawFd;
use std::sync::{Arc, Mutex, mpsc};

/// Imports producer-owned Vulkan image slots into one CUDA device.
#[derive(Clone)]
pub(crate) struct Importer {
	backend: Arc<Backend>,
}

impl fmt::Debug for Importer {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Importer")
			.field("device", &self.backend.ctx.ordinal())
			.finish_non_exhaustive()
	}
}

impl Importer {
	/// Open CUDA device `ordinal` and its completion worker.
	pub(crate) fn new(ordinal: usize) -> Result<Self, Error> {
		let ctx = CudaContext::new(ordinal)
			.map_err(|e| Error::Unsupported(format!("CUDA device {ordinal} is unavailable: {e:?}")))?;
		let (reap, receiver) = mpsc::channel::<Reap>();
		let thread = std::thread::Builder::new()
			.name("moq-video-vulkan-cuda".into())
			.spawn(move || {
				while let Ok(job) = receiver.recv() {
					let Reap::Synchronize(stream, completion, signal_queued) = job;
					// A device loss returns an error here instead of stranding the
					// producer. This is the dedicated completion worker, never the
					// producer's render thread. A failed slot is released instead of
					// being reused with an unsignalled Vulkan semaphore.
					let reusable = signal_queued && stream.synchronize().is_ok();
					completion.complete(reusable);
				}
			})
			.map_err(|e| Error::Codec(anyhow::anyhow!("start Vulkan/CUDA completion worker: {e}")))?;
		let thread_id = thread.thread().id();
		Ok(Self {
			backend: Arc::new(Backend {
				ctx,
				reap: Mutex::new(Some(reap)),
				thread: Mutex::new(Some((thread_id, thread))),
			}),
		})
	}

	pub(crate) fn import(&self, frame: &vulkan::Frame) -> Result<Frame, Error> {
		let imported = frame.import(|| Imported::new(self.backend.clone(), frame.handles()?, frame.image().clone()))?;
		frame.read(imported.clone())?;
		Ok(Frame {
			imported,
			_frame: frame.clone(),
		})
	}
}

struct Backend {
	ctx: Arc<CudaContext>,
	reap: Mutex<Option<mpsc::Sender<Reap>>>,
	thread: Mutex<Option<(std::thread::ThreadId, std::thread::JoinHandle<()>)>>,
}

impl Backend {
	fn send(&self, job: Reap) {
		if let Some(sender) = self.reap.lock().expect("completion sender poisoned").as_ref() {
			// A job owns a Slot, which owns this Backend, so the receiver cannot
			// disappear before this send.
			let _ = sender.send(job);
		}
	}
}

impl Drop for Backend {
	fn drop(&mut self) {
		self.reap.lock().expect("completion sender poisoned").take();
		if let Some((thread_id, thread)) = self.thread.lock().expect("completion worker poisoned").take()
			&& std::thread::current().id() != thread_id
		{
			let _ = thread.join();
		}
	}
}

enum Reap {
	Synchronize(Arc<CudaStream>, Box<dyn vulkan::Complete>, bool),
}

/// One imported image retained alongside its published producer frame.
pub(crate) struct Frame {
	imported: Arc<Imported>,
	_frame: vulkan::Frame,
}
impl Frame {
	/// Image width in pixels.
	#[cfg(test)]
	pub(crate) fn width(&self) -> u32 {
		self.imported.image.size.width
	}

	/// Image height in pixels.
	#[cfg(test)]
	pub(crate) fn height(&self) -> u32 {
		self.imported.image.size.height
	}

	/// Image size in pixels.
	pub(crate) fn size(&self) -> Size {
		self.imported.image.size
	}

	/// The channel order the producer declared when importing the image.
	pub(crate) fn channels(&self) -> Channels {
		self.imported.image.channels
	}

	/// The level-0 array behind the surface, for the tests' readback only.
	#[cfg(test)]
	pub(crate) fn cuda_array(&self) -> sys::CUarray {
		let mut array = std::ptr::null_mut();
		// SAFETY: the imported image has exactly one mip level and is alive.
		unsafe { sys::cuMipmappedArrayGetLevel(&mut array, self.imported.mipmap, 0) }
			.result()
			.expect("Vulkan image mip level");
		array
	}

	/// The surface object over the image, for a kernel reading it in place.
	pub(crate) fn cuda_surface(&self) -> sys::CUsurfObject {
		self.imported.surface
	}

	/// The stream every CUDA reader of this image queues on, so the completion
	/// signal queued after the last reader lands behind their work.
	pub(crate) fn cuda_stream(&self) -> &Arc<CudaStream> {
		&self.imported.stream
	}

	/// The context that owns the imported image.
	pub(crate) fn cuda_context(&self) -> &Arc<CudaContext> {
		&self.imported.backend.ctx
	}
}

impl vulkan::Reader for Imported {
	fn wait(&self, value: u64) -> Result<(), Error> {
		self.wait(value)
	}
	fn finish(&self, value: u64, completion: Box<dyn vulkan::Complete>) {
		let signal_queued = self.signal(value).is_ok();
		self.backend
			.send(Reap::Synchronize(self.stream.clone(), completion, signal_queued));
	}
}
pub(crate) struct Imported {
	backend: Arc<Backend>,
	stream: Arc<CudaStream>,
	image: Image,
	memory: sys::CUexternalMemory,
	semaphore: sys::CUexternalSemaphore,
	mipmap: sys::CUmipmappedArray,
	surface: sys::CUsurfObject,
}

// CUDA's external handles are explicitly safe to use from threads after making
// their context current. `CudaContext` and `CudaStream` already carry the same
// guarantees; cudarc's raw pointer aliases cannot express them.
unsafe impl Send for Imported {}
unsafe impl Sync for Imported {}

impl Imported {
	fn new(backend: Arc<Backend>, handles: Handles, image: Image) -> Result<Self, Error> {
		if !matches!(&image.memory, vulkan::Memory::OpaqueFd { .. }) {
			return Err(Error::Unsupported("CUDA accepts only OPAQUE_FD Vulkan images".into()));
		}
		backend.ctx.bind_to_thread().map_err(cuda("bind CUDA context"))?;
		let actual = backend.ctx.uuid().map_err(cuda("read CUDA device UUID"))?;
		// `CUuuid.bytes` is `[c_char; 16]`, which is `i8` on x86_64 and `u8` on
		// aarch64. Reinterpret per element: an `as u8` cast fails clippy's
		// `unnecessary_cast` where `c_char` is already `u8`.
		let actual = actual.bytes.map(|b| u8::from_ne_bytes(b.to_ne_bytes()));
		if actual != image.device.device_uuid {
			return Err(Error::Unsupported(format!(
				"Vulkan device UUID {} does not match CUDA device UUID {}",
				uuid(image.device.device_uuid),
				uuid(actual)
			)));
		}

		let stream = backend.ctx.new_stream().map_err(cuda("create CUDA stream"))?;
		let memory_desc = sys::CUDA_EXTERNAL_MEMORY_HANDLE_DESC {
			type_: sys::CUexternalMemoryHandleType::CU_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD,
			handle: sys::CUDA_EXTERNAL_MEMORY_HANDLE_DESC_st__bindgen_ty_1 {
				fd: handles.memory.as_raw_fd(),
			},
			size: image.allocation_size,
			flags: 1, // CUDA_EXTERNAL_MEMORY_DEDICATED
			reserved: [0; 16],
		};
		let mut memory = std::ptr::null_mut();
		// SAFETY: the descriptor names a live owned fd and its exact allocation
		// size. CUDA takes fd ownership only after successful import.
		unsafe { sys::cuImportExternalMemory(&mut memory, &memory_desc) }
			.result()
			.map_err(cuda("import Vulkan image memory"))?;
		std::mem::forget(handles.memory);

		let semaphore_desc = sys::CUDA_EXTERNAL_SEMAPHORE_HANDLE_DESC {
			type_: sys::CUexternalSemaphoreHandleType::CU_EXTERNAL_SEMAPHORE_HANDLE_TYPE_TIMELINE_SEMAPHORE_FD,
			handle: sys::CUDA_EXTERNAL_SEMAPHORE_HANDLE_DESC_st__bindgen_ty_1 {
				fd: handles.timeline.as_raw_fd(),
			},
			flags: 0,
			reserved: [0; 16],
		};
		let mut semaphore = std::ptr::null_mut();
		// SAFETY: the descriptor names a Vulkan timeline semaphore exported as an
		// opaque fd. CUDA takes fd ownership only after successful import.
		if let Err(error) = unsafe { sys::cuImportExternalSemaphore(&mut semaphore, &semaphore_desc) }.result() {
			// SAFETY: memory was imported above and has no mappings yet.
			let _ = unsafe { sys::cuDestroyExternalMemory(memory) };
			return Err(cuda("import Vulkan timeline semaphore")(error));
		}
		std::mem::forget(handles.timeline);

		let array_desc = sys::CUDA_EXTERNAL_MEMORY_MIPMAPPED_ARRAY_DESC {
			offset: 0,
			arrayDesc: sys::CUDA_ARRAY3D_DESCRIPTOR {
				Width: image.size.width as usize,
				Height: image.size.height as usize,
				Depth: 0,
				Format: sys::CUarray_format::CU_AD_FORMAT_UNSIGNED_INT8,
				NumChannels: 4,
				Flags: sys::CUDA_ARRAY3D_SURFACE_LDST,
			},
			numLevels: 1,
			reserved: [0; 16],
		};
		let mut mipmap = std::ptr::null_mut();
		// SAFETY: the Vulkan allocation is dedicated to an optimal-tiling RGBA8
		// image matching this descriptor.
		if let Err(error) =
			unsafe { sys::cuExternalMemoryGetMappedMipmappedArray(&mut mipmap, memory, &array_desc) }.result()
		{
			// SAFETY: both handles were imported and no work references them.
			let _ = unsafe { sys::cuDestroyExternalSemaphore(semaphore) };
			let _ = unsafe { sys::cuDestroyExternalMemory(memory) };
			return Err(cuda("map Vulkan image in CUDA")(error));
		}

		let mut array = std::ptr::null_mut();
		// SAFETY: the imported image has exactly one mip level.
		if let Err(error) = unsafe { sys::cuMipmappedArrayGetLevel(&mut array, mipmap, 0) }.result() {
			// SAFETY: no work references these newly-created handles.
			let _ = unsafe { sys::cuMipmappedArrayDestroy(mipmap) };
			let _ = unsafe { sys::cuDestroyExternalSemaphore(semaphore) };
			let _ = unsafe { sys::cuDestroyExternalMemory(memory) };
			return Err(cuda("get Vulkan image mip level")(error));
		}

		// A surface object is how a kernel reads the array in place; the array
		// was mapped with `CUDA_ARRAY3D_SURFACE_LDST` for exactly this.
		let surface_desc = sys::CUDA_RESOURCE_DESC {
			resType: sys::CUresourcetype::CU_RESOURCE_TYPE_ARRAY,
			res: sys::CUDA_RESOURCE_DESC_st__bindgen_ty_1 {
				array: sys::CUDA_RESOURCE_DESC_st__bindgen_ty_1__bindgen_ty_1 { hArray: array },
			},
			flags: 0,
		};
		let mut surface = 0;
		// SAFETY: the descriptor names the live level-0 array mapped above.
		if let Err(error) = unsafe { sys::cuSurfObjectCreate(&mut surface, &surface_desc) }.result() {
			// SAFETY: no work references these newly-created handles.
			let _ = unsafe { sys::cuMipmappedArrayDestroy(mipmap) };
			let _ = unsafe { sys::cuDestroyExternalSemaphore(semaphore) };
			let _ = unsafe { sys::cuDestroyExternalMemory(memory) };
			return Err(cuda("create surface over Vulkan image")(error));
		}

		Ok(Self {
			backend,
			stream,
			image,
			memory,
			semaphore,
			mipmap,
			surface,
		})
	}

	fn wait(&self, value: u64) -> Result<(), Error> {
		self.backend.ctx.bind_to_thread().map_err(cuda("bind CUDA context"))?;
		let params = timeline_wait(value);
		// SAFETY: both semaphore and stream are live and belong to this context.
		unsafe { sys::cuWaitExternalSemaphoresAsync(&self.semaphore, &params, 1, self.stream.cu_stream()) }
			.result()
			.map_err(cuda("queue Vulkan timeline wait"))
	}

	fn signal(&self, value: u64) -> Result<(), Error> {
		self.backend.ctx.bind_to_thread().map_err(cuda("bind CUDA context"))?;
		let params = timeline_signal(value);
		// SAFETY: both semaphore and stream are live and belong to this context.
		unsafe { sys::cuSignalExternalSemaphoresAsync(&self.semaphore, &params, 1, self.stream.cu_stream()) }
			.result()
			.map_err(cuda("queue Vulkan timeline signal"))
	}
}

impl Drop for Imported {
	fn drop(&mut self) {
		if self.backend.ctx.bind_to_thread().is_ok() {
			// Completion returned the slot only after stream synchronization, so no
			// queued work can still reference these handles.
			let _ = unsafe { sys::cuSurfObjectDestroy(self.surface) };
			let _ = unsafe { sys::cuMipmappedArrayDestroy(self.mipmap) };
			let _ = unsafe { sys::cuDestroyExternalMemory(self.memory) };
			let _ = unsafe { sys::cuDestroyExternalSemaphore(self.semaphore) };
		}
	}
}

fn timeline_wait(value: u64) -> sys::CUDA_EXTERNAL_SEMAPHORE_WAIT_PARAMS {
	sys::CUDA_EXTERNAL_SEMAPHORE_WAIT_PARAMS {
		params: sys::CUDA_EXTERNAL_SEMAPHORE_WAIT_PARAMS_st__bindgen_ty_1 {
			fence: sys::CUDA_EXTERNAL_SEMAPHORE_WAIT_PARAMS_st__bindgen_ty_1__bindgen_ty_1 { value },
			nvSciSync: sys::CUDA_EXTERNAL_SEMAPHORE_WAIT_PARAMS_st__bindgen_ty_1__bindgen_ty_2 { reserved: 0 },
			keyedMutex: sys::CUDA_EXTERNAL_SEMAPHORE_WAIT_PARAMS_st__bindgen_ty_1__bindgen_ty_3 {
				key: 0,
				timeoutMs: 0,
			},
			reserved: [0; 10],
		},
		flags: 0,
		reserved: [0; 16],
	}
}

fn timeline_signal(value: u64) -> sys::CUDA_EXTERNAL_SEMAPHORE_SIGNAL_PARAMS {
	sys::CUDA_EXTERNAL_SEMAPHORE_SIGNAL_PARAMS {
		params: sys::CUDA_EXTERNAL_SEMAPHORE_SIGNAL_PARAMS_st__bindgen_ty_1 {
			fence: sys::CUDA_EXTERNAL_SEMAPHORE_SIGNAL_PARAMS_st__bindgen_ty_1__bindgen_ty_1 { value },
			nvSciSync: sys::CUDA_EXTERNAL_SEMAPHORE_SIGNAL_PARAMS_st__bindgen_ty_1__bindgen_ty_2 { reserved: 0 },
			keyedMutex: sys::CUDA_EXTERNAL_SEMAPHORE_SIGNAL_PARAMS_st__bindgen_ty_1__bindgen_ty_3 { key: 0 },
			reserved: [0; 12],
		},
		flags: 0,
		reserved: [0; 16],
	}
}

fn cuda(action: &'static str) -> impl FnOnce(cudarc::driver::result::DriverError) -> Error {
	move |error| Error::Codec(anyhow::anyhow!("{action}: {error:?}"))
}
