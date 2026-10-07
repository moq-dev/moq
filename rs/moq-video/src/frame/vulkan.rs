//! External Vulkan images and their producer-owned reuse slots.
//!
//! A producer exports a dedicated image and timeline semaphore once, then
//! publishes the slot for each capture. Encoders import it on its declared
//! device and return the slot only after their GPU reads have completed.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::fmt;
use std::os::fd::OwnedFd;
use std::sync::{Arc, Mutex};

use crate::{Error, Size};

/// The physical device and driver that own an external image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Device {
	/// Vulkan physical-device UUID.
	pub device_uuid: [u8; 16],
	/// Vulkan driver UUID, required to import opaque Vulkan allocations.
	pub driver_uuid: [u8; 16],
	/// Render node `dev_t`, absent when the exporter has no DRM render node.
	pub render_node: Option<u64>,
}

impl fmt::Display for Device {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "device {} driver {}", uuid(self.device_uuid), uuid(self.driver_uuid))
	}
}

/// One DRM modifier memory plane's allocation offset and row pitch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plane {
	/// Byte offset from the start of the exported allocation.
	pub offset: u64,
	/// Byte pitch reported by the producer, including any row padding.
	pub row_pitch: u64,
}

/// The memory handle's import contract.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Memory {
	/// Dedicated optimal-tiling memory, imported with the exporter's memory type.
	OpaqueFd {
		/// Vulkan memory type index used for the original allocation.
		memory_type: u32,
	},
	/// A DMA-BUF image, imported with its DRM format modifier.
	DmaBuf {
		/// DRM format modifier describing the allocation's tiling.
		modifier: u64,
		/// Explicit memory-plane layouts, in the DRM modifier's plane order.
		/// Importers check the plane count against the modifier's properties.
		planes: Vec<Plane>,
	},
}

/// The Vulkan handles exported for one reusable image slot.
pub struct Handles {
	/// Memory exported with the handle type declared by [`Image::memory`].
	pub memory: OwnedFd,
	/// An `OPAQUE_FD` timeline semaphore.
	pub timeline: OwnedFd,
}

impl Handles {
	/// Group the exported memory and timeline semaphore handles for an image.
	pub fn new(memory: OwnedFd, timeline: OwnedFd) -> Self {
		Self { memory, timeline }
	}

	#[cfg_attr(not(feature = "nvidia"), allow(dead_code))]
	fn try_clone(&self) -> Result<Self, Error> {
		Ok(Self::new(
			self.memory.try_clone().map_err(|e| Error::Codec(e.into()))?,
			self.timeline.try_clone().map_err(|e| Error::Codec(e.into()))?,
		))
	}
}

/// The byte order of an image's four 8-bit channels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channels {
	/// `VK_FORMAT_R8G8B8A8_UNORM`.
	Rgba,
	/// `VK_FORMAT_B8G8R8A8_UNORM`.
	Bgra,
}

/// An external dedicated Vulkan image, in `GENERAL` layout while being read.
///
/// The producer creates a 2D image with one mip, one array layer, one sample,
/// no create flags, exclusive sharing, and usage `TRANSFER_DST | SAMPLED |
/// STORAGE`. It binds dedicated memory at offset zero. `OpaqueFd` images use
/// optimal tiling; `DmaBuf` images use DRM modifier tiling with explicit memory
/// planes. The channel order names the exact UNORM format. Importers recreate
/// this contract, including usage, rather than infer image creation parameters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
	/// Device and driver that allocated the image.
	pub device: Device,
	/// Memory handle type and its import parameters.
	pub memory: Memory,
	/// Visible image size.
	pub size: Size,
	/// Complete allocation size, rather than the number of visible pixel bytes.
	pub allocation_size: u64,
	/// Packed 8-bit channel order.
	pub channels: Channels,
}

impl Image {
	fn validate(&self) -> Result<(), Error> {
		self.size.validate_nonzero("external Vulkan image")?;
		if self.allocation_size == 0 {
			return Err(Error::Unsupported(
				"external Vulkan allocation size must be non-zero".into(),
			));
		}
		match &self.memory {
			Memory::OpaqueFd { memory_type } if *memory_type >= 32 => {
				return Err(Error::Unsupported("Vulkan memory type index must be below 32".into()));
			}
			Memory::DmaBuf { planes, .. }
				if (planes.is_empty()
					|| planes.len() > 4
					|| planes
						.iter()
						.any(|plane| plane.row_pitch == 0 || plane.offset >= self.allocation_size)) =>
			{
				return Err(Error::Unsupported("Vulkan DMA-BUF requires one to four explicit memory planes with non-zero row pitches and offsets within its allocation".into()));
			}
			_ => {}
		}
		Ok(())
	}
}

