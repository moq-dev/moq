//! Vulkan images imported into CUDA on Linux/NVIDIA.
//!
//! A [`Slot`] is an exportable Vulkan allocation imported once. Publishing it
//! consumes the slot and [`Completion::wait`] returns it only after every CUDA
//! operation queued through the frame's stream has finished. A producer cannot
//! accidentally overwrite an image while a consumer still reads it.
//!
//! The image is packed RGBA or BGRA, which no encoder takes: a
//! [`cuda::Converter`](super::cuda::Converter) turns a published [`Frame`] into
//! the NV12 [`cuda::Frame`](super::cuda::Frame) NVENC encodes in place.

use std::fmt;
use std::num::NonZeroUsize;
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use cudarc::driver::sys;
use cudarc::driver::{CudaContext, CudaStream};

use crate::{Error, Size};

/// The Vulkan handles exported for one reusable image slot.
pub struct Handles {
	/// A dedicated `VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT` allocation.
	pub memory: OwnedFd,
	/// A `VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_OPAQUE_FD_BIT` timeline semaphore.
	pub timeline: OwnedFd,
}

impl Handles {
	/// Group the exported memory and timeline semaphore handles for an image.
	pub fn new(memory: OwnedFd, timeline: OwnedFd) -> Self {
		Self { memory, timeline }
	}
}

/// The byte order of an imported image's four 8-bit channels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channels {
	/// `VK_FORMAT_R8G8B8A8_UNORM`.
	Rgba,
	/// `VK_FORMAT_B8G8R8A8_UNORM`, what a swapchain or an Unreal render target
	/// usually holds.
	Bgra,
}

/// The Vulkan image contract accepted by CUDA.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Image {
	device_uuid: [u8; 16],
	size: Size,
	allocation_size: u64,
	channels: Channels,
}

impl Image {
	/// Describe a dedicated optimal-tiling `VK_FORMAT_R8G8B8A8_UNORM` image.
	///
	/// The image must be in `VK_IMAGE_LAYOUT_GENERAL` while CUDA owns the slot.
	/// `allocation_size` is the complete `VkDeviceMemory` allocation size from
	/// Vulkan, not `width * height * 4`.
	pub fn rgba8(device_uuid: [u8; 16], size: Size, allocation_size: u64) -> Result<Self, Error> {
		Self::new(device_uuid, size, allocation_size, Channels::Rgba)
	}

	/// Describe a dedicated optimal-tiling `VK_FORMAT_B8G8R8A8_UNORM` image,
	/// under the same contract as [`rgba8`](Self::rgba8).
	pub fn bgra8(device_uuid: [u8; 16], size: Size, allocation_size: u64) -> Result<Self, Error> {
		Self::new(device_uuid, size, allocation_size, Channels::Bgra)
	}

	fn new(device_uuid: [u8; 16], size: Size, allocation_size: u64, channels: Channels) -> Result<Self, Error> {
		size.validate_nonzero("Vulkan/CUDA image")?;
		if allocation_size == 0 {
			return Err(Error::Unsupported(
				"Vulkan/CUDA allocation size must be non-zero".into(),
			));
		}
		Ok(Self {
			device_uuid,
			size,
			allocation_size,
			channels,
		})
	}

	/// Vulkan physical-device UUID required to match the CUDA device.
	pub const fn device_uuid(&self) -> [u8; 16] {
		self.device_uuid
	}

	/// Visible image size.
	pub const fn size(&self) -> Size {
		self.size
	}

	/// Complete size of the dedicated Vulkan memory allocation.
	pub const fn allocation_size(&self) -> u64 {
		self.allocation_size
	}

	/// The channel order of the image's pixels.
	pub const fn channels(&self) -> Channels {
		self.channels
	}
}

/// Monotonic values for one Vulkan-to-CUDA-to-Vulkan handoff.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timeline {
	/// Value Vulkan signals after finishing writes and the layout transition.
	pub ready: u64,
	/// Value CUDA signals after the last queued reader finishes.
	pub complete: u64,
}

impl Timeline {
	/// Create one handoff. `complete` must be greater than `ready`.
	pub fn new(ready: u64, complete: u64) -> Result<Self, Error> {
		if complete <= ready {
			return Err(Error::Unsupported(format!(
				"Vulkan/CUDA completion value {complete} must be greater than ready value {ready}"
			)));
		}
		Ok(Self { ready, complete })
	}
}

/// Imports a bounded number of reusable Vulkan image slots into one CUDA device.
#[derive(Clone)]
pub struct Importer {
	backend: Arc<Backend>,
}

impl fmt::Debug for Importer {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Importer")
			.field("device", &self.backend.ctx.ordinal())
			.field("capacity", &self.backend.capacity)
			.finish_non_exhaustive()
	}
}

