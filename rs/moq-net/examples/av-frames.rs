//! Synthetic A/V-sized production-model workloads, with live cooperative fanout.
use bytes::Bytes;
use moq_net::{Timestamp, broadcast, frame, group};
use std::{
	hint::black_box,
	sync::{
		Arc,
		atomic::{AtomicUsize, Ordering},
	},
	task::{Poll, Wake, Waker},
	time::Instant,
};

#[path = "support/live_allocation.rs"]
mod allocation;

#[derive(Clone, Copy, Debug)]
enum Input {
	Owned,
	StreamOwned,
	Split,
	Chunk1200,
	Chunk16k,
}

#[derive(Clone, Copy, Debug)]
enum Delivery {
	Backlog,
	Live,
	Fanout,
}

impl Delivery {
	fn readers(self) -> usize {
		if matches!(self, Self::Fanout) { 8 } else { 1 }
	}
	fn live(self) -> bool {
		!matches!(self, Self::Backlog)
	}
}

struct Workload {
	name: String,
	frames: Vec<Bytes>,
}

impl Workload {
	fn new(name: String, sizes: Vec<usize>) -> Self {
		let frames: Vec<_> = sizes
			.into_iter()
			.enumerate()
			.map(|(i, size)| {
				let payload = Bytes::from(vec![i as u8; size]);
				// Promote Bytes ownership before measuring clones so fixture metadata is excluded.
				drop(payload.clone());
				payload
			})
			.collect();
		assert!(frames.iter().map(Bytes::len).sum::<usize>() <= 8 * 1024 * 1024);
		Self { name, frames }
	}
}

#[derive(Default)]
struct Wakes(AtomicUsize);
impl Wake for Wakes {
	fn wake(self: Arc<Self>) {
		self.wake_by_ref();
	}
	fn wake_by_ref(self: &Arc<Self>) {
		self.0.fetch_add(1, Ordering::Relaxed);
	}
}

struct Reader {
	group: group::Consumer,
	frame: Option<frame::Consumer>,
	index: usize,
	offset: usize,
	ended: bool,
	wakes: Arc<Wakes>,
	waiter: kio::Waiter,
	observed_wakes: usize,
}

impl Reader {
	fn new(group: group::Consumer) -> Self {
		let wakes = Arc::new(Wakes::default());
		let waiter = kio::Waiter::new(Waker::from(wakes.clone()));
		Self {
			group,
			frame: None,
			index: 0,
			offset: 0,
			ended: false,
			wakes,
			waiter,
			observed_wakes: 0,
		}
	}

	fn drain(&mut self, workload: &Workload, verify: bool, retained: &mut Option<Bytes>) {
		loop {
			if self.frame.is_none() {
				match self.group.poll_next_frame(&self.waiter) {
					Poll::Pending => return,
					Poll::Ready(Err(err)) => panic!("group read: {err}"),
					Poll::Ready(Ok(None)) => {
						assert_eq!(self.index, workload.frames.len());
						self.ended = true;
						return;
					}
					Poll::Ready(Ok(Some(frame))) => {
						assert!(self.index < workload.frames.len());
						if verify {
							assert_eq!(frame.size as usize, workload.frames[self.index].len());
							assert_eq!(frame.timestamp.as_micros(), self.index as u128 * 20_000);
						}
						self.frame = Some(frame);
					}
				}
			}
			match self.frame.as_mut().unwrap().poll_read_chunk(&self.waiter) {
				Poll::Pending => return,
				Poll::Ready(Err(err)) => panic!("frame read: {err}"),
				Poll::Ready(Ok(None)) => {
					assert_eq!(self.offset, workload.frames[self.index].len());
					self.offset = 0;
					self.index += 1;
					self.frame = None;
				}
				Poll::Ready(Ok(Some(chunk))) => {
					if verify {
						assert_eq!(
							chunk.as_ref(),
							&workload.frames[self.index][self.offset..self.offset + chunk.len()]
						);
					}
					if self.index + 1 == workload.frames.len() && self.offset == 0 && retained.is_none() {
						*retained = Some(chunk.slice(..chunk.len().min(64)));
					}
					self.offset += chunk.len();
					black_box(chunk);
				}
			}
		}
	}

