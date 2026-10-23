//! A finished [`encode::Producer`] keeps its track and rendition published, but not its splitter.
//!
//! Bindings keep finished video producers until their broadcast ends, so a long-lived broadcast
//! that retires many encoders would otherwise hold each one's largest access unit. A counting
//! allocator needs the whole binary to itself, hence a test of its own rather than a unit test.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use moq_video::encode;

/// The access unit each source publishes, far above the bookkeeping a finished producer keeps.
const PAD: usize = 1 << 20;

thread_local! {
	/// Bytes this thread freed minus those it allocated while measuring. Per thread so concurrent
	/// tests don't mix, and `const` so the allocator can read it without allocating.
	static RELEASED: Cell<Option<isize>> = const { Cell::new(None) };
}

fn count(bytes: isize) {
	// `try_with` because TLS is unavailable while a thread is being torn down.
	let _ = RELEASED.try_with(|released| released.set(released.get().map(|n| n + bytes)));
}

/// Counts on the measuring thread; every call forwards to the system allocator unchanged.
struct Counting;

unsafe impl GlobalAlloc for Counting {
	unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
		count(-(layout.size() as isize));
		unsafe { System.alloc(layout) }
	}

	unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
		count(layout.size() as isize);
		unsafe { System.dealloc(ptr, layout) }
	}

	unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
		count(layout.size() as isize - new_size as isize);
		unsafe { System.realloc(ptr, layout, new_size) }
	}
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// The heap `f` gives back, net of what it allocates on the way.
fn released(f: impl FnOnce()) -> isize {
	RELEASED.with(|released| released.set(Some(0)));
	f();
	RELEASED.with(|released| released.replace(None).unwrap())
}

/// An H.264 IDR access unit carrying its SPS and PPS, padded to [`PAD`] with filler data.
fn keyframe() -> bytes::Bytes {
	let mut au = vec![0, 0, 0, 1];
	au.extend_from_slice(&[
		0x67, 0x64, 0x00, 0x1f, 0xac, 0x24, 0x84, 0x01, 0x40, 0x16, 0xec, 0x04, 0x40, 0x00, 0x00, 0x03, 0x00, 0x40,
		0x00, 0x00, 0x0c, 0x23, 0xc6, 0x0c, 0x92,
	]);
	au.extend_from_slice(&[0, 0, 0, 1, 0x68, 0xee, 0x32, 0xc8, 0xb0]);
	au.extend_from_slice(&[0, 0, 0, 1, 0x65, 0x88, 0x84, 0x21]);
	au.extend_from_slice(&[0, 0, 0, 1, 0x0c]);
	au.resize(au.len() + PAD, 0xff);
	au.push(0x80);
	au.into()
}

#[tokio::test(start_paused = true)]
async fn finished_producer_releases_its_splitter() {
	for sources in [1, 4] {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let consumer = broadcast.consume();

		let rendition = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::H264(hang::catalog::H264 {
			inline: true,
			profile: 0x64,
			constraints: 0,
			level: 0x1f,
		}));
		let finished: Vec<_> = (0..sources)
			.map(|_| {
				let mut producer =
					encode::Producer::new(broadcast.clone(), catalog.reserve(), rendition.clone()).unwrap();
				for pts in [0, 33_333] {
					let pts = moq_net::Timestamp::from_micros(pts).unwrap();
					producer
						.publish(&[encode::Encoded::new(keyframe(), pts, true)])
						.unwrap();
				}
				producer.finish().unwrap();
				producer
			})
			.collect();

		// Late readers keep the published media, so only what the producers own is measured.
		let snapshot = catalog.snapshot();
		assert_eq!(snapshot.video.renditions.len(), sources);
		let _tracks: Vec<_> = snapshot
			.video
			.renditions
			.keys()
			.map(|name| consumer.track(name).unwrap())
			.collect();

		let released = released(|| drop(finished));
		assert!(
			released < (sources * PAD / 4) as isize,
			"{sources} finished producers still owned {released} bytes"
		);
	}
}
