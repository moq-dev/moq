//! Small-frame storage experiment. Run with `cargo run --release -p moq-net --example frame-storage`.
//! Packed storage measures sealed groups only; it does not implement live publication or eviction.
use bytes::{Bytes, BytesMut};
use futures::FutureExt;
use moq_net::{Timestamp, broadcast, frame};
use std::{collections::VecDeque, hint::black_box, time::Instant};

#[path = "support/allocation.rs"]
mod allocation;
use allocation::ALLOCS;
const PAGE: usize = 65536;
const COUNT: usize = 65536;
const REPEATS: usize = 9;

enum Storage {
	Records(VecDeque<frame::Frame>),
	Packed(Bytes),
}
fn build(kind: &str, payload: &Bytes, count: usize, input_mode: &str) -> Storage {
	if kind == "packed" {
		let mut data = BytesMut::new();
		for i in 0..count {
			let input = if input_mode == "owned" {
				Bytes::copy_from_slice(payload)
			} else {
				payload.clone()
			};
			data.extend_from_slice(&(input.len() as u32).to_le_bytes());
			data.extend_from_slice(&(i as u64).to_le_bytes());
			data.extend_from_slice(&input);
		}
		return Storage::Packed(data.freeze());
	}
	let mut records = VecDeque::new();
	let mut slab = BytesMut::new();
	for i in 0..count {
		let input = if input_mode == "owned" {
			Bytes::copy_from_slice(payload)
		} else {
			payload.clone()
		};
		let payload = if kind == "slab" {
			if slab.capacity() < input.len() {
				slab = BytesMut::with_capacity(PAGE.max(input.len()));
			}
			slab.extend_from_slice(&input);
			slab.split_to(input.len()).freeze()
		} else if input_mode == "borrowed" {
			Bytes::copy_from_slice(&input)
		} else {
			input
		};
		records.push_back(frame::Frame {
			timestamp: Timestamp::from_millis(i as u64).unwrap(),
			payload,
		});
	}
	Storage::Records(records)
}
fn drain(storage: &Storage) -> usize {
	let mut count = 0;
	match storage {
		Storage::Records(records) => {
			for frame in records {
				let payload = frame.payload.clone();
				black_box((frame.timestamp.value(), &payload));
				count += 1;
			}
		}
		Storage::Packed(bytes) => {
			let mut pos = 0;
			while pos < bytes.len() {
				let len = u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
				let timestamp = u64::from_le_bytes(bytes[pos + 4..pos + 12].try_into().unwrap());
				pos += 12;
				let payload = bytes.slice(pos..pos + len);
				black_box((timestamp, &payload));
				pos += len;
				count += 1;
			}
		}
	}
	count
}
fn verify(storage: &Storage, payload: &[u8], count: usize) {
	match storage {
		Storage::Records(records) => {
			assert_eq!(records.len(), count);
			for (i, frame) in records.iter().enumerate() {
				assert_eq!(frame.timestamp.value(), i as u64);
				assert_eq!(frame.payload.as_ref(), payload);
			}
		}
		Storage::Packed(bytes) => {
			let mut pos = 0;
			for i in 0..count {
				let len = u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
				let timestamp = u64::from_le_bytes(bytes[pos + 4..pos + 12].try_into().unwrap());
				assert_eq!(timestamp, i as u64);
				pos += 12;
				assert_eq!(&bytes[pos..pos + len], payload);
				pos += len;
			}
			assert_eq!(pos, bytes.len());
		}
	}
}
fn median(mut v: Vec<f64>) -> f64 {
	v.sort_by(f64::total_cmp);
	v[v.len() / 2]
}
fn storage_case(kind: &str, size: usize, input_mode: &str) {
	let payload = Bytes::from(vec![42; size]);
	let mut writes = Vec::new();
	let mut reads = Vec::new();
	// Warm up both allocation and read paths.
	assert_eq!(drain(&build(kind, &payload, COUNT, input_mode)), COUNT);
	for _ in 0..REPEATS {
		let start = Instant::now();
		let storage = build(kind, &payload, COUNT, input_mode);
		writes.push(start.elapsed().as_secs_f64() * 1e9 / COUNT as f64);
		let start = Instant::now();
		assert_eq!(drain(&storage), COUNT);
		reads.push(start.elapsed().as_secs_f64() * 1e9 / COUNT as f64);
		black_box(&storage);
	}
	ALLOCS.with(|c| c.set(Some((0, 0))));
	let storage = build(kind, &payload, COUNT, input_mode);
	let (allocs, requested) = ALLOCS.with(|c| c.replace(None).unwrap());
	ALLOCS.with(|c| c.set(Some((0, 0))));
	assert_eq!(drain(&storage), COUNT);
	let (read_allocs, _) = ALLOCS.with(|c| c.replace(None).unwrap());
	verify(&storage, &payload, COUNT);
	println!(
		"storage,{kind},{size},{input_mode},{COUNT},{:.2},{:.2},{allocs},{requested},{read_allocs}",
		median(writes),
		median(reads)
	);
}
fn model_case(kind: &str, size: usize) {
	let count = COUNT.min(16 * 1024 * 1024 / size.max(1));
	let payload = Bytes::from(vec![42; size]);
	let mut writes = Vec::new();
	let mut reads = Vec::new();
	let mut allocation = (0, 0);
	for pass in 0..=REPEATS {
		let mut broadcast = broadcast::Producer::new(broadcast::Info::default());
		let mut track = broadcast.create_track("bench", None).unwrap();
		let mut group = track.append_group().unwrap();
		if pass == 0 {
			ALLOCS.with(|c| c.set(Some((0, 0))));
		}
		let start = Instant::now();
		for _ in 0..count {
			match kind {
				"whole" => group.write_frame(Timestamp::ZERO, payload.clone()).unwrap(),
				"borrowed" => group.write_frame(Timestamp::ZERO, payload.as_ref()).unwrap(),
				_ => {
					let mut frame = group
						.create_frame(frame::Info {
							size: size as u64,
							timestamp: Timestamp::ZERO,
						})
						.unwrap();
					if kind == "stream" {
						frame.write(payload.clone()).unwrap();
					} else {
						let middle = size / 2;
						frame.write(&payload[..middle]).unwrap();
						frame.write(&payload[middle..]).unwrap();
					}
					frame.finish().unwrap();
				}
			}
		}
		let write = start.elapsed().as_secs_f64() * 1e9 / count as f64;
		if pass == 0 {
			allocation = ALLOCS.with(|c| c.replace(None).unwrap());
		}
		group.finish().unwrap();
		let mut consumer = group.consume();
		let start = Instant::now();
		for _ in 0..count {
			let frame = consumer.read_frame().now_or_never().unwrap().unwrap().unwrap();
			assert_eq!(frame.payload.len(), size);
			black_box(frame);
		}
		let read = start.elapsed().as_secs_f64() * 1e9 / count as f64;
		assert!(consumer.read_frame().now_or_never().unwrap().unwrap().is_none());
		if pass > 0 {
			writes.push(write);
			reads.push(read);
		}
	}
	println!(
		"model,{kind},{size},na,{count},{:.2},{:.2},{},{},na",
		median(writes),
		median(reads),
		allocation.0,
		allocation.1
	);
}
fn main() {
	eprintln!(
		"Frame={} Bytes={} reps={REPEATS}; allocations include realloc calls; requested bytes are cumulative, not peak RSS",
		std::mem::size_of::<frame::Frame>(),
		std::mem::size_of::<Bytes>()
	);
	println!(
		"scope,layout,payload,input_mode,frames,write_ns_per_frame,read_ns_per_frame,allocations,requested_bytes,first_read_allocations"
	);
	for size in [0, 1, 16, 64, 256, 1024] {
		for input_mode in ["shared", "borrowed", "owned"] {
			for kind in ["records", "slab", "packed"] {
				storage_case(kind, size, input_mode);
			}
		}
		for kind in ["whole", "borrowed", "stream", "chunked"] {
			model_case(kind, size);
		}
	}
}
