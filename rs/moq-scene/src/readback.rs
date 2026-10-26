//! Copy canvas pictures back to the CPU, for an encoder that cannot take a GPU
//! surface.

use std::sync::{Arc, Mutex};

use moq_net::Timestamp;
use moq_video::render::wgpu;

use crate::Error;
use crate::collection::Canvas;

/// Copies of a canvas texture on their way back to the CPU, through a small
/// pool of staging buffers.
///
/// [`copy`](Self::copy) records the copy and returns at once; the [`Pending`]
/// it hands back waits for the GPU and converts the pixels, on whatever thread
/// the encoder runs on, so the canvas keeps drawing meanwhile. A copy that
/// finds every buffer still in flight is refused rather than queued, so an
/// encoder that falls behind drops pictures instead of adding latency.
pub struct Readback {
	device: wgpu::Device,
	queue: wgpu::Queue,
	canvas: Canvas,
	/// Bytes per row in a staging buffer, padded to the copy alignment.
	row: u32,
	pool: Arc<Mutex<Pool>>,
}

/// Staging buffers not held by a [`Pending`].
#[derive(Default)]
struct Pool {
	free: Vec<wgpu::Buffer>,
	/// Buffers in existence, up to [`Readback::DEPTH`].
	allocated: usize,
}

impl Readback {
	/// Copies in flight at once: one being converted, one being copied, and one
	/// spare so a slow conversion does not drop the very next picture.
	pub const DEPTH: usize = 3;

	/// Read back `canvas`-sized textures from `device`.
	pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, canvas: Canvas) -> Self {
		Self {
			device: device.clone(),
			queue: queue.clone(),
			canvas,
			row: (canvas.width * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT),
			pool: Arc::default(),
		}
	}

	/// Start copying `texture`, a canvas-sized [`FORMAT`](crate::compositor::FORMAT)
	/// texture created with `COPY_SRC`, to be encoded at `timestamp`.
	///
	/// `None` when every buffer is still held by an earlier [`Pending`].
	pub fn copy(&mut self, texture: &wgpu::Texture, timestamp: Timestamp) -> Option<Pending> {
		let mut pool = self.pool.lock().unwrap();
		let buffer = match pool.free.pop() {
			Some(buffer) => buffer,
			None if pool.allocated < Self::DEPTH => {
				pool.allocated += 1;
				self.device.create_buffer(&wgpu::BufferDescriptor {
					label: Some("moq-scene readback"),
					size: u64::from(self.row) * u64::from(self.canvas.height),
					usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
					mapped_at_creation: false,
				})
			}
			None => return None,
		};
		drop(pool);

		let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
			label: Some("moq-scene readback"),
		});
		encoder.copy_texture_to_buffer(
			wgpu::TexelCopyTextureInfo {
				texture,
				mip_level: 0,
				origin: wgpu::Origin3d::ZERO,
				aspect: wgpu::TextureAspect::All,
			},
			wgpu::TexelCopyBufferInfo {
				buffer: &buffer,
				layout: wgpu::TexelCopyBufferLayout {
					offset: 0,
					bytes_per_row: Some(self.row),
					rows_per_image: Some(self.canvas.height),
				},
			},
			wgpu::Extent3d {
				width: self.canvas.width,
				height: self.canvas.height,
				depth_or_array_layers: 1,
			},
		);
		let submission = self.queue.submit([encoder.finish()]);

		Some(Pending {
			device: self.device.clone(),
			buffer: Some(buffer),
			submission,
			canvas: self.canvas,
			row: self.row,
			timestamp,
			pool: self.pool.clone(),
			mapping: false,
		})
	}
}

/// One copy in flight. Dropping it returns its buffer to the pool.
pub struct Pending {
	device: wgpu::Device,
	buffer: Option<wgpu::Buffer>,
	submission: wgpu::SubmissionIndex,
	canvas: Canvas,
	row: u32,
	timestamp: Timestamp,
	pool: Arc<Mutex<Pool>>,
	/// Set while a mapping was requested and not yet undone, so a wait that
	/// failed partway does not hand a mapped buffer to the next copy.
	mapping: bool,
}

impl Pending {
	/// Block until the copy lands, then hand back the picture as a frame an
	/// encoder takes.
	pub fn wait(mut self) -> Result<moq_video::Frame, Error> {
		let rgba = self.rgba()?;
		let surface = moq_video::Surface::rgba(&rgba, self.canvas.size())?;
		Ok(moq_video::Frame::new(surface, self.timestamp))
	}

	/// Block until the copy lands, then hand back tightly packed RGBA rows.
	pub fn rgba(&mut self) -> Result<Vec<u8>, Error> {
		let buffer = self.buffer.as_ref().expect("held until drop");
		let (send, recv) = std::sync::mpsc::channel();
		self.mapping = true;
		buffer.map_async(wgpu::MapMode::Read, .., move |result| {
			let _ = send.send(result);
		});
		self.device
			.poll(wgpu::PollType::Wait {
				submission_index: Some(self.submission.clone()),
				timeout: None,
			})
			.map_err(|err| Error::Gpu(format!("waiting for the readback: {err}")))?;
		recv.recv()
			.map_err(|_| Error::Gpu("the readback was dropped".into()))?
			.map_err(|err| Error::Gpu(format!("mapping the readback: {err}")))?;

		let width = self.canvas.width as usize * 4;
		let mut rgba = Vec::with_capacity(width * self.canvas.height as usize);
		{
			let mapped = buffer
				.slice(..)
				.get_mapped_range()
				.map_err(|err| Error::Gpu(format!("reading the readback: {err}")))?;
			for row in mapped.chunks_exact(self.row as usize) {
				rgba.extend_from_slice(&row[..width]);
			}
		}
		buffer.unmap();
		self.mapping = false;
		Ok(rgba)
	}

	/// When the picture is shown.
	pub fn timestamp(&self) -> Timestamp {
		self.timestamp
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