	fn notified(&mut self, workload: &Workload, verify: bool, retained: &mut Option<Bytes>) {
		let wakes = self.wakes.0.load(Ordering::Relaxed);
		assert!(wakes > self.observed_wakes, "reader was not notified");
		self.observed_wakes = wakes;
		self.drain(workload, verify, retained);
	}
}

struct Output {
	retained: Option<Bytes>,
	cached: isize,
	wakes: usize,
}

fn run(workload: &Workload, input: Input, delivery: Delivery, verify: bool) -> Output {
	let mut broadcast = broadcast::Producer::new(broadcast::Info::default());
	let mut track = broadcast.create_track("bench", None).unwrap();
	let mut group = track.append_group().unwrap();
	let mut readers: Vec<_> = (0..delivery.readers()).map(|_| Reader::new(group.consume())).collect();
	let mut retained = None;
	if delivery.live() {
		for reader in &mut readers {
			reader.drain(workload, verify, &mut retained);
		}
	}
	for (i, payload) in workload.frames.iter().enumerate() {
		let timestamp = Timestamp::from_micros(i as u64 * 20_000).unwrap();
		if matches!(input, Input::Owned) {
			group.write_frame(timestamp, payload.clone()).unwrap();
		} else {
			let mut writer = group
				.create_frame(frame::Info {
					timestamp,
					size: payload.len() as u64,
				})
				.unwrap();
			if delivery.live() {
				for reader in &mut readers {
					reader.notified(workload, verify, &mut retained);
				}
			}
			if matches!(input, Input::StreamOwned) {
				writer.write(payload.clone()).unwrap();
				if delivery.live() {
					for reader in &mut readers {
						reader.notified(workload, verify, &mut retained);
					}
				}
			} else {
				let chunk = match input {
					Input::Split => payload.len().div_ceil(2),
					Input::Chunk1200 => 1200,
					Input::Chunk16k => 16384,
					_ => unreachable!(),
				};
				for chunk in payload.chunks(chunk) {
					writer.write(chunk).unwrap();
					if delivery.live() {
						for reader in &mut readers {
							reader.notified(workload, verify, &mut retained);
						}
					}
				}
			}
			writer.finish().unwrap();
		}
		if delivery.live() {
			for reader in &mut readers {
				reader.notified(workload, verify, &mut retained);
			}
		}
	}
	group.finish().unwrap();
	let cached = allocation::snapshot().live;
	for reader in &mut readers {
		if delivery.live() {
			reader.notified(workload, verify, &mut retained);
		} else {
			reader.drain(workload, verify, &mut retained);
		}
		assert!(reader.ended);
	}
	let wakes = readers
		.iter()
		.map(|reader| reader.wakes.0.load(Ordering::Relaxed))
		.sum();
	Output {
		retained,
		cached,
		wakes,
	}
}

fn bench(workload: &Workload, input: Input, delivery: Delivery) {
	let bytes: usize = workload.frames.iter().map(Bytes::len).sum();
	let groups = (2048 / workload.frames.len())
		.clamp(1, 64)
		.min((8 * 1024 * 1024 / bytes).max(1));
	drop(run(workload, input, delivery, true));
	let mut samples = Vec::new();
	for _ in 0..9 {
		let start = Instant::now();
		for _ in 0..groups {
			black_box(run(workload, input, delivery, false));
		}
		samples.push(start.elapsed().as_secs_f64() * 1e9 / (groups * workload.frames.len()) as f64);
	}
	samples.sort_by(f64::total_cmp);
	allocation::COUNTS.with(|cell| cell.set(Some(Default::default())));
	let output = run(workload, input, delivery, true);
	let counts = allocation::snapshot();
	assert!(counts.live >= 0);
	let held = output.retained.as_ref().unwrap().len();
	let cached = output.cached;
	let wakes = output.wakes;
	drop(output);
	assert_eq!(
		allocation::snapshot().live,
		0,
		"measurement leaked allocations or freed a fixture"
	);
	allocation::COUNTS.with(|cell| cell.set(None));
	println!(
		"{},{input:?},{delivery:?},{},{bytes},{},{groups},{:.2},{},{},{},{cached},{},{held},{wakes}",
		workload.name,
		workload.frames.len(),
		delivery.readers(),
		samples[4],
		counts.allocations,
		counts.requested,
		counts.peak,
		counts.live
	);
}

