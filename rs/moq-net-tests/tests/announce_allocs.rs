//! Allocation counting for the announce hot paths: decoding the paths a peer
//! announces, joining them, and revisiting a cursor whose winning route is
//! unchanged. Also the messages around them: a decoded field allocates only once
//! kept, encoding copies nothing, and a served request keeps one copy of its track
//! name.
//!
//! Every route change at a prefix revisits each cursor presenting it, and on a
//! dense mesh most of those visits find the cursor's winner unchanged: a worse
//! path arriving or leaving. Such a visit must not allocate, or the cost of
//! route churn multiplies by the cursors watching. A counting allocator needs
//! the whole binary to itself, hence a test of its own.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use moq_net::{
	Client, Hop, Hops, Path, Pattern, Patterns, Server, Version,
	fuzz::{decode_path, decode_subscribe, encode_path, encode_subscribe},
	kio, origin,
	time::{Driver, Instant},
	track,
	transport::memory,
};

#[path = "../../moq-net/tests/integration/support/mod.rs"]
mod support;

use support::{
	harness::{now, spawn, subscribed},
	mock::create_mock_session_pair,
};

thread_local! {
	/// Fresh allocations made by *this* thread. Per-thread because the harness may
	/// run tests concurrently in one process.
	///
	/// `const` initialised, so reading it from inside the allocator cannot allocate
	/// and recurse.
	static ALLOCS: Cell<usize> = const { Cell::new(0) };

	/// A byte size to watch, and the fresh allocations of exactly that size *this*
	/// thread made. Size 0 watches nothing, since nothing allocates zero bytes.
	static SIZED: Cell<(usize, usize)> = const { Cell::new((0, 0)) };

	/// The size [`SIZED`] watches while a [`Watched`] driver is polled.
	static WATCH: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

// Counting only. Every call forwards to the system allocator unchanged. A
// `realloc` grows a buffer that already exists, which amortizes, so only fresh
// allocations count.
unsafe impl GlobalAlloc for Counting {
	unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
		// `try_with`, because TLS is unavailable while a thread is being torn down.
		let _ = ALLOCS.try_with(|allocs| allocs.set(allocs.get() + 1));
		let _ = SIZED.try_with(|sized| {
			let (size, count) = sized.get();
			if size == layout.size() {
				sized.set((size, count + 1));
			}
		});
		unsafe { System.alloc(layout) }
	}

	unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
		unsafe { System.dealloc(ptr, layout) }
	}

	unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
		unsafe { System.realloc(ptr, layout, new_size) }
	}
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// A route as `peer` would have announced it: one hop, at `cost`.
fn peer_route(peer: u64, cost: u64) -> origin::Route {
	let mut hops = Hops::new();
	hops.push(Hop::new(peer).unwrap()).unwrap();
	origin::Route::default().with_hops(hops).with_cost(cost)
}

/// Allocations made by a worse route arriving at a watched prefix and leaving
/// again, with `cursors` cursors rooted at `root` watching it.
fn churn_allocs(root: &str, cursors: usize) -> usize {
	let (producer, _driver) = origin::Producer::new(origin::Config::default());
	let _winner = producer.publish("room/a", peer_route(1, 1)).unwrap();
	let all = Patterns::from(Pattern::all());
	let mut cursors: Vec<_> = (0..cursors)
		.map(|_| producer.consume().scope(root, &all).unwrap().announced())
		.collect();
	for cursor in &mut cursors {
		while cursor.try_next().is_some() {}
	}

	let cycle = || drop(producer.publish("room/a", peer_route(2, 5)).unwrap());
	// Reach the steady state first, with every table grown to what it settles at.
	for _ in 0..4 {
		cycle();
	}
	let before = ALLOCS.with(Cell::get);
	cycle();
	let allocs = ALLOCS.with(Cell::get) - before;

	for cursor in &mut cursors {
		assert!(cursor.try_next().is_none(), "the winner is unchanged");
	}
	allocs
}

/// A route that never wins costs the same however many cursors watch it, whether
/// they present its prefix beneath their root or as the root itself, which ranks
/// every route covering it.
#[test]
fn an_unchanged_winner_allocates_nothing_per_cursor() {
	for root in ["", "room", "room/a"] {
		let one = churn_allocs(root, 1);
		for cursors in [20, 100] {
			assert_eq!(
				churn_allocs(root, cursors),
				one,
				"{cursors} cursors rooted at {root:?} allocated more than one"
			);
		}
	}
}

/// Fresh allocations made while running `f`, including what it returns.
fn allocs<R>(f: impl FnOnce() -> R) -> usize {
	let before = ALLOCS.with(Cell::get);
	let out = f();
	let allocs = ALLOCS.with(Cell::get) - before;
	drop(out);
	allocs
}

const SHORT: &str = "room/alice";
const LONG: &str = "1234/private/channel_7/stream-health-1700000000";

/// The one buffer a decoded path keeps: none for a path short enough to store inline.
fn path_buffers(path: &str) -> usize {
	(path.len() > 22) as usize
}

/// A path decodes straight into the one shared buffer it keeps, or inline when short:
/// moq-lite reads it in place, and a moq-transport tuple joins its fields on the stack.
#[test]
fn a_decoded_path_allocates_only_its_buffer() {
	for path in [SHORT, LONG] {
		let mut wire = vec![path.len() as u8];
		wire.extend_from_slice(path.as_bytes());
		assert_eq!(decode_path(&wire, false).as_str(), path);
		let lite = allocs(|| decode_path(&wire, false));
		assert_eq!(lite, path_buffers(path), "allocations for a moq-lite {path:?}");

		let parts: Vec<&str> = path.split('/').collect();
		let mut wire = vec![parts.len() as u8];
		for part in &parts {
			wire.push(part.len() as u8);
			wire.extend_from_slice(part.as_bytes());
		}
		assert_eq!(decode_path(&wire, true).as_str(), path);
		let ietf = allocs(|| decode_path(&wire, true));
		assert_eq!(ietf, path_buffers(path), "allocations for a moq-transport {path:?}");
	}
}

