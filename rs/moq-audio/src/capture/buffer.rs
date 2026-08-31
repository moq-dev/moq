//! Preallocated handoff from the microphone callback to the async capture loop.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver as RecycleReceiver, SyncSender, sync_channel};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use super::Samples;

/// Roughly 80 ms at the common 10 ms callback cadence.
const DEPTH: usize = 8;

/// Process unusually large host buffers in fixed pieces rather than growing a
/// scratch allocation on the realtime thread.
const CHUNK_FRAMES: usize = 4096;

/// Drops are logged at most this often, so a sustained stall does not spam.
const REPORT_INTERVAL: Duration = Duration::from_secs(1);

/// Create a pool and its bounded filled-buffer queue.
pub(super) fn channel(channels: usize, #[cfg(feature = "aec")] aec: Option<crate::aec::Canceller>) -> (Writer, Reader) {
	let samples = CHUNK_FRAMES * channels;
	let (filled, rx) = mpsc::channel(DEPTH);
	let (recycle, recycled) = sync_channel(DEPTH);

	for _ in 0..DEPTH {
		recycle
			.try_send(Vec::with_capacity(samples))
			.expect("the recycle pool is sized to its initial buffers");
	}

	let dropped = Arc::new(AtomicU64::new(0));
	let writer = Writer {
		filled,
		recycled,
		current: Some(Vec::with_capacity(samples)),
		dropped: dropped.clone(),
		channels,
		#[cfg(feature = "aec")]
		aec,
	};
	let reader = Reader {
		rx,
		recycle,
		dropped,
		unreported: 0,
		last_report: None,
	};

	(writer, reader)
}

/// The callback-owned half of the pool.
pub(super) struct Writer {
	filled: mpsc::Sender<Vec<f32>>,
	recycled: RecycleReceiver<Vec<f32>>,
	current: Option<Vec<f32>>,
	dropped: Arc<AtomicU64>,
	channels: usize,
	#[cfg(feature = "aec")]
	aec: Option<crate::aec::Canceller>,
}

impl Writer {
	/// Copy native `f32` samples into preallocated buffers.
	pub(super) fn write_f32(&mut self, input: &[f32]) {
		self.write(input, |sample| sample);
	}

	/// Convert native signed samples into preallocated buffers.
	pub(super) fn write_i16(&mut self, input: &[i16]) {
		self.write(input, |sample| sample as f32 / 32768.0);
	}

	/// Convert native unsigned samples into preallocated buffers.
	pub(super) fn write_u16(&mut self, input: &[u16]) {
		self.write(input, |sample| (sample as f32 - 32768.0) / 32768.0);
	}

	/// Convert and submit one host callback without touching the allocator.
	fn write<T: Copy>(&mut self, input: &[T], convert: impl Fn(T) -> f32) {
		let chunk_samples = CHUNK_FRAMES * self.channels;
		let complete = input.len() - input.len() % self.channels;

		for input in input[..complete].chunks(chunk_samples) {
			let Some(mut output) = self.take() else {
				self.drop_one();
				continue;
			};

			debug_assert!(output.capacity() >= input.len());
			output.extend(input.iter().copied().map(&convert));

			#[cfg(feature = "aec")]
			if let Some(aec) = &self.aec {
				aec.process(&mut output);
			}

			match self.filled.try_send(output) {
				Ok(()) => {}
				Err(mpsc::error::TrySendError::Full(mut output)) => {
					output.clear();
					self.current = Some(output);
					self.drop_one();
				}
				Err(mpsc::error::TrySendError::Closed(mut output)) => {
					// Retain the allocation until the stream is dropped off the
					// callback thread.
					output.clear();
					self.current = Some(output);
					return;
				}
			}
		}

		if complete != input.len() {
			self.drop_one();
		}
	}

	/// Take an empty buffer without waiting for the async reader.
	fn take(&mut self) -> Option<Vec<f32>> {
		let mut output = self.current.take().or_else(|| self.recycled.try_recv().ok())?;
		output.clear();
		Some(output)
	}

	fn drop_one(&self) {
		self.dropped.fetch_add(1, Ordering::Relaxed);
	}
}

