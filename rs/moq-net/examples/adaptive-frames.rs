//! Adaptive pages and separate versus inline headers. Storage prototype, not a wire format.
#[path = "support/allocation.rs"]
mod allocation;
#[path = "adaptive_frames/storage.rs"]
mod storage;
use allocation::ALLOCS;
use bytes::{BufMut, Bytes};
use std::{hint::black_box, time::Instant};
use storage::{Group, Header, Layout, Read};

#[derive(Clone, Copy)]
struct Case {
	layout: Layout,
	size: usize,
	count: usize,
	publish_every: usize,
}

fn fill(case: Case) -> Group {
	let Case {
		layout,
		size,
		count,
		publish_every,
	} = case;
	let mut group = Group::new(layout);
	for i in 0..count {
		let mut frame = group
			.create(Header {
				timestamp: i as u64,
				size: size as u64,
			})
			.unwrap();
		frame.put_bytes(42, size);
		frame.finish().unwrap();
		if (i + 1) % publish_every == 0 {
			group.publish();
		}
	}
	group.finish().unwrap();
	group
}

fn drain(group: &Group, case: Case, verify: bool) {
	let Case { size, count, .. } = case;
	let mut consumer = group.consumer();
	for i in 0..count {
		let Read::Ready(mut frame) = consumer.next().unwrap() else {
			panic!("missing frame")
		};
		assert_eq!(
			frame.header,
			Header {
				timestamp: i as u64,
				size: size as u64
			}
		);
		let mut read = 0;
		loop {
			match frame.read_chunk().unwrap() {
				Read::Ready(chunk) => {
					if verify {
						assert!(chunk.iter().all(|b| *b == 42));
					}
					read += chunk.len();
					black_box(chunk);
				}
				Read::End => break,
				Read::Pending => panic!("sealed group returned pending"),
			}
		}
		assert_eq!(read, size);
		frame.finish().unwrap();
	}
	assert!(matches!(consumer.next().unwrap(), Read::End));
}

fn median(mut times: Vec<f64>) -> f64 {
	times.sort_by(f64::total_cmp);
	times[times.len() / 2]
}

fn bench(case: Case) {
	let Case {
		layout,
		size,
		count,
		publish_every,
	} = case;
	let mut writes = Vec::new();
	let mut reads = Vec::new();
	// Repeat tiny groups inside each sample to avoid timing single clock ticks.
	let groups = (65536 / count).min(4096);
	for pass in 0..10 {
		let start = Instant::now();
		let all: Vec<_> = (0..groups).map(|_| fill(case)).collect();
		let write = start.elapsed().as_secs_f64() * 1e9 / (count * groups) as f64;
		let start = Instant::now();
		for group in &all {
			drain(group, case, false);
		}
		let read = start.elapsed().as_secs_f64() * 1e9 / (count * groups) as f64;
		if pass != 0 {
			writes.push(write);
			reads.push(read);
		}
		black_box(all);
	}
	ALLOCS.with(|c| c.set(Some((0, 0))));
	let group = fill(case);
	let (allocations, _) = ALLOCS.with(|c| c.replace(None).unwrap());
	drain(&group, case, true);
	let f = group.footprint();
	println!(
		"{layout:?},{size},{count},{publish_every},{:.2},{:.2},{},{},{},{},{},{allocations}",
		median(writes),
		median(reads),
		f.pages,
		f.page_bytes,
		f.header_bytes,
		f.chunks,
		f.chunk_bytes
	);
}

fn main() {
	// Exercise the owned-buffer path independently of copied/direct-fill timing.
	let payload = Bytes::from(vec![1; 100]);
	let ptr = payload.as_ptr();
	let mut group = Group::new(Layout::Indexed);
	let mut frame = group
		.create(Header {
			timestamp: 0,
			size: 100,
		})
		.unwrap();
	frame.write_owned(payload).unwrap();
	frame.publish();
	frame.finish().unwrap();
	group.finish().unwrap();
	let mut consumer = group.consumer();
	let Read::Ready(mut frame) = consumer.next().unwrap() else {
		panic!("missing frame")
	};
	let Read::Ready(bytes) = frame.read_chunk().unwrap() else {
		panic!("missing bytes")
	};
	assert_eq!(bytes.as_ptr(), ptr);
	frame.finish().unwrap();
	assert_eq!(group.footprint().pages, 0);

	println!(
		"layout,payload,frames,publish_every,write_ns_per_frame,read_ns_per_frame,pages,page_capacity,header_capacity_bytes,published_chunks,chunk_capacity_bytes,write_allocations"
	);
	for (size, count) in [
		(100, 1),
		(100, 4),
		(0, 65536),
		(1, 65536),
		(16, 65536),
		(64, 65536),
		(256, 65536),
		(1024, 8192),
	] {
		for cadence in [1, 32] {
			for layout in [Layout::Indexed, Layout::Packed] {
				bench(Case {
					layout,
					size,
					count,
					publish_every: cadence,
				});
			}
		}
	}
}

#[cfg(test)]
#[path = "adaptive_frames/tests.rs"]
mod tests;