/// Joining two paths copies them straight into the one buffer the result keeps, or
/// inline when short, rather than through a `String` first.
#[test]
fn a_joined_path_allocates_only_its_buffer() {
	for path in [SHORT, LONG] {
		let (head, tail) = path.split_once('/').unwrap();
		let (head, tail) = (Path::new(head).unwrap(), Path::new(tail).unwrap());
		assert_eq!(head.join(&tail).unwrap().as_str(), path);
		let joined = allocs(|| head.join(&tail).unwrap());
		assert_eq!(joined, path_buffers(path), "allocations joining {path:?}");
	}
}

/// Encoding a path writes its fields straight from its text, whatever the wire.
#[test]
fn an_encoded_path_allocates_nothing() {
	for path in [SHORT, LONG, r"room/a\/b", r"room/a\\/b"] {
		let path = Path::new(path).unwrap();
		for ietf in [false, true] {
			let mut wire = Vec::with_capacity(64);
			let encoded = allocs(|| encode_path(&path, ietf, &mut wire));
			assert_eq!(encoded, 0, "allocations encoding {path:?} (ietf: {ietf})");
			assert_eq!(decode_path(&wire, ietf), path);
		}
	}
}

/// A SUBSCRIBE's track name is only looked up, so decoding one borrows it from the wire:
/// what allocates is the path it keeps and, on moq-lite, its list of ranges.
#[test]
fn a_decoded_subscribe_borrows_its_track_name() {
	for path in [SHORT, LONG] {
		let broadcast = Path::new(path).unwrap();
		for ietf in [false, true] {
			let mut wire = Vec::new();
			encode_subscribe(&broadcast, "video/hd", ietf, &mut wire);
			assert_eq!(decode_subscribe(&wire, ietf), (broadcast.clone(), "video/hd".len()));

			let ranges = !ietf as usize;
			let decoded = allocs(|| decode_subscribe(&wire, ietf));
			assert_eq!(
				decoded,
				path_buffers(path) + ranges,
				"allocations decoding a SUBSCRIBE for {path:?} (ietf: {ietf})"
			);
		}
	}
}

/// A session driver that counts the allocations sized [`WATCH`] only while it is
/// polled, so one end of a session is counted apart from the other on the same thread.
struct Watched<D>(D);

impl<D: Driver> Driver for Watched<D> {
	fn poll(&mut self, now: Instant, waiter: &kio::Waiter) -> moq_net::Result<Option<Instant>> {
		SIZED.with(|sized| sized.set((WATCH.with(Cell::get), sized.get().1)));
		let next = self.0.poll(now, waiter);
		SIZED.with(|sized| sized.set((0, sized.get().1)));
		next
	}
}

/// A served request keeps one copy of its track name: the one its owned message
/// made, which moves into the log context as the request starts, rather than a
/// second one for the log context alone.
///
/// Counts the publisher's allocations sized exactly like the name across a whole
/// subscription, so the name is an unusual length: a `String` copy of it is sized
/// that way, while the model's shared `Arc<str>` copies carry a header.
#[kio::test]
async fn a_lite_request_keeps_one_copy_of_its_track_name() {
	const TRACK: &str = "video/hd/a-track-name-of-an-odd-length";

	// Moq-lite 05 and later ask for the track's info on a TRACK stream of its own
	// before the SUBSCRIBE, so the publisher serves two requests to 03's one.
	for (version, requests) in [("moq-lite-03", 1), ("moq-lite-07-wip", 2)] {
		let version: Version = version.parse().unwrap();
		let (publisher, driver) = origin::Producer::new(origin::Config::new(Hop::new(1).unwrap()));
		spawn(driver);
		let broadcast = publisher.publish("room", Default::default()).unwrap();
		let track = broadcast.create_track(TRACK, track::Info::default()).unwrap();
		let (viewer, driver) = origin::Producer::new(origin::Config::new(Hop::new(2).unwrap()));
		spawn(driver);

		let mut transport = memory::Config::default();
		transport.protocol = Some(version.alpn().to_string());
		let (client, server) = create_mock_session_pair(transport);
		let connect = Client::new()
			.with_versions(version.into())
			.with_subscriber(viewer.clone());
		let accept = Server::new()
			.with_versions(version.into())
			.with_publisher(publisher.consume());
		// Both at once: each handshake waits on the other's driver.
		let (client, server) = futures::join!(
			async {
				let (session, driver) = connect.connect(now(), client).await.unwrap();
				spawn(driver);
				session
			},
			async {
				let (session, driver) = accept.accept(now(), server).await.unwrap();
				spawn(Watched(driver));
				session
			}
		);

		let remote = viewer
			.consume()
			.follow("room", None)
			.unwrap()
			.available()
			.await
			.unwrap();
		WATCH.with(|watch| watch.set(TRACK.len()));
		SIZED.with(|sized| sized.set((0, 0)));
		let subscriber = remote.track(TRACK).unwrap().subscribe(None).await.unwrap();
		subscribed(&track).await;
		WATCH.with(|watch| watch.set(0));
		let copies = SIZED.with(|sized| sized.replace((0, 0)).1);
		assert_eq!(copies, requests, "{version}: the publisher's copies of the track name");
		drop((subscriber, client, server));
	}
}
