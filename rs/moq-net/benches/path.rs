//! Allocations per path-heavy operation: building and slicing paths, decoding the
//! messages that carry them, resolving requests on an origin, and fanning
//! announcements out to its cursors.
//!
//! Counts are deterministic, so this prints them instead of timing: a regression
//! shows as a higher number, not as noise. Paths span the inline limit: short (10
//! bytes), medium (22, the most that fits inline), and long (47).
//!
//! Run with `cargo bench -p moq-net --features fuzz --bench path`.

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicU64, Ordering};

use futures::FutureExt;
use moq_net::{
	Hop, Hops, Path, Pattern, Patterns, announce,
	fuzz::{Announced, Messages, decode_announces, decode_namespaces, encode_announces, encode_namespaces},
	kio, origin,
};

struct Counting;

static ALLOCS: AtomicU64 = AtomicU64::new(0);

// SAFETY: forwards every call to the system allocator unchanged, only counting.
unsafe impl GlobalAlloc for Counting {
	unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
		ALLOCS.fetch_add(1, Ordering::Relaxed);
		// SAFETY: the caller upholds `alloc`'s contract.
		unsafe { System.alloc(layout) }
	}

	unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
		// SAFETY: the caller upholds `dealloc`'s contract.
		unsafe { System.dealloc(ptr, layout) }
	}

	unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new: usize) -> *mut u8 {
		ALLOCS.fetch_add(1, Ordering::Relaxed);
		// SAFETY: the caller upholds `realloc`'s contract.
		unsafe { System.realloc(ptr, layout, new) }
	}
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Print the average allocations per item over `n` calls of `f`, each handling `per` items.
fn allocs<R>(name: &str, n: u64, per: u64, mut f: impl FnMut() -> R) {
	// Warm lazily built state first.
	for _ in 0..16 {
		black_box(f());
	}
	let before = ALLOCS.load(Ordering::Relaxed);
	for _ in 0..n {
		black_box(f());
	}
	let total = ALLOCS.load(Ordering::Relaxed) - before;
	println!("{name:<40} {:>8.2}", total as f64 / (n * per) as f64);
}

const SHORT: &str = "room/alice";
const MEDIUM: &str = "anon/7f3a9c2e1b/camera";
const LONG: &str = "1234/private/channel_7/stream-health-1700000000";

fn hops(ids: &[u64]) -> Hops {
	Hops::try_from(ids.iter().map(|id| Hop::new(*id).unwrap()).collect::<Vec<_>>()).unwrap()
}

fn paths() {
	for (name, s) in [("short", SHORT), ("medium", MEDIUM), ("long", LONG)] {
		let path = Path::new(s).unwrap();
		let prefix = Path::new(s.split('/').next().unwrap()).unwrap();
		let cam = Path::from_static("cam");
		allocs(&format!("path/new/{name}"), 10_000, 1, || Path::new(black_box(s)));
		allocs(&format!("path/clone/{name}"), 10_000, 1, || path.clone());
		allocs(&format!("path/strip_prefix/{name}"), 10_000, 1, || {
			path.strip_prefix(black_box(&prefix)).unwrap()
		});
		allocs(&format!("path/join/{name}"), 10_000, 1, || path.join(black_box(&cam)));
	}
}

