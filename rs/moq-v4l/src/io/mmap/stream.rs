use std::time::Duration;
use std::{io, mem, sync::Arc};

use crate::buffer::{Metadata, Type};
use crate::device::{Device, Handle};
use crate::io::mmap::arena::Arena;
use crate::io::traits::{CaptureStream, OutputStream, Stream as StreamTrait};
use crate::io::State;
use crate::memory::Memory;
use crate::sys::*;
use crate::v4l2;

/// Stream of mapped buffers
///
/// An arena instance is used internally for buffer handling.
pub struct Stream {
	handle: Arc<Handle>,
	arena: Arena,
	arena_index: usize,
	buf_type: Type,
	buf_meta: Vec<Metadata>,
	timeout: Option<i32>,
	state: State,
	/// Output only: how many buffers since start userspace has yet to fill.
	unqueued: usize,
}

impl Stream {
	/// Returns a stream for frame capturing
	///
	/// # Arguments
	///
	/// * `dev` - Capture device ref to get its file descriptor
	/// * `buf_type` - Type of the buffers
	///
	/// # Example
	///
	/// ```
	/// use moq_v4l::buffer::Type;
	/// use moq_v4l::device::Device;
	/// use moq_v4l::io::mmap::Stream;
	///
	/// let dev = Device::new(0);
	/// if let Ok(dev) = dev {
	///     let stream = Stream::new(&dev, Type::VideoCapture);
	/// }
	/// ```
	pub fn new(dev: &Device, buf_type: Type) -> io::Result<Self> {
		Stream::with_buffers(dev, buf_type, 4)
	}

	pub fn with_buffers(dev: &Device, buf_type: Type, buf_count: u32) -> io::Result<Self> {
		let mut arena = Arena::new(dev.handle(), buf_type);
		let count = arena.allocate(buf_count)?;
		if count == 0 {
			return Err(io::Error::new(
				io::ErrorKind::InvalidInput,
				"the driver granted no buffers",
			));
		}
		let mut buf_meta = Vec::new();
		buf_meta.resize(count as usize, Metadata::default());

		Ok(Stream {
			handle: dev.handle(),
			arena,
			arena_index: 0,
			buf_type,
			buf_meta,
			timeout: None,
			state: State::Stopped,
			unqueued: 0,
		})
	}

	/// Returns the raw device handle
	pub fn handle(&self) -> Arc<Handle> {
		self.handle.clone()
	}

	/// Sets a timeout of the v4l file handle.
	pub fn set_timeout(&mut self, duration: Duration) {
		self.timeout = Some(crate::io::timeout_ms(duration));
	}

	/// Clears the timeout of the v4l file handle.
	pub fn clear_timeout(&mut self) {
		self.timeout = None;
	}

	fn buffer_desc(&self) -> v4l2_buffer {
		v4l2_buffer {
			type_: self.buf_type as u32,
			memory: Memory::Mmap as u32,
			..unsafe { mem::zeroed() }
		}
	}
}

impl Drop for Stream {
	fn drop(&mut self) {
		// Fails once the device is gone (ENODEV), which stops the queue anyway.
		let _ = self.stop();
	}
}

impl StreamTrait for Stream {
	type Item = [u8];

	fn start(&mut self) -> io::Result<()> {
		unsafe {
			let mut typ = self.buf_type as u32;
			v4l2::ioctl(
				self.handle.fd(),
				v4l2::vidioc::VIDIOC_STREAMON,
				&mut typ as *mut _ as *mut std::os::raw::c_void,
			)?;
		}

		// STREAMON on a streaming queue does nothing, and leaves a dequeued
		// buffer with userspace.
		if let State::Stopped = self.state {
			self.state = State::Started;
		}
		Ok(())
	}

	fn stop(&mut self) -> io::Result<()> {
		unsafe {
			let mut typ = self.buf_type as u32;
			v4l2::ioctl(
				self.handle.fd(),
				v4l2::vidioc::VIDIOC_STREAMOFF,
				&mut typ as *mut _ as *mut std::os::raw::c_void,
			)?;
		}

		self.state = State::Stopped;
		Ok(())
	}
}

impl<'b> CaptureStream<'b> for Stream {
	fn queue(&mut self, index: usize) -> io::Result<()> {
		let mut v4l2_buf = v4l2_buffer {
			index: index as u32,
			..self.buffer_desc()
		};

		unsafe {
			v4l2::ioctl(
				self.handle.fd(),
				v4l2::vidioc::VIDIOC_QBUF,
				&mut v4l2_buf as *mut _ as *mut std::os::raw::c_void,
			)?;
		}

		Ok(())
	}