/// Monotonic values for one producer-to-consumer-to-producer handoff.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timeline {
	/// Value the producer signals after finishing writes and the layout transition.
	pub ready: u64,
	/// Value the consumer signals after the last GPU reader finishes.
	pub complete: u64,
}

impl Timeline {
	/// Create one handoff with a completion value greater than its ready value.
	pub fn new(ready: u64, complete: u64) -> Result<Self, Error> {
		if complete <= ready {
			return Err(Error::Unsupported(format!(
				"Vulkan completion value {complete} must exceed ready value {ready}"
			)));
		}
		Ok(Self { ready, complete })
	}
}

#[cfg_attr(not(feature = "nvidia"), allow(dead_code))]
struct Allocation {
	handles: Handles,
	image: Image,
	// Imports belong to a slot, not to an FD that can be duplicated or reused.
	imports: Mutex<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>,
}

/// One external Vulkan allocation and the producer value that keeps it alive.
pub struct Slot<T: Send + Sync + 'static> {
	allocation: Arc<Allocation>,
	owner: T,
	last_complete: u64,
}

impl<T: Send + Sync + 'static> fmt::Debug for Slot<T> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Slot")
			.field("image", &self.allocation.image)
			.field("last_complete", &self.last_complete)
			.finish_non_exhaustive()
	}
}

impl<T: Send + Sync + 'static> Slot<T> {
	/// Own exported handles and retain their producer without opening a backend.
	///
	/// The producer guard must keep the Vulkan image, memory, and semaphore alive
	/// and safely finish or cancel its own queued writes before destroying them.
	/// An unread frame has no imported backend through which to wait for those
	/// writes, so its failed completion releases the guard for producer teardown.
	pub fn new(handles: Handles, image: Image, owner: T) -> Result<Self, Error> {
		image.validate()?;
		Ok(Self {
			allocation: Arc::new(Allocation {
				handles,
				image,
				imports: Mutex::new(HashMap::new()),
			}),
			owner,
			last_complete: 0,
		})
	}

	/// Access the producer-owned value while the slot is idle.
	pub fn owner(&self) -> &T {
		&self.owner
	}

	/// Mutate the producer-owned value while the slot is idle.
	pub fn owner_mut(&mut self) -> &mut T {
		&mut self.owner
	}

	/// Publish after queueing the ready signal, returning a completion handle.
	///
	/// A frame that no backend reads fails completion and releases its slot.
	/// It cannot be recycled because no consumer signalled its timeline. The
	/// guard must safely finish or cancel queued producer writes before teardown,
	/// as required by [`Slot::new`].
	pub fn publish(self, timeline: Timeline) -> Result<(Frame, Completion<T>), PublishError<T>> {
		if timeline.complete <= timeline.ready || timeline.ready <= self.last_complete {
			return Err(PublishError::new(
				Error::Unsupported(format!(
					"invalid Vulkan handoff {timeline:?} after completion {}",
					self.last_complete
				)),
				self,
			));
		}
		let (sender, receiver) = tokio::sync::oneshot::channel();
		let allocation = self.allocation.clone();
		let completion: Box<dyn Complete> = Box::new(ReturnSlot {
			slot: Slot {
				last_complete: timeline.complete,
				..self
			},
			sender,
		});
		Ok((
			Frame {
				inner: Arc::new(FrameInner {
					allocation,
					timeline,
					reader: Mutex::new(None),
					completion: Some(completion),
					#[cfg(feature = "nvidia")]
					converted: Mutex::new(Vec::new()),
				}),
			},
			Completion { receiver },
		))
	}
}

/// A published external image, retained until every consumer drops its clone.
#[derive(Clone)]
pub struct Frame {
	inner: Arc<FrameInner>,
}

impl fmt::Debug for Frame {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Frame")
			.field("image", &self.inner.allocation.image)
			.field("timeline", &self.inner.timeline)
			.finish_non_exhaustive()
	}
}

impl Frame {
	/// Image width in pixels.
	pub fn width(&self) -> u32 {
		self.image().size.width
	}
	/// Image height in pixels.
	pub fn height(&self) -> u32 {
		self.image().size.height
	}
	/// Image size in pixels.
	pub fn size(&self) -> Size {
		self.image().size
	}
	/// Image and device import contract.
	pub fn image(&self) -> &Image {
		&self.inner.allocation.image
	}
	/// Packed channel order.
	pub fn channels(&self) -> Channels {
		self.image().channels
	}