fn decode() {
	let messages = Messages::default();
	let mut lite = Vec::new();
	messages.encode_lite(&mut lite);
	let mut ietf = Vec::new();
	messages.encode_ietf(&mut ietf);
	allocs("decode/lite_mix", 10_000, 1, || messages.decode_lite(&lite));
	allocs("decode/ietf_mix", 10_000, 1, || messages.decode_ietf(&ietf));

	type Shape = (&'static str, fn(u64) -> String);
	let shapes: [Shape; 3] = [
		("short", |i| format!("room/{i}")),
		("medium", |i| format!("anon/{:010x}/camera", i * 0x9E37_79B9)),
		("long", |i| {
			format!(
				"{}/private/channel_{}/stream-health-{}",
				1000 + i % 8,
				i,
				1_700_000_000 + i
			)
		}),
	];
	for (name, shape) in shapes {
		let paths: Vec<Path> = (0..1000).map(|i| moq_net::Path::try_from(shape(i)).unwrap()).collect();
		let stream: Vec<Announced> = paths
			.iter()
			.enumerate()
			.map(|(i, path)| Announced::Start(path.clone(), hops(&[0x1111 + i as u64, 0x2222])))
			.collect();
		for compress in [false, true] {
			let data = encode_announces(&stream, compress);
			let version = if compress { "lite07" } else { "lite06" };
			allocs(&format!("decode/announce_{version}/{name}"), 50, 1000, || {
				decode_announces(&data, compress)
			});
		}
		let data = encode_namespaces(&paths);
		allocs(&format!("decode/announce_ietf/{name}"), 50, 1000, || {
			decode_namespaces(&data)
		});
	}
}

fn requests() {
	for (name, s) in [("short", SHORT), ("long", LONG)] {
		let (producer, mut driver) = origin::Producer::new(origin::Config::default());
		let consumer = producer.consume();
		let _pubs: Vec<_> = (0..1000)
			.map(|i| producer.publish(format!("{s}/{i}"), origin::Route::default()).unwrap())
			.collect();
		let _covering = producer.publish(s, origin::Route::default()).unwrap();
		let waiter = kio::Waiter::noop();

		let mut local = |request: &dyn Fn() -> kio::Pending<origin::Requesting>| {
			let pending = request();
			driver.poll(moq_net::time::Instant::now(), &waiter).unwrap();
			pending.now_or_never().expect("resolves").expect("local broadcast")
		};

		let target = format!("{s}/0");
		let target_path = Path::new(&target).unwrap();
		allocs(&format!("origin/request_str/{name}"), 2_000, 1, || {
			local(&|| consumer.request(target.as_str(), None))
		});
		allocs(&format!("origin/request_path/{name}"), 2_000, 1, || {
			local(&|| consumer.request(&target_path, None))
		});

		// A rooted handle joins its root onto every path it is given.
		let (root, rest) = s.split_once('/').unwrap();
		let rooted = consumer.scope(root, &Patterns::from(Pattern::all())).unwrap();
		let rooted_target = format!("{rest}/0");
		allocs(&format!("origin/request_rooted_str/{name}"), 2_000, 1, || {
			local(&|| rooted.request(rooted_target.as_str(), None))
		});

		let missing = format!("{s}/missing");
		allocs(&format!("origin/unroutable_str/{name}"), 2_000, 1, || {
			let result = consumer.request(missing.as_str(), None).now_or_never();
			assert!(matches!(result, Some(Err(moq_net::Error::Unroutable))));
		});
	}
}

fn fanout() {
	for (name, s) in [("short", "room/incoming"), ("long", LONG)] {
		let (producer, _driver) = origin::Producer::new(origin::Config::default());
		let consumer = producer.consume();
		let _pubs: Vec<_> = (0..100)
			.map(|i| producer.publish(format!("room/{i}"), origin::Route::default()).unwrap())
			.collect();
		let mut cursors: Vec<announce::Consumer> = (0..100).map(|_| consumer.announced()).collect();
		for cursor in &mut cursors {
			while cursor.next().now_or_never().flatten().is_some() {}
		}

		// Per cursor, per publish and retract.
		allocs(&format!("origin/announce_fanout/{name}"), 500, 100, || {
			let publication = producer.publish(s, origin::Route::default()).unwrap();
			for cursor in &mut cursors {
				cursor.next().now_or_never().flatten().expect("announce delivered");
			}
			drop(publication);
			for cursor in &mut cursors {
				cursor.next().now_or_never().flatten().expect("retract delivered");
			}
		});
	}
}

fn main() {
	paths();
	decode();
	requests();
	fanout();
}
