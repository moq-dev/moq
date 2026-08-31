//! Preallocated handoff from the microphone callback to the async capture loop.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::{CachingCons, CachingProd, HeapCons, HeapProd, HeapRb};
use tokio::sync::mpsc;

use super::Samples;

/// Roughly 80 ms at the common 10 ms callback cadence.
const DEPTH: usize = 8;

/// Process unusually large host buffers in fixed pieces rather than growing a
/// scratch allocation on the realtime thread.
const CHUNK_FRAMES: usize = 4096;

/// The relay polls so the callback never participates in a blocking wakeup.
const RELAY_POLL_INTERVAL: Duration = Duration::from_millis(1);

/// Drops are logged at most this often, so a sustained stall does not spam.
const REPORT_INTERVAL: Duration = Duration::from_secs(1);

/// Create a pool and its bounded filled-buffer queue.
pub(super) fn channel(channels: usize, #[cfg(feature = "aec")] aec: Option<crate::aec::Canceller>) -> (Writer, Reader) {
	let samples = CHUNK_FRAMES * channels;
	let (filled, pending) = HeapRb::<Filled>::new(DEPTH).split();
	let (tx, rx) = mpsc::channel(1);

	let mut recycle = Vec::with_capacity(DEPTH);
	let mut free = Vec::with_capacity(DEPTH);
	for slot in 0..DEPTH {
		let ring = Arc::new(HeapRb::new(1));
		let recycled = CachingCons::new(ring.clone());
		recycle.push(RecycleSlot { ring, recycled });
		free.push(Buffer {
			data: Vec::with_capacity(samples),
			slot,
		});
	}

	// Polling isolates Tokio's wakeup and linked-block allocations from the
	// callback, which only produces into a fixed lock-free SPSC ring.
	std::thread::Builder::new()
		.name("moq-audio-capture".into())
		.spawn(move || relay(pending, tx))
		.expect("failed to spawn audio capture handoff");

	let dropped = Arc::new(AtomicU64::new(0));
	let writer = Writer {
		filled,
		recycle,
		free,
		pending_gap: false,
		dropped: dropped.clone(),
		channels,
		#[cfg(feature = "aec")]
		aec,
	};
	let reader = Reader {
		rx,
		dropped,
		unreported: 0,
		last_report: None,
	};

	(writer, reader)
}

/// Move filled buffers into Tokio without waking from the callback.
fn relay(mut pending: HeapCons<Filled>, tx: mpsc::Sender<Filled>) {
	loop {
		if let Some(filled) = pending.try_pop() {
			if tx.blocking_send(filled).is_err() {
				return;
			}
			continue;
		}

		if !pending.write_is_held() {
			return;
		}
		std::thread::sleep(RELAY_POLL_INTERVAL);
	}
}

/// One callback allocation and the recycle slot that owns it.
struct Buffer {
	data: Vec<f32>,
	slot: usize,
}

/// One buffer submitted to the async capture pipeline.
struct Filled {
	data: Vec<f32>,
	gap: bool,
	slot: usize,
	recycle: HeapProd<Vec<f32>>,
}

/// A dedicated SPSC return path for one pool allocation.
struct RecycleSlot {
	ring: Arc<HeapRb<Vec<f32>>>,
	recycled: HeapCons<Vec<f32>>,
}

/// The callback-owned half of the pool.
pub(super) struct Writer {
	filled: HeapProd<Filled>,
	recycle: Vec<RecycleSlot>,
	free: Vec<Buffer>,
	pending_gap: bool,
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

			debug_assert!(output.data.capacity() >= input.len());
			output.data.extend(input.iter().copied().map(&convert));

			#[cfg(feature = "aec")]
			if let Some(aec) = &self.aec {
				aec.process(&mut output.data);
			}

			let recycle = CachingProd::new(self.recycle[output.slot].ring.clone());
			let filled = Filled {
				data: output.data,
				gap: self.pending_gap,
				slot: output.slot,
				recycle,
			};

			match self.filled.try_push(filled) {
				Ok(()) => self.pending_gap = false,
				Err(filled) => {
					self.restore(filled);
					if !self.filled.read_is_held() {
						return;
					}
					self.drop_one();
				}
			}
		}

		if complete != input.len() {
			self.drop_one();
		}
	}

	/// Take an empty buffer without waiting for the async reader.
	fn take(&mut self) -> Option<Buffer> {
		self.reclaim();
		let mut output = self.free.pop()?;
		output.data.clear();
		Some(output)
	}

	/// Recover buffers whose downstream borrower has released them.
	fn reclaim(&mut self) {
		for (slot, recycle) in self.recycle.iter_mut().enumerate() {
			if recycle.recycled.write_is_held() {
				continue;
			}
			if let Some(mut data) = recycle.recycled.try_pop() {
				data.clear();
				self.free.push(Buffer { data, slot });
			}
		}
	}

	/// Put a failed submission directly back in the callback-owned pool.
	fn restore(&mut self, filled: Filled) {
		let Filled {
			mut data,
			slot,
			recycle,
			..
		} = filled;
		drop(recycle);
		data.clear();
		self.free.push(Buffer { data, slot });
	}

	fn drop_one(&mut self) {
		self.pending_gap = true;
		#[cfg(feature = "aec")]
		if let Some(aec) = &self.aec {
			aec.mark_discontinuous();
		}
		self.dropped.fetch_add(1, Ordering::Relaxed);
	}
}

