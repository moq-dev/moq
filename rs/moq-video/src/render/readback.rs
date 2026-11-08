//! Read textures drawn with `wgpu` back to the CPU, as packed RGBA.
//!
//! For a consumer that needs the raw picture rather than an encoded one, such as
//! a virtual camera or a screenshot. [`Readback::copy`] snapshots a texture into
//! one of a few staging buffers it owns and returns at once, so the caller keeps
//! drawing into its texture; [`Pending::wait`] then waits for the GPU on whatever
//! thread consumes the picture, and hands back its pixels.
//!
//! The [`export`](super::export) module reads through this when its encoder
//! takes frames on the CPU.

use std::sync::{Arc, Mutex};

use crate::convert::Rgba;
use crate::{Error, Size};

fn err(message: impl std::fmt::Display) -> Error {
	Error::render(anyhow::anyhow!("{message}"))
}

/// Copies textures back to the CPU, through a small pool of staging buffers.
///
/// A copy that finds every buffer still in flight is refused rather than
/// queued, so a consumer that falls behind drops pictures instead of adding
/// latency.
pub struct Readback {
	device: wgpu::Device,
	queue: wgpu::Queue,
	size: Size,
	/// Bytes per row in a staging buffer, padded to the copy alignment.
	row: u32,
	pool: Arc<Mutex<Buffers>>,
}

/// Staging buffers not held by a [`Pending`].
#[derive(Default)]
struct Buffers {
	free: Vec<wgpu::Buffer>,
	/// Buffers in existence, up to [`Readback::DEPTH`].
	allocated: usize,
}

impl Readback {
	/// Copies in flight at once: one being consumed, one being copied, and one
	/// spare so a slow consumer does not drop the very next picture.
	pub const DEPTH: usize = 3;

	/// Read back `size` textures drawn on `device`.
	///
	/// # Errors
	///
	/// Fails when `size` has a zero dimension.
	pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, size: Size) -> Result<Self, Error> {
		size.validate_nonzero("readback")?;
		Ok(Self {
			device: device.clone(),
			queue: queue.clone(),
			size,
			row: (size.width * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT),
			pool: Arc::default(),
		})
	}

	/// The size of every texture this copies, and of every picture it hands back.
	pub fn size(&self) -> Size {
		self.size
	}

	/// Start copying `texture` back to the CPU.
	///
	/// `texture` is [`size`](Self::size), [`wgpu::TextureFormat::Rgba8Unorm`],
	/// and created with `COPY_SRC`. The copy is submitted to the queue before
	/// this returns, so draw into `texture` again freely.
	///
	/// `None` when every buffer is still held by an earlier [`Pending`].
	pub fn copy(&mut self, texture: &wgpu::Texture) -> Result<Option<Pending>, Error> {
		check(texture, self.size, wgpu::TextureUsages::COPY_SRC)?;

		let mut pool = self.pool.lock().unwrap();
		let buffer = match pool.free.pop() {
			Some(buffer) => buffer,
			None if pool.allocated < Self::DEPTH => {
				pool.allocated += 1;
				self.device.create_buffer(&wgpu::BufferDescriptor {
					label: Some("moq-video readback"),
					size: u64::from(self.row) * u64::from(self.size.height),
					usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
					mapped_at_creation: false,
				})
			}
			None => return Ok(None),
		};
		drop(pool);

		let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
			label: Some("moq-video readback"),
		});
		encoder.copy_texture_to_buffer(
			texture.as_image_copy(),
			wgpu::TexelCopyBufferInfo {
				buffer: &buffer,
				layout: wgpu::TexelCopyBufferLayout {
					offset: 0,
					bytes_per_row: Some(self.row),
					rows_per_image: Some(self.size.height),
				},
			},
			extent(self.size),
		);
		let submission = self.queue.submit([encoder.finish()]);

		Ok(Some(Pending {
			device: self.device.clone(),
			buffer: Some(buffer),
			submission,
			size: self.size,
			row: self.row,
			pool: self.pool.clone(),
			mapping: false,
		}))
	}
}

/// One readback in flight. Dropping it gives its buffer back.
pub struct Pending {
	device: wgpu::Device,
	buffer: Option<wgpu::Buffer>,
	submission: wgpu::SubmissionIndex,
	size: Size,
	row: u32,
	pool: Arc<Mutex<Buffers>>,
	/// Set while a mapping was requested and not yet undone, so a wait that
	/// failed partway does not hand a mapped buffer to the next copy.
	mapping: bool,
}