impl Importer {
	/// Open CUDA device `ordinal` and cap the number of imported image slots.
	pub fn new(ordinal: usize, capacity: NonZeroUsize) -> Result<Self, Error> {
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
				capacity: capacity.get(),
				imported: AtomicUsize::new(0),
				reap: Mutex::new(Some(reap)),
				thread: Mutex::new(Some((thread_id, thread))),
			}),
		})
	}

	/// Import an exportable Vulkan image and attach its producer-owned slot.
	///
	/// `owner` is returned inside [`Slot`] and remains retained until CUDA
	/// completion. If import fails, it is returned in [`ImportError`]. The opaque
	/// FDs are consumed by CUDA on success and closed on failure.
	pub fn import<T: Send + Sync + 'static>(
		&self,
		handles: Handles,
		image: Image,
		owner: T,
	) -> Result<Slot<T>, ImportError<T>> {
		if self
			.backend
			.imported
			.fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
				(count < self.backend.capacity).then_some(count + 1)
			})
			.is_err()
		{
			return Err(ImportError::new(
				Error::Unsupported(format!(
					"Vulkan/CUDA importer capacity {} exhausted",
					self.backend.capacity
				)),
				owner,
			));
		}

		match Imported::new(self.backend.clone(), handles, image) {
			Ok(imported) => Ok(Slot {
				imported: Arc::new(imported),
				owner,
				last_complete: 0,
			}),
			Err(error) => {
				self.backend.imported.fetch_sub(1, Ordering::AcqRel);
				Err(ImportError::new(error, owner))
			}
		}
	}
}

struct Backend {
	ctx: Arc<CudaContext>,
	capacity: usize,
	imported: AtomicUsize,
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
	Synchronize(Arc<CudaStream>, Box<dyn Complete>, bool),
}

trait Complete: Send + Sync {
	fn complete(self: Box<Self>, reusable: bool);
}

struct ReturnSlot<T: Send + Sync + 'static> {
	slot: Slot<T>,
	sender: tokio::sync::oneshot::Sender<Slot<T>>,
}

impl<T: Send + Sync + 'static> Complete for ReturnSlot<T> {
	fn complete(self: Box<Self>, reusable: bool) {
		let Self { slot, sender } = *self;
		if reusable {
			let _ = sender.send(slot);
		}
	}
}

/// One imported Vulkan image plus its producer-owned slot.
pub struct Slot<T: Send + Sync + 'static> {
	imported: Arc<Imported>,
	owner: T,
	last_complete: u64,
}

impl<T: Send + Sync + 'static> fmt::Debug for Slot<T> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Slot")
			.field("image", &self.imported.image)
			.field("last_complete", &self.last_complete)
			.finish_non_exhaustive()
	}
}

impl<T: Send + Sync + 'static> Slot<T> {
	/// Access the producer-owned value while the slot is idle.
	pub fn owner(&self) -> &T {
		&self.owner
	}

	/// Mutate the producer-owned value while the slot is idle.
	pub fn owner_mut(&mut self) -> &mut T {
		&mut self.owner
	}

	/// Publish this slot after Vulkan has queued its `ready` signal.
	///
	/// CUDA waits asynchronously. Dropping the last [`Frame`] queues the
	/// `complete` signal after all consumer work on the frame stream, then the
	/// completion worker returns this exact slot.
	pub fn publish(self, timeline: Timeline) -> Result<(Frame, Completion<T>), PublishError<T>> {
		if timeline.ready <= self.last_complete {
			return Err(PublishError::new(
				Error::Unsupported(format!(
					"Vulkan/CUDA ready value {} must be greater than previous completion {}",
					timeline.ready, self.last_complete
				)),
				self,
			));
		}

		if let Err(error) = self.imported.wait(timeline.ready) {
			return Err(PublishError::new(error, self));
		}

		let (sender, receiver) = tokio::sync::oneshot::channel();
		let imported = self.imported.clone();
		let completion: Box<dyn Complete> = Box::new(ReturnSlot {
			slot: Slot {
				imported: self.imported,
				owner: self.owner,
				last_complete: timeline.complete,
			},
			sender,
		});
		Ok((
			Frame {
				inner: Arc::new(FrameInner {
					imported,
					complete: timeline.complete,
					completion: Some(completion),
				}),
			},
			Completion { receiver },
		))
	}
}

/// A Vulkan image being read through CUDA.
#[derive(Clone)]
pub struct Frame {
	inner: Arc<FrameInner>,
}

impl fmt::Debug for Frame {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Frame")
			.field("image", &self.inner.imported.image)
			.field("complete", &self.inner.complete)
			.finish_non_exhaustive()
	}
}

impl Frame {
	/// Image width in pixels.
	pub fn width(&self) -> u32 {
		self.inner.imported.image.size.width
	}

	/// Image height in pixels.
	pub fn height(&self) -> u32 {
		self.inner.imported.image.size.height
	}

	/// Image size in pixels.
	pub fn size(&self) -> Size {
		self.inner.imported.image.size
	}

	/// The channel order the producer declared when importing the image.
	pub fn channels(&self) -> Channels {
		self.inner.imported.image.channels
	}

	/// The level-0 array behind the surface, for the tests' readback only.
	#[cfg(test)]
	pub(crate) fn cuda_array(&self) -> sys::CUarray {
		let mut array = std::ptr::null_mut();
		// SAFETY: the imported image has exactly one mip level and is alive.
		unsafe { sys::cuMipmappedArrayGetLevel(&mut array, self.inner.imported.mipmap, 0) }
			.result()
			.expect("Vulkan image mip level");
		array
	}