fn main() {
	let args: Vec<_> = std::env::args().skip(1).collect();
	let list = args == ["--list"];
	let selected = match args.as_slice() {
		[] => None,
		[flag] if flag == "--list" => None,
		[flag, case] if flag == "--case" => Some(case.as_str()),
		_ => panic!("usage: av-frames [--list | --case WORKLOAD/INPUT/DELIVERY]"),
	};
	if !list {
		println!(
			"workload,input,delivery,frames,payload_bytes,readers,groups_per_sample,ns_per_frame,allocations,requested_bytes,peak_live_bytes,cached_live_bytes,retained_live_bytes,retained_payload_bytes,wakes"
		);
	}
	let mut workloads = Vec::new();
	for size in [100, 256, 1000, 4096, 16384, 32768, 65536, 65537, 262144, 1048576] {
		let count = 60.min(8 * 1024 * 1024 / size);
		workloads.push((format!("fixed-{size}"), vec![size; count]));
	}
	workloads.push((
		"mixed-audio".into(),
		(0..50).map(|i| [100, 240, 400, 160, 1000][i % 5]).collect(),
	));
	workloads.push((
		"mixed-video".into(),
		(0..60)
			.map(|i| if i == 0 { 262144 } else { [4096, 16384, 32768][i % 3] })
			.collect(),
	));
	let mut found = false;
	for (name, sizes) in workloads {
		let mut workload = None;
		for input in [
			Input::Owned,
			Input::StreamOwned,
			Input::Split,
			Input::Chunk1200,
			Input::Chunk16k,
		] {
			for delivery in [Delivery::Backlog, Delivery::Live, Delivery::Fanout] {
				let case = format!("{name}/{input:?}/{delivery:?}");
				if list {
					println!("{case}");
					continue;
				}
				if selected.is_some_and(|selected| selected != case) {
					continue;
				}
				found = true;
				bench(
					workload.get_or_insert_with(|| Workload::new(name.clone(), sizes.clone())),
					input,
					delivery,
				);
			}
		}
	}
	assert!(list || found, "unknown benchmark case");
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn model_matrix_preserves_payloads_wakes_and_ownership() {
		let workload = Workload::new("test".into(), vec![100, 65536, 65537, 256, 262144]);
		for input in [
			Input::Owned,
			Input::StreamOwned,
			Input::Split,
			Input::Chunk1200,
			Input::Chunk16k,
		] {
			for delivery in [Delivery::Backlog, Delivery::Live, Delivery::Fanout] {
				drop(run(&workload, input, delivery, true));
				allocation::COUNTS.with(|cell| cell.set(Some(Default::default())));
				let output = run(&workload, input, delivery, true);
				assert_eq!(output.retained.as_ref().unwrap().len(), 64);
				assert!(allocation::snapshot().live >= 0);
				drop(output);
				assert_eq!(allocation::snapshot().live, 0);
				allocation::COUNTS.with(|cell| cell.set(None));
			}
		}
	}

	#[test]
	fn counts_follow_reallocation_and_deallocation() {
		allocation::COUNTS.with(|cell| cell.set(Some(Default::default())));
		let mut bytes = Vec::<u8>::with_capacity(black_box(17));
		let first = bytes.capacity();
		assert_eq!(allocation::snapshot().live, first as isize);
		bytes.reserve(black_box(1000));
		let second = bytes.capacity();
		assert_eq!(allocation::snapshot().live, second as isize);
		assert_eq!(allocation::snapshot().requested, first + second);
		assert_eq!(allocation::snapshot().allocations, 2);
		drop(bytes);
		assert_eq!(allocation::snapshot().live, 0);
		allocation::COUNTS.with(|cell| cell.set(None));
	}
}