impl Pending {
	/// Block until the copy lands, then hand back its pixels.
	pub fn wait(mut self) -> Result<Rgba, Error> {
		let buffer = self.buffer.as_ref().expect("held until drop");
		let (send, recv) = std::sync::mpsc::channel();
		self.mapping = true;
		buffer.map_async(wgpu::MapMode::Read, .., move |result| {
			let _ = send.send(result);
		});
		finish(&self.device, self.submission.clone())?;
		recv.recv()
			.map_err(|_| err("the readback was dropped"))?
			.map_err(|e| err(format!("mapping the readback: {e}")))?;

		let width = self.size.width as usize * 4;
		let mut rgba = Vec::with_capacity(width * self.size.height as usize);
		{
			let mapped = buffer
				.slice(..)
				.get_mapped_range()
				.map_err(|e| err(format!("reading the readback: {e}")))?;
			for row in mapped.chunks_exact(self.row as usize) {
				rgba.extend_from_slice(&row[..width]);
			}
		}
		buffer.unmap();
		self.mapping = false;
		Ok(Rgba::new(self.size, rgba))
	}
}

impl Drop for Pending {
	fn drop(&mut self) {
		let Some(buffer) = self.buffer.take() else {
			return;
		};
		let mut pool = self.pool.lock().unwrap();
		// The pool only holds buffers the next copy can write. One a failed wait
		// left mapped, or waiting on a mapping, is let go and replaced.
		if self.mapping {
			pool.allocated -= 1;
		} else {
			pool.free.push(buffer);
		}
	}
}

/// Refuse a texture that is not `size`, `Rgba8Unorm`, and created with `usage`.
pub(super) fn check(texture: &wgpu::Texture, size: Size, usage: wgpu::TextureUsages) -> Result<(), Error> {
	let actual = Size::new(texture.width(), texture.height());
	if actual != size || texture.format() != wgpu::TextureFormat::Rgba8Unorm {
		return Err(Error::Unsupported(format!(
			"cannot read a {actual} {:?} texture; expected {size} Rgba8Unorm",
			texture.format(),
		)));
	}
	if !texture.usage().contains(usage) {
		return Err(Error::Unsupported(format!(
			"cannot read a texture created without {usage:?}"
		)));
	}
	Ok(())
}

/// Block until `submission` has finished on the GPU.
pub(super) fn finish(device: &wgpu::Device, submission: wgpu::SubmissionIndex) -> Result<(), Error> {
	device
		.poll(wgpu::PollType::Wait {
			submission_index: Some(submission),
			timeout: None,
		})
		.map_err(|e| err(format!("waiting for the copy: {e}")))?;
	Ok(())
}

pub(super) fn extent(size: Size) -> wgpu::Extent3d {
	wgpu::Extent3d {
		width: size.width,
		height: size.height,
		depth_or_array_layers: 1,
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::render::export::tests::gpu;
	use crate::render::export::texture;

	// The readback lives on the drawing thread and each copy is waited on by the
	// consuming one.
	const _: fn() = || {
		fn send<T: Send>() {}
		send::<Readback>();
		send::<Pending>();
	};

	/// A readback hands back exactly what was drawn, unpadded, and drops copies
	/// past its depth rather than queuing them.
	#[tokio::test]
	#[ignore = "needs a GPU; run by just rs render-sw"]
	async fn round_trips_and_drops_past_its_depth() {
		let (device, queue) = gpu().await.expect("a GPU adapter");
		// 60 pixels is 240 bytes a row, short of the 256-byte copy alignment, so
		// the staging rows are padded and the unpadding is exercised.
		let size = Size::new(60, 32);
		let pixel = [255, 128, 7, 200];
		let drawn = texture(&device, &queue, size, pixel);
		let mut readback = Readback::new(&device, &queue, size).unwrap();

		let mut held: Vec<_> = (0..Readback::DEPTH)
			.map(|_| readback.copy(&drawn).unwrap().expect("a free buffer"))
			.collect();
		assert!(readback.copy(&drawn).unwrap().is_none());

		let rgba = held.pop().unwrap().wait().unwrap();
		assert_eq!(rgba.size(), size);
		assert_eq!(rgba.data(), pixel.repeat((size.width * size.height) as usize));

		// The waited buffer went back to the pool.
		assert!(readback.copy(&drawn).unwrap().is_some());
	}

	/// A texture of another size or format, or one that cannot be copied from, is
	/// refused rather than misread.
	#[tokio::test]
	#[ignore = "needs a GPU; run by just rs render-sw"]
	async fn refuses_a_mismatched_texture() {
		let (device, queue) = gpu().await.expect("a GPU adapter");
		let size = Size::new(64, 32);
		let mut readback = Readback::new(&device, &queue, size).unwrap();
		let other = texture(&device, &queue, Size::new(32, 32), [0; 4]);
		assert!(readback.copy(&other).is_err());

		let unreadable = device.create_texture(&wgpu::TextureDescriptor {
			label: None,
			size: extent(size),
			mip_level_count: 1,
			sample_count: 1,
			dimension: wgpu::TextureDimension::D2,
			format: wgpu::TextureFormat::Rgba8Unorm,
			usage: wgpu::TextureUsages::TEXTURE_BINDING,
			view_formats: &[],
		});
		assert!(readback.copy(&unreadable).is_err());
	}
}