	/// The surface object over the image, for a kernel reading it in place.
	pub(crate) fn cuda_surface(&self) -> sys::CUsurfObject {
		self.inner.imported.surface
	}

	/// The stream every CUDA reader of this image queues on, so the completion
	/// signal queued after the last reader lands behind their work.
	pub(crate) fn cuda_stream(&self) -> &Arc<CudaStream> {
		&self.inner.imported.stream
	}

	/// The context that owns the imported image.
	pub(crate) fn cuda_context(&self) -> &Arc<CudaContext> {
		&self.inner.imported.backend.ctx
	}
}

struct FrameInner {
	imported: Arc<Imported>,
	complete: u64,
	completion: Option<Box<dyn Complete>>,
}

impl Drop for FrameInner {
	fn drop(&mut self) {
		let completion = self.completion.take().expect("Vulkan/CUDA completion missing");
		// Queueing is asynchronous. A device error releases the slot after the
		// worker drains the stream, but never returns it for unsafe Vulkan reuse.
		let signal_queued = self.imported.signal(self.complete).is_ok();
		self.imported.backend.send(Reap::Synchronize(
			self.imported.stream.clone(),
			completion,
			signal_queued,
		));
	}
}

/// Resolves to the producer slot after CUDA has signalled completion.
pub struct Completion<T: Send + Sync + 'static> {
	receiver: tokio::sync::oneshot::Receiver<Slot<T>>,
}

impl<T: Send + Sync + 'static> Completion<T> {
	/// Return the slot if CUDA has completed, without blocking or polling CUDA.
	pub fn try_wait(&mut self) -> Result<Option<Slot<T>>, CompletionError> {
		match self.receiver.try_recv() {
			Ok(slot) => Ok(Some(slot)),
			Err(tokio::sync::oneshot::error::TryRecvError::Empty) => Ok(None),
			Err(tokio::sync::oneshot::error::TryRecvError::Closed) => Err(CompletionError),
		}
	}

	/// Wait without blocking a thread until the image can be written again.
	pub async fn wait(self) -> Result<Slot<T>, CompletionError> {
		self.receiver.await.map_err(|_| CompletionError)
	}
}

/// CUDA failed before the producer slot became safe to reuse.
#[derive(Clone, Copy, Debug, thiserror::Error)]
#[error("Vulkan/CUDA completion failed; the producer slot was released")]
pub struct CompletionError;

/// An import failure that returns the producer-owned value.
pub struct ImportError<T> {
	error: Error,
	owner: T,
}

impl<T> ImportError<T> {
	fn new(error: Error, owner: T) -> Self {
		Self { error, owner }
	}

	/// Split the error from the producer-owned value.
	pub fn into_parts(self) -> (Error, T) {
		(self.error, self.owner)
	}
}

impl<T> fmt::Debug for ImportError<T> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("ImportError")
			.field("error", &self.error)
			.finish_non_exhaustive()
	}
}

impl<T> fmt::Display for ImportError<T> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		fmt::Display::fmt(&self.error, f)
	}
}

impl<T: 'static> std::error::Error for ImportError<T> {}

/// A publish failure that returns the still-idle slot.
pub struct PublishError<T: Send + Sync + 'static> {
	error: Error,
	slot: Slot<T>,
}

impl<T: Send + Sync + 'static> PublishError<T> {
	fn new(error: Error, slot: Slot<T>) -> Self {
		Self { error, slot }
	}

	/// Split the error from the reusable slot.
	pub fn into_parts(self) -> (Error, Slot<T>) {
		(self.error, self.slot)
	}
}

impl<T: Send + Sync + 'static> fmt::Debug for PublishError<T> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("PublishError")
			.field("error", &self.error)
			.finish_non_exhaustive()
	}
}

impl<T: Send + Sync + 'static> fmt::Display for PublishError<T> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		fmt::Display::fmt(&self.error, f)
	}
}

impl<T: Send + Sync + 'static> std::error::Error for PublishError<T> {}

struct Imported {
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
		backend.ctx.bind_to_thread().map_err(cuda("bind CUDA context"))?;
		let actual = backend.ctx.uuid().map_err(cuda("read CUDA device UUID"))?;
		// `CUuuid.bytes` is `[c_char; 16]`, which is `i8` on x86_64 and `u8` on
		// aarch64. Reinterpret per element: an `as u8` cast fails clippy's
		// `unnecessary_cast` where `c_char` is already `u8`.
		let actual = actual.bytes.map(|b| u8::from_ne_bytes(b.to_ne_bytes()));
		if actual != image.device_uuid {
			return Err(Error::Unsupported(format!(
				"Vulkan device UUID {} does not match CUDA device UUID {}",
				uuid(image.device_uuid),
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
		self.backend.imported.fetch_sub(1, Ordering::AcqRel);
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

fn uuid(bytes: [u8; 16]) -> String {
	bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
#[path = "vulkan_test.rs"]
pub(crate) mod tests;