	#[cfg(feature = "nvidia")]
	pub(crate) fn handles(&self) -> Result<Handles, Error> {
		self.inner.allocation.handles.try_clone()
	}

	#[cfg(feature = "nvidia")]
	pub(crate) fn import<T: Any + Send + Sync>(
		&self,
		create: impl FnOnce() -> Result<T, Error>,
	) -> Result<Arc<T>, Error> {
		let mut imports = self.inner.allocation.imports.lock().expect("Vulkan imports poisoned");
		if let Some(imported) = imports.get(&TypeId::of::<T>()) {
			return Ok(imported.clone().downcast().expect("Vulkan import type"));
		}
		let imported = Arc::new(create()?);
		imports.insert(TypeId::of::<T>(), imported.clone());
		Ok(imported)
	}

	#[cfg(feature = "nvidia")]
	pub(crate) fn read(&self, imported: Arc<dyn Reader>) -> Result<(), Error> {
		let mut reader = self.inner.reader.lock().expect("Vulkan reader poisoned");
		if let Some(current) = reader.as_ref() {
			if !Arc::ptr_eq(current, &imported) {
				return Err(Error::Unsupported(
					"external image cannot be read through multiple import backends".into(),
				));
			}
		} else {
			imported.wait(self.inner.timeline.ready)?;
			*reader = Some(imported);
		}
		Ok(())
	}

	#[cfg(feature = "nvidia")]
	pub(crate) fn converted(
		&self,
		color: crate::Color,
		convert: impl FnOnce() -> Result<super::cuda::Frame, Error>,
	) -> Result<super::cuda::Frame, Error> {
		let mut converted = self.inner.converted.lock().expect("Vulkan conversions poisoned");
		if let Some((_, frame)) = converted.iter().find(|(existing, _)| *existing == color) {
			return Ok(frame.clone());
		}
		let frame = convert()?;
		converted.push((color, frame.clone()));
		Ok(frame)
	}
}

pub(crate) trait Reader: Send + Sync {
	#[cfg_attr(not(feature = "nvidia"), allow(dead_code))]
	fn wait(&self, value: u64) -> Result<(), Error>;
	fn finish(&self, value: u64, completion: Box<dyn Complete>);
}

struct FrameInner {
	allocation: Arc<Allocation>,
	timeline: Timeline,
	reader: Mutex<Option<Arc<dyn Reader>>>,
	completion: Option<Box<dyn Complete>>,
	#[cfg(feature = "nvidia")]
	converted: Mutex<Vec<(crate::Color, super::cuda::Frame)>>,
}

impl Drop for FrameInner {
	fn drop(&mut self) {
		let completion = self.completion.take().expect("Vulkan completion missing");
		if let Some(reader) = self.reader.get_mut().expect("Vulkan reader poisoned").take() {
			reader.finish(self.timeline.complete, completion);
		} else {
			completion.complete(false);
		}
	}
}

pub(crate) trait Complete: Send + Sync {
	fn complete(self: Box<Self>, reusable: bool);
}

struct ReturnSlot<T: Send + Sync + 'static> {
	slot: Slot<T>,
	sender: tokio::sync::oneshot::Sender<Slot<T>>,
}

impl<T: Send + Sync + 'static> Complete for ReturnSlot<T> {
	fn complete(self: Box<Self>, reusable: bool) {
		if reusable {
			let _ = self.sender.send(self.slot);
		}
	}
}

/// Resolves to the producer slot after the GPU has signalled completion.
pub struct Completion<T: Send + Sync + 'static> {
	receiver: tokio::sync::oneshot::Receiver<Slot<T>>,
}

impl<T: Send + Sync + 'static> Completion<T> {
	/// Return the slot if the GPU has completed, without blocking or polling the GPU.
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

/// the GPU failed before the producer slot became safe to reuse.
#[derive(Clone, Copy, Debug, thiserror::Error)]
#[error("Vulkan GPU completion failed; the producer slot was released")]
pub struct CompletionError;

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

pub(crate) fn uuid(bytes: [u8; 16]) -> String {
	bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(all(test, feature = "nvidia"))]
#[path = "vulkan_test.rs"]
pub(crate) mod tests;

#[cfg(test)]
mod unit_tests {
	use super::*;
	use std::os::unix::net::UnixStream;

	fn image() -> Image {
		Image {
			device: Device {
				device_uuid: [0xaa; 16],
				driver_uuid: [0xbb; 16],
				render_node: None,
			},
			memory: Memory::OpaqueFd { memory_type: 1 },
			size: Size::new(4, 2),
			allocation_size: 4096,
			channels: Channels::Rgba,
		}
	}

