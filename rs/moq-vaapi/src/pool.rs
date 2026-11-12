//! A recycling pool of NV12 surfaces of one size, shared by the decoder's
//! targets and the video processor's outputs.

use std::cell::Cell;
use std::sync::{Arc, Mutex, Weak};

use anyhow::{anyhow, Context as _};

use crate::{bindings, Display, Surface, UsageHint, VA_FOURCC_NV12, VA_RT_FORMAT_YUV420};

/// A recycling pool of NV12 surfaces.
///
/// Surfaces are allocated on demand and come back when the last reference to
/// one drops, such as an [`ExportedFrame`](crate::decode::ExportedFrame)'s. So
/// a picture a consumer holds is never written over, and one it has let go of
/// is written into again rather than replaced by a fresh allocation.
///
/// The pool keeps at most [`FreeList::capacity`] surfaces free and destroys
/// any returned past that, so a consumer that held many pictures and then let
/// them all go does not leave that many parked here.
pub(crate) struct SurfacePool {
	display: Arc<Display>,
	width: u32,
	height: u32,
	hint: UsageHint,
	/// Shared with every surface on loan, which puts itself back from whichever
	/// thread drops it last.
	pub(crate) free: Arc<FreeList>,
	/// Surfaces allocated over the pool's life.
	pub(crate) allocated: Cell<usize>,
}

/// The surfaces a [`SurfacePool`] has free to write into.
pub(crate) struct FreeList {
	pub(crate) surfaces: Mutex<Vec<Surface<()>>>,
	/// The most surfaces kept; one returned past this is destroyed.
	pub(crate) capacity: usize,
}

impl SurfacePool {
	/// Returns an empty pool of `width` by `height` surfaces allocated with `hint`, keeping at most `capacity` free.
	pub(crate) fn new(display: &Arc<Display>, (width, height): (u32, u32), hint: UsageHint, capacity: usize) -> Self {
		Self {
			display: Arc::clone(display),
			width,
			height,
			hint,
			free: Arc::new(FreeList {
				surfaces: Mutex::new(Vec::new()),
				capacity,
			}),
			allocated: Cell::new(0),
		}
	}

	/// Returns a free surface, or allocates one when none is.
	pub(crate) fn alloc(&self) -> anyhow::Result<PooledSurface> {
		let recycled = self.free.surfaces.lock().expect("poisoned").pop();
		let surface = match recycled {
			Some(surface) => surface,
			None => {
				let surface = self
					.display
					.create_surfaces::<()>(
						VA_RT_FORMAT_YUV420,
						Some(VA_FOURCC_NV12),
						self.width,
						self.height,
						Some(self.hint),
						vec![()],
					)
					.map_err(|e| anyhow!("create a {}x{} surface: {e:?}", self.width, self.height))?
					.pop()
					.context("VA-API returned an empty surface list")?;
				self.allocated.set(self.allocated.get() + 1);
				tracing::debug!(
					"allocated surface {} at {}x{}",
					self.allocated.get(),
					self.width,
					self.height
				);
				surface
			}
		};

		Ok(PooledSurface {
			surface: Some(surface),
			free: Arc::downgrade(&self.free),
		})
	}
}

/// A surface on loan from a [`SurfacePool`], returned to it on drop.
///
/// Shared behind an [`Arc`] by whatever still reads it, such as the decoder's
/// DPB and an [`ExportedFrame`](crate::decode::ExportedFrame), so it goes back
/// only once none refers to it. The pool is held weakly: a surface that
/// outlives its pool is destroyed rather than kept for a size nobody asks for
/// any more.
pub(crate) struct PooledSurface {
	/// Always `Some` until dropped; the `Option` is only there so [`Drop`] can
	/// move the surface back into the pool.
	surface: Option<Surface<()>>,
	/// Where to return the surface. Empty for a surface that came from
	/// somewhere other than a pool, which is destroyed on drop.
	free: Weak<FreeList>,
}

impl PooledSurface {
	/// Wraps a surface that belongs to no pool.
	pub(crate) fn unpooled(surface: Surface<()>) -> Self {
		Self {
			surface: Some(surface),
			free: Weak::new(),
		}
	}

	pub(crate) fn get(&self) -> &Surface<()> {
		self.surface.as_ref().expect("surface is taken only on drop")
	}

	pub(crate) fn id(&self) -> bindings::VASurfaceID {
		self.get().id()
	}
}

impl Drop for PooledSurface {
	fn drop(&mut self) {
		let (Some(surface), Some(free)) = (self.surface.take(), self.free.upgrade()) else {
			return;
		};
		let mut surfaces = free.surfaces.lock().expect("poisoned");
		if surfaces.len() < free.capacity {
			surfaces.push(surface);
		}
	}
}