/// The async half of the pool.
pub(super) struct Reader {
	rx: mpsc::Receiver<Vec<f32>>,
	recycle: SyncSender<Vec<f32>>,
	dropped: Arc<AtomicU64>,
	unreported: u64,
	last_report: Option<Instant>,
}

impl Reader {
	/// Await one filled buffer and arrange to recycle it when the samples leave
	/// the capture pipeline.
	pub(super) async fn recv(&mut self) -> Option<Samples> {
		let data = self.rx.recv().await?;
		let gap = self.observe();
		Some(Samples::pooled(data, gap, self.recycle.clone()))
	}

	/// Fold callback drops into the timeline gap and a throttled log.
	fn observe(&mut self) -> bool {
		let dropped = self.dropped.swap(0, Ordering::Relaxed);
		if dropped == 0 {
			return false;
		}
		self.unreported += dropped;

		let now = Instant::now();
		if self
			.last_report
			.is_none_or(|last| now.duration_since(last) >= REPORT_INTERVAL)
		{
			tracing::warn!(
				dropped = self.unreported,
				capacity = DEPTH,
				"dropped audio capture buffers"
			);
			self.last_report = Some(now);
			self.unreported = 0;
		}

		true
	}
}

#[cfg(test)]
mod tests {
	use std::alloc::{GlobalAlloc, Layout, System};
	use std::cell::Cell;

	use super::*;

	struct TrackingAllocator;

	thread_local! {
		static ACTIVITY: Cell<Option<usize>> = const { Cell::new(None) };
	}

	unsafe impl GlobalAlloc for TrackingAllocator {
		unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
			ACTIVITY.with(|activity| {
				if let Some(count) = activity.get() {
					activity.set(Some(count + 1));
				}
			});
			unsafe { System.alloc(layout) }
		}

		unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
			ACTIVITY.with(|activity| {
				if let Some(count) = activity.get() {
					activity.set(Some(count + 1));
				}
			});
			unsafe { System.dealloc(ptr, layout) };
		}
	}

	#[global_allocator]
	static ALLOCATOR: TrackingAllocator = TrackingAllocator;

	fn activity(f: impl FnOnce()) -> usize {
		ACTIVITY.with(|activity| activity.set(Some(0)));
		f();
		ACTIVITY.with(|activity| activity.replace(None).unwrap())
	}

	fn create(channels: usize) -> (Writer, Reader) {
		channel(
			channels,
			#[cfg(feature = "aec")]
			None,
		)
	}

	#[test]
	fn every_sample_format_uses_the_preallocated_pool() {
		let (mut writer, _reader) = create(2);
		let f32s = vec![0.25f32; 960 * 2];
		let i16s = vec![8192i16; 960 * 2];
		let u16s = vec![40960u16; 960 * 2];

		assert_eq!(activity(|| writer.write_f32(&f32s)), 0);
		assert_eq!(activity(|| writer.write_i16(&i16s)), 0);
		assert_eq!(activity(|| writer.write_u16(&u16s)), 0);
	}

	#[tokio::test]
	async fn recycles_buffers_after_the_pipeline_releases_them() {
		let (mut writer, mut reader) = create(1);
		let input = vec![0.5; 480];

		for _ in 0..DEPTH {
			writer.write_f32(&input);
		}

		let released = reader.recv().await.unwrap();
		let address = released.data.as_ptr();
		writer.write_f32(&input);
		drop(released);

		let held = reader.recv().await.unwrap();
		writer.write_f32(&input);

		let mut reused = false;
		for _ in 0..DEPTH {
			let samples = reader.recv().await.unwrap();
			reused |= samples.data.as_ptr() == address;
		}
		drop(held);
		assert!(reused, "the released buffer never returned to the callback");
	}

	#[tokio::test]
	async fn overflow_marks_the_next_buffer_as_a_gap() {
		let (mut writer, mut reader) = create(1);
		let input = vec![0.5; 480];

		for _ in 0..=DEPTH {
			writer.write_f32(&input);
		}

		assert!(reader.recv().await.unwrap().gap);
	}

	#[test]
	fn oversized_callbacks_are_chunked_without_allocating() {
		let (mut writer, _reader) = create(2);
		let input = vec![0.25f32; (CHUNK_FRAMES * 2) + 960];

		assert_eq!(activity(|| writer.write_f32(&input)), 0);
	}
}