/// The async half of the pool.
pub(super) struct Reader {
	rx: mpsc::Receiver<Filled>,
	dropped: Arc<AtomicU64>,
	unreported: u64,
	last_report: Option<Instant>,
}

impl Reader {
	/// Await one filled buffer and arrange to recycle it when the samples leave
	/// the capture pipeline.
	pub(super) async fn recv(&mut self) -> Option<Samples> {
		let filled = self.rx.recv().await?;
		self.observe();
		Some(Samples::pooled(filled.data, filled.gap, filled.recycle))
	}

	/// Fold callback drops into a throttled log.
	fn observe(&mut self) {
		let dropped = self.dropped.swap(0, Ordering::Relaxed);
		if dropped == 0 {
			return;
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
	}
}

#[cfg(test)]
mod tests {
	use std::alloc::{GlobalAlloc, Layout, System};
	use std::cell::Cell;
	use std::sync::mpsc::sync_channel;

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

	#[cfg(feature = "aec")]
	fn create_with_aec(channels: usize) -> (Writer, Reader, crate::aec::Canceller) {
		let aec = crate::aec::Canceller::new(
			Arc::new(crate::playback::Shared::default()),
			crate::aec::Config::default(),
		);
		aec.open(48_000, channels as u32).unwrap();
		let (writer, reader) = channel(channels, Some(aec.clone()));
		(writer, reader, aec)
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
	async fn sustained_handoff_never_allocates_on_the_writer() {
		let (mut writer, mut reader) = create(1);
		let input = vec![0.25f32; 480];

		for _ in 0..128 {
			assert_eq!(activity(|| writer.write_f32(&input)), 0);
			drop(reader.recv().await.unwrap());
		}
	}

	#[test]
	fn stalled_reader_never_blocks_or_allocates_on_the_writer() {
		let (mut writer, _reader) = create(1);
		let input = vec![0.25f32; 480];
		let (done, wait) = sync_channel(1);
		let thread = std::thread::spawn(move || {
			let activity = activity(|| {
				for _ in 0..DEPTH * 4 {
					writer.write_f32(&input);
				}
			});
			done.send(activity).unwrap();
		});

		assert_eq!(wait.recv_timeout(Duration::from_secs(1)).unwrap(), 0);
		thread.join().unwrap();
	}

	#[tokio::test]
	async fn recycles_buffers_after_the_pipeline_releases_them() {
		let (mut writer, mut reader) = create(1);
		let input = vec![0.5; 480];

		writer.write_f32(&input);
		let released = reader.recv().await.unwrap();
		let address = released.data.as_ptr();
		drop(released);

		writer.write_f32(&input);
		let reused = reader.recv().await.unwrap();
		assert_eq!(reused.data.as_ptr(), address);
	}

	#[tokio::test]
	async fn overflow_marks_the_first_recovery_buffer_as_a_gap() {
		let (mut writer, mut reader) = create(1);
		let backlog = vec![0.5; 480];

		for _ in 0..DEPTH {
			writer.write_f32(&backlog);
		}
		writer.write_f32(&[0.75; 480]);

		let first = reader.recv().await.unwrap();
		assert!(!first.gap);
		assert_eq!(first.data[0], 0.5);
		drop(first);
		writer.write_f32(&[1.0; 480]);

		for _ in 1..DEPTH {
			let backlog = reader.recv().await.unwrap();
			assert!(!backlog.gap);
			assert_eq!(backlog.data[0], 0.5);
		}
		let recovered = reader.recv().await.unwrap();
		assert!(recovered.gap);
		assert_eq!(recovered.data[0], 1.0);
	}

	#[cfg(feature = "aec")]
	#[tokio::test]
	async fn overflow_discards_partial_aec_frames() {
		let (mut writer, mut reader, aec) = create_with_aec(1);

		writer.write_f32(&[0.5; 512]);
		for _ in 1..DEPTH {
			writer.write_f32(&[0.5; 480]);
		}
		assert_eq!(aec.pending_samples(), 32);

		writer.write_f32(&[0.75; 512]);
		drop(reader.recv().await.unwrap());
		writer.write_f32(&[1.0; 448]);

		assert_eq!(
			aec.pending_samples(),
			448,
			"samples buffered before overflow leaked into the recovery callback"
		);
	}

	#[test]
	fn oversized_callbacks_are_chunked_without_allocating() {
		let (mut writer, _reader) = create(2);
		let input = vec![0.25f32; (CHUNK_FRAMES * 2) + 960];

		assert_eq!(activity(|| writer.write_f32(&input)), 0);
	}
}
