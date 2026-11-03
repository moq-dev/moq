//! Shared capture history with independent, bounded readers.

use std::collections::VecDeque;
use std::sync::Arc;
use std::task::{Poll, ready};
use std::time::Duration;

use super::{Device, Error, Source};
use crate::encode::Input;
use moq_net::time::Instant;

/// Slack for a scheduled reader, independent of encoder retention.
const RETENTION: Duration = Duration::from_millis(80);

/// One shared block of interleaved f32 capture samples.
#[derive(Clone, Debug)]
pub struct Buffer {
	/// Samples in the consumer's fixed input layout.
	pub data: Arc<[f32]>,
	/// When the first sample was captured, stamped once before fanout.
	pub captured: Instant,
	/// Whether samples were lost before this buffer, including this reader's lag.
	pub gap: bool,
}

#[derive(Debug)]
struct State {
	buffers: VecDeque<Buffer>,
	first: u64,
	next: u64,
	samples: usize,
	failure: Option<Error>,
}

pub(super) struct Producer {
	state: kio::Producer<State>,
	input: Input,
	source: Source,
	device: Option<Device>,
}

impl Producer {
	pub(super) fn new(input: Input, source: Source, device: Option<Device>) -> Self {
		Self {
			state: kio::Producer::new(State {
				buffers: VecDeque::new(),
				first: 0,
				next: 0,
				samples: 0,
				failure: None,
			}),
			input,
			source,
			device,
		}
	}

	pub(super) fn consume(&self) -> Consumer {
		Consumer {
			state: self.state.consume(),
			next: self.state.read().next,
			input: self.input.clone(),
			source: self.source.clone(),
			device: self.device.clone(),
		}
	}

	pub(super) fn write(&self, buffer: Buffer) {
		let Ok(mut state) = self.state.write() else { return };
		let limit = (u128::from(self.input.sample_rate) * RETENTION.as_nanos() / 1_000_000_000) as usize
			* self.input.layout.channels() as usize;
		state.samples += buffer.data.len();
		state.buffers.push_back(buffer);
		state.next += 1;
		// Keep one complete host buffer even when it is larger than the history budget.
		while state.samples > limit && state.buffers.len() > 1 {
			state.samples -= state.buffers.pop_front().unwrap().data.len();
			state.first += 1;
		}
	}

	pub(super) fn abort(&self, error: Error) {
		if let Ok(mut state) = self.state.write() {
			state.failure = Some(error);
			state.buffers.clear();
			state.samples = 0;
			state.close();
		}
	}
}

impl Drop for Producer {
	fn drop(&mut self) {
		self.abort(Error::Capture("audio capture stopped".into()));
	}
}

/// An independent reader of one capture's samples and fixed layout.
///
/// Clones start at the same cursor and then advance independently. A reader that
/// falls behind skips expired buffers and marks its next buffer as a gap.
/// Readers never keep the device alive or hold its callback allocations.
#[derive(Clone, Debug)]
pub struct Consumer {
	state: kio::Consumer<State>,
	next: u64,
	input: Input,
	source: Source,
	device: Option<Device>,
}

impl Consumer {
	pub(crate) fn poll_failed(&self, waiter: &kio::Waiter) -> Poll<Error> {
		ready!(self.state.poll_closed(waiter));
		Poll::Ready(
			self.state
				.read()
				.failure
				.clone()
				.unwrap_or_else(|| Error::Capture("audio capture stopped".into())),
		)
	}

	pub(crate) fn skip_to_live(&mut self) {
		self.next = self.state.read().next;
	}

	/// The PCM format shared by every consumer of this capture.
	pub fn input(&self) -> &Input {
		&self.input
	}

	/// The selected capture source.
	pub fn source(&self) -> &Source {
		&self.source
	}

	/// The concrete microphone in use, or none for system audio.
	pub fn device(&self) -> Option<&Device> {
		self.device.as_ref()
	}

	/// Read the next buffer, or the shared error when capture stops or fails.
	pub fn poll_read(&mut self, waiter: &kio::Waiter) -> Poll<Result<Buffer, Error>> {
		let result = ready!(self.state.poll(waiter, |state| {
			if let Some(error) = &state.failure {
				return Poll::Ready(Err(error.clone()));
			}
			let next = self.next.max(state.first);
			let Some(buffer) = state.buffers.get((next - state.first) as usize) else {
				return Poll::Pending;
			};
			let mut buffer = buffer.clone();
			buffer.gap |= self.next < state.first;
			Poll::Ready(Ok((buffer, next + 1)))
		}));
		match result {
			Ok(Ok((buffer, next))) => {
				self.next = next;
				Poll::Ready(Ok(buffer))
			}
			Ok(Err(error)) => Poll::Ready(Err(error)),
			Err(_) => Poll::Ready(Err(Error::Capture("audio capture stopped".into()))),
		}
	}

	/// Read the next buffer, or the shared error when capture stops or fails.
	pub async fn read(&mut self) -> Result<Buffer, Error> {
		kio::wait(|waiter| self.poll_read(waiter)).await
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::Layout;

	fn producer() -> Producer {
		Producer::new(Input::new(48_000, Layout::Stereo), Source::default(), None)
	}

	fn buffer(value: f32, captured: Instant, gap: bool) -> Buffer {
		Buffer {
			data: vec![value; 960].into(),
			captured,
			gap,
		}
	}

	#[tokio::test(start_paused = true)]
	async fn readers_share_samples_times_and_source_gaps_without_retaining_the_device() {
		let producer = producer();
		let mut first = producer.consume();
		let mut second = first.clone();
		for gap in [false, true, false] {
			let captured = Instant::now();
			producer.write(buffer(0.25, captured, gap));
			let a = first.read().await.unwrap();
			let b = second.read().await.unwrap();
			assert!(Arc::ptr_eq(&a.data, &b.data));
			assert_eq!(a.captured, captured);
			assert_eq!(b.captured, captured);
			assert_eq!(a.gap, gap);
			assert_eq!(b.gap, gap);
		}
		drop(first);
		producer.write(buffer(0.5, Instant::now(), false));
		assert_eq!(second.read().await.unwrap().data[0], 0.5);
		drop(producer);
		assert!(matches!(second.read().await, Err(Error::Capture(_))));
	}

	#[tokio::test(start_paused = true)]
	async fn lag_is_bounded_and_marks_only_the_reader_that_missed_samples() {
		let producer = producer();
		let mut fast = producer.consume();
		let mut slow = producer.consume();
		let captured = Instant::now();
		for index in 0..40 {
			producer.write(buffer(index as f32, captured, false));
			let current = fast.read().await.unwrap();
			assert_eq!(current.data[0], index as f32);
			assert!(!current.gap);
		}
		let state = producer.state.read();
		assert_eq!(state.buffers.len(), 8);
		assert_eq!(state.samples, 7680);
		drop(state);
		let resumed = slow.read().await.unwrap();
		assert_eq!(resumed.data[0], 32.0);
		assert!(resumed.gap);
		assert!(!slow.read().await.unwrap().gap);
	}

	#[tokio::test(start_paused = true)]
	async fn failure_wins_over_buffered_audio_and_wakes_all_readers() {
		let producer = producer();
		let mut a = producer.consume();
		let mut b = producer.consume();
		let waiter = kio::Waiter::noop();
		assert!(a.poll_read(&waiter).is_pending());
		producer.write(buffer(0.5, Instant::now(), false));
		producer.abort(Error::Capture("device disconnected".into()));
		for reader in [&mut a, &mut b] {
			assert!(matches!(reader.read().await, Err(Error::Capture(message)) if message == "device disconnected"));
		}
	}
}