	fn dequeue(&mut self) -> io::Result<usize> {
		let mut v4l2_buf = self.buffer_desc();

		if self.handle.poll(libc::POLLIN, self.timeout.unwrap_or(-1))? == 0 {
			// This condition can only happen if there was a timeout.
			// A timeout is only possible if the `timeout` value is non-zero, meaning we should
			// propagate it to the caller.
			return Err(io::Error::new(io::ErrorKind::TimedOut, "VIDIOC_DQBUF"));
		}

		unsafe {
			v4l2::ioctl(
				self.handle.fd(),
				v4l2::vidioc::VIDIOC_DQBUF,
				&mut v4l2_buf as *mut _ as *mut std::os::raw::c_void,
			)?;
		}
		self.arena_index = v4l2_buf.index as usize;
		if let State::Streaming { held } = &mut self.state {
			*held = true;
		}

		self.buf_meta[self.arena_index] = Metadata {
			bytesused: v4l2_buf.bytesused,
			flags: v4l2_buf.flags.into(),
			field: v4l2_buf.field,
			timestamp: v4l2_buf.timestamp.into(),
			sequence: v4l2_buf.sequence,
		};

		Ok(self.arena_index)
	}

	fn next(&'b mut self) -> io::Result<(&'b Self::Item, &'b Metadata)> {
		match self.state {
			State::Stopped | State::Started => {
				// Hand the driver every buffer once per start, however it started. A
				// start that fails partway hands back what it queued, which STREAMOFF
				// does even on a queue that never streamed, so the next call starts
				// from the same place rather than queueing a buffer twice.
				let started = (0..self.arena.len())
					.try_for_each(|index| CaptureStream::queue(self, index))
					.and_then(|()| self.start());
				if let Err(err) = started {
					self.stop()?;
					return Err(err);
				}
				self.state = State::Streaming { held: false };
			}
			State::Streaming { held: true } => {
				// After a timed-out dequeue the driver already holds every buffer, and
				// queueing one twice fails with EINVAL.
				CaptureStream::queue(self, self.arena_index)?;
				self.state = State::Streaming { held: false };
			}
			State::Streaming { held: false } => {}
		}

		self.arena_index = CaptureStream::dequeue(self)?;

		// The index used to access the buffer elements is given to us by v4l2, so we assume it
		// will always be valid.
		let bytes = self.arena.get(self.arena_index);
		let meta = &self.buf_meta[self.arena_index];
		Ok((bytes, meta))
	}
}

impl<'b> OutputStream<'b> for Stream {
	fn queue(&mut self, index: usize) -> io::Result<()> {
		let mut v4l2_buf = v4l2_buffer {
			index: index as u32,
			..self.buffer_desc()
		};
		unsafe {
			// output settings
			//
			// MetaData.bytesused is initialized to 0. For an output device, when bytesused is
			// set to 0 v4l2 will set it to the size of the plane:
			// https://www.kernel.org/doc/html/v4.15/media/uapi/v4l/buffer.html#struct-v4l2-plane
			v4l2_buf.bytesused = self.buf_meta[index].bytesused;
			v4l2_buf.field = self.buf_meta[index].field;

			v4l2::ioctl(
				self.handle.fd(),
				v4l2::vidioc::VIDIOC_QBUF,
				&mut v4l2_buf as *mut _ as *mut std::os::raw::c_void,
			)
		}
	}

	fn dequeue(&mut self) -> io::Result<usize> {
		let mut v4l2_buf = self.buffer_desc();

		// An output queue is writable once the driver hands a consumed buffer back;
		// before the first QBUF it has none to hand back, so the wait goes here.
		if self.handle.poll(libc::POLLOUT, self.timeout.unwrap_or(-1))? == 0 {
			return Err(io::Error::new(io::ErrorKind::TimedOut, "VIDIOC_DQBUF"));
		}

		unsafe {
			v4l2::ioctl(
				self.handle.fd(),
				v4l2::vidioc::VIDIOC_DQBUF,
				&mut v4l2_buf as *mut _ as *mut std::os::raw::c_void,
			)?;
		}
		self.arena_index = v4l2_buf.index as usize;

		self.buf_meta[self.arena_index] = Metadata {
			bytesused: v4l2_buf.bytesused,
			flags: v4l2_buf.flags.into(),
			field: v4l2_buf.field,
			timestamp: v4l2_buf.timestamp.into(),
			sequence: v4l2_buf.sequence,
		};

		Ok(self.arena_index)
	}