	fn slot() -> Slot<()> {
		let (memory, timeline) = UnixStream::pair().unwrap();
		Slot::new(Handles::new(memory.into(), timeline.into()), image(), ()).unwrap()
	}

	#[test]
	fn rejects_invalid_image_contracts() {
		let mut contract = image();
		contract.size.width = 0;
		assert!(contract.validate().is_err());
		contract.size = Size::new(3, 5);
		assert!(contract.validate().is_ok());
		contract.allocation_size = 0;
		assert!(contract.validate().is_err());
		contract = image();
		contract.memory = Memory::OpaqueFd { memory_type: 32 };
		assert!(contract.validate().is_err());
	}

	#[test]
	fn refuses_dma_buf_without_explicit_valid_plane_layouts() {
		for planes in [
			vec![],
			vec![Plane {
				offset: 0,
				row_pitch: 0,
			}],
			vec![Plane {
				offset: 4096,
				row_pitch: 32,
			}],
			vec![
				Plane {
					offset: 0,
					row_pitch: 32
				};
				5
			],
		] {
			let mut contract = image();
			contract.memory = Memory::DmaBuf { modifier: 0, planes };
			assert!(contract.validate().is_err());
		}
		let mut padded = image();
		padded.memory = Memory::DmaBuf {
			modifier: 0,
			planes: vec![Plane {
				offset: 128,
				row_pitch: 64,
			}],
		};
		assert!(padded.validate().is_ok());
	}

	#[test]
	fn validates_timeline_even_when_fields_are_set_directly() {
		assert!(Timeline::new(4, 4).is_err());
		assert!(Timeline::new(5, 4).is_err());
		assert!(slot().publish(Timeline { ready: 4, complete: 4 }).is_err());
		assert!(slot().publish(Timeline { ready: 0, complete: 1 }).is_err());
	}

	#[tokio::test]
	async fn unconsumed_image_retains_guard_until_last_clone_and_is_never_recycled() {
		use std::sync::atomic::{AtomicUsize, Ordering};
		struct Guard(Arc<AtomicUsize>);
		impl Drop for Guard {
			fn drop(&mut self) {
				self.0.fetch_add(1, Ordering::SeqCst);
			}
		}
		let released = Arc::new(AtomicUsize::new(0));
		let (memory, timeline) = UnixStream::pair().unwrap();
		let slot = Slot::new(
			Handles::new(memory.into(), timeline.into()),
			image(),
			Guard(released.clone()),
		)
		.unwrap();
		let (frame, mut completion) = slot.publish(Timeline::new(1, 2).unwrap()).unwrap();
		let held = frame.clone();
		drop(frame);
		assert_eq!(released.load(Ordering::SeqCst), 0);
		assert!(completion.try_wait().unwrap().is_none());
		drop(held);
		assert!(completion.wait().await.is_err());
		assert_eq!(released.load(Ordering::SeqCst), 1);
	}

	#[cfg(feature = "nvidia")]
	#[tokio::test]
	async fn caches_imports_by_slot_and_returns_only_after_last_reader() {
		struct Import;
		impl Reader for Import {
			fn wait(&self, _: u64) -> Result<(), Error> {
				Ok(())
			}
			fn finish(&self, _: u64, completion: Box<dyn Complete>) {
				completion.complete(true);
			}
		}
		let (frame, mut completion) = slot().publish(Timeline::new(1, 2).unwrap()).unwrap();
		let imported = frame.import(|| Ok(Import)).unwrap();
		let duplicate_handles = frame.handles().unwrap();
		drop(duplicate_handles);
		assert!(Arc::ptr_eq(
			&imported,
			&frame.import::<Import>(|| panic!("imported twice")).unwrap()
		));
		frame.read(imported.clone()).unwrap();
		let held = frame.clone();
		drop(frame);
		assert!(completion.try_wait().unwrap().is_none());
		drop(held);
		let slot = completion.wait().await.unwrap();
		let (_, completion) = slot.publish(Timeline::new(2, 3).unwrap()).err().unwrap().into_parts();
		let (frame, completion) = completion.publish(Timeline::new(3, 4).unwrap()).unwrap();
		assert!(Arc::ptr_eq(
			&imported,
			&frame.import::<Import>(|| panic!("reimported slot")).unwrap()
		));
		frame.read(imported).unwrap();
		drop(frame);
		assert!(completion.wait().await.is_ok());
	}
}