	fn next(&'b mut self) -> io::Result<(&'b mut Self::Item, &'b mut Metadata)> {
		match self.state {
			State::Stopped | State::Started => {
				// After start every buffer is userspace's to fill, however it started.
				self.start()?;
				self.arena_index = 0;
				self.unqueued = self.arena.len().saturating_sub(1);
			}
			State::Streaming { held } => {
				// A queue that failed leaves the filled buffer held for the retry, and
				// a dequeue that failed leaves it with the driver, so neither queues a
				// buffer twice.
				if held {
					OutputStream::queue(self, self.arena_index)?;
					self.state = State::Streaming { held: false };
				}
				// Fill every buffer once before waiting on one: a driver may not start
				// until several are queued.
				if self.unqueued > 0 {
					self.arena_index = self.arena.len() - self.unqueued;
					self.unqueued -= 1;
				} else {
					self.arena_index = OutputStream::dequeue(self)?;
				}
			}
		}
		self.state = State::Streaming { held: true };

		// The index used to access the buffer elements is given to us by v4l2, so we assume it
		// will always be valid.
		let bytes = self.arena.get_mut(self.arena_index);
		let meta = &mut self.buf_meta[self.arena_index];
		Ok((bytes, meta))
	}
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use super::*;
	use crate::capability::Flags;

	/// Against the kernel's `vivid` virtual driver, which nightly loads with
	/// `modprobe vivid`; run with `just rs v4l2`.
	mod vivid {
		use super::*;

		const FRAMES: usize = 8;

		/// The first `vivid` node streaming `flag`, found by driver name rather
		/// than a fixed `/dev/videoN`. A host without one skips, unless CI_STRICT
		/// makes that a failure.
		fn node(flag: Flags) -> Option<Device> {
			let mut nodes = crate::context::enum_devices();
			nodes.sort_by_key(crate::context::Node::index);
			let device = nodes.into_iter().find_map(|node| {
				let device = Device::with_path(node.path()).ok()?;
				let caps = device.query_caps().ok()?;
				(caps.driver == "vivid" && caps.capabilities.contains(flag | Flags::STREAMING)).then_some(device)
			});
			if device.is_none() {
				assert!(
					std::env::var_os("CI_STRICT").is_none(),
					"no vivid node; load it with `sudo modprobe vivid`"
				);
				eprintln!("skipping: no vivid node; load it with `sudo modprobe vivid`");
			}
			device
		}

		fn stream(device: &Device, buf_type: Type) -> Stream {
			let mut stream = Stream::with_buffers(device, buf_type, 4).expect("allocate buffers");
			stream.set_timeout(Duration::from_secs(2));
			stream
		}

		fn capture(stream: &mut Stream) {
			for _ in 0..FRAMES {
				let (buf, meta) = CaptureStream::next(stream).expect("capture a frame");
				assert!(meta.bytesused > 0 && buf.len() >= meta.bytesused as usize);
			}
		}

		fn output(stream: &mut Stream) {
			for i in 0..FRAMES {
				let (buf, _) = OutputStream::next(stream).expect("output a frame");
				buf.fill(i as u8);
			}
		}

		#[test]
		#[ignore = "needs the vivid kernel module"]
		fn capture_streams_however_it_starts() {
			let Some(device) = node(Flags::VIDEO_CAPTURE) else {
				return;
			};
			let mut stream = stream(&device, Type::VideoCapture);
			capture(&mut stream);
			stream.stop().expect("stop");
			stream.start().expect("start");
			capture(&mut stream);
		}

		#[test]
		#[ignore = "needs the vivid kernel module"]
		fn output_streams_however_it_starts() {
			let Some(device) = node(Flags::VIDEO_OUTPUT) else {
				return;
			};
			let mut stream = stream(&device, Type::VideoOutput);
			output(&mut stream);
			stream.stop().expect("stop");
			stream.start().expect("start");
			output(&mut stream);
			stream.stop().expect("stop");
			output(&mut stream);
		}

		/// A wait shorter than a frame makes most dequeues fail; a retry must
		/// neither queue a buffer twice (EINVAL) nor lose one.
		#[test]
		#[ignore = "needs the vivid kernel module"]
		fn output_retries_a_timed_out_dequeue() {
			let Some(device) = node(Flags::VIDEO_OUTPUT) else {
				return;
			};
			let mut stream = stream(&device, Type::VideoOutput);
			stream.set_timeout(Duration::from_millis(1));
			let (mut frames, mut timeouts) = (0, 0);
			while frames < 3 * FRAMES {
				match OutputStream::next(&mut stream) {
					Ok(_) => frames += 1,
					Err(err) if err.kind() == io::ErrorKind::TimedOut => timeouts += 1,
					Err(err) => panic!("after {frames} frames and {timeouts} timeouts: {err}"),
				}
			}
			assert!(timeouts > 0, "no dequeue timed out, so no retry was exercised");
		}
	}
}
