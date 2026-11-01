//! What cached frames received from the network keep alive beyond the bytes the cache bills.
//!
//! A receiving session hands each frame's payload to the cache as the transport delivered
//! it. A frame that arrives whole in one chunk is cached as that chunk, a slice of whatever
//! receive allocation the packet was copied into, while a frame spanning packets is copied
//! into storage of its own. The pool bills each frame's length, so a small frame can keep a
//! much larger receive allocation alive, including the other frames' bytes that were copied
//! out of it.
//!
//! Each case receives [`SECONDS`] of media on `tracks` video and audio track pairs sharing one
//! pool, through the session's frame reader ([`moq_net::fuzz::GroupRecv`]). Frames are
//! interleaved by timestamp and packed back to back into packets of [`PACKET`] stream bytes,
//! which arrive in one of three backings:
//!
//! - `exact`: every chunk in an allocation of its own, the control.
//! - `datagram`: one allocation per datagram, as `moq-uring` copies a completion without GRO.
//! - `recv64k`: consecutive datagrams share a 64 KiB allocation, as `moq-tokio`'s endpoint
//!   driver copies each receive into one reused `BytesMut` and splits datagrams off it.
//!   A `moq-uring` GRO completion is one allocation of up to 64 KiB as well.
//!
//! Then the oldest video groups are evicted, keeping `keep` of them, while every audio group
//! stays cached: the selective expiry that leaves a small audio frame holding a receive
//! allocation whose video bytes the cache already dropped.
//!
//! `BACKING_ALLOCS=1` prints, for every case, the live heap the cache retains against
//! [`cache::Pool::used`], the payload bytes retained, how many frames arrived whole, and the
//! allocations and bytes allocated per frame received, instead of timing. The live heap
//! counts each allocation once, however many frames slice it. An argument filters the cases
//! by name. Timing covers only the cases that keep every group, since eviction adds a few
//! aborts, and prints the same line for each before timing it.
//!
//! Run with `cargo bench -p moq-net --features fuzz --bench backing`.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicUsize, Ordering};

use bytes::{Bytes, BytesMut};
use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use moq_net::{Timescale, Timestamp, broadcast, cache, fuzz::GroupRecv, group, track};

/// Stream bytes per packet: a 1200-byte datagram less its QUIC overhead.
const PACKET: usize = 1150;

/// What `moq-tokio`'s receive buffer reallocates once a datagram split off it is still held:
/// `bytes` keeps at most 64 KiB of a buffer's original capacity.
const RECV_CAPACITY: usize = 64 * 1024;

/// Media received per case.
const SECONDS: u64 = 10;

/// One group per track every two seconds: a video GOP, and the audio beside it.
const GROUP_MS: u64 = 2000;

const VIDEO_FPS: u64 = 30;
/// A keyframe is this many delta frames.
const KEYFRAME: u64 = 10;

/// 20 ms Opus frames at 64 kbps.
const AUDIO_FPS: u64 = 50;
const AUDIO_FRAME: usize = 160;

/// Video bitrates, in kbps. Only the lowest has delta frames small enough to arrive whole.
const VIDEO_KBPS: [u64; 3] = [150, 1000, 4000];
const TRACKS: [usize; 2] = [1, 4];

#[derive(Clone, Copy, Debug)]
enum Backing {
	Exact,
	Datagram,
	Recv,
}

impl Backing {
	const ALL: [Self; 3] = [Self::Exact, Self::Datagram, Self::Recv];

	fn name(self) -> &'static str {
		match self {
			Self::Exact => "exact",
			Self::Datagram => "datagram",
			Self::Recv => "recv64k",
		}
	}
}

/// How many of each video track's groups stay cached: all, the newest quarter, or none.
const KEEP: [(&str, u64, u64); 3] = [("all", 1, 1), ("quarter", 1, 4), ("none", 0, 1)];

struct Counter;

static COUNTING: AtomicBool = AtomicBool::new(false);
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static ALLOCATED: AtomicUsize = AtomicUsize::new(0);
static LIVE: AtomicIsize = AtomicIsize::new(0);

#[global_allocator]
static ALLOCATOR: Counter = Counter;

impl Counter {
	fn count(size: usize) {
		if COUNTING.load(Ordering::Relaxed) {
			ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
			ALLOCATED.fetch_add(size, Ordering::Relaxed);
		}
	}
}

// Counting only. Every call forwards to the system allocator unchanged.
unsafe impl GlobalAlloc for Counter {
	unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
		Self::count(layout.size());
		LIVE.fetch_add(layout.size() as isize, Ordering::Relaxed);
		unsafe { System.alloc(layout) }
	}

	unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
		Self::count(layout.size());
		LIVE.fetch_add(layout.size() as isize, Ordering::Relaxed);
		unsafe { System.alloc_zeroed(layout) }
	}

	unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
		Self::count(new_size);
		LIVE.fetch_add(new_size as isize - layout.size() as isize, Ordering::Relaxed);
		unsafe { System.realloc(ptr, layout, new_size) }
	}

	unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
		LIVE.fetch_sub(layout.size() as isize, Ordering::Relaxed);
		unsafe { System.dealloc(ptr, layout) }
	}
}

/// One frame of the workload, and the packets its payload landed in.
struct Frame {
	/// Index into [`Media::tracks`]: video tracks first, then audio.
	track: usize,
	timestamp: u64,
	/// Opens a group.
	start: bool,
	/// `(packet, offset, len)` per chunk, in order.
	chunks: Vec<(usize, usize, usize)>,
}

/// The workload of one case, packetized once.
struct Media {
	tracks: usize,
	frames: Vec<Frame>,
	/// Stream bytes per packet.
	packets: Vec<usize>,
	payload: usize,
}

impl Media {
	fn new(tracks: usize, video_kbps: u64) -> Self {
		// A GOP of one keyframe and the deltas after it carries the bitrate.
		let gop = VIDEO_FPS * GROUP_MS / 1000;
		let delta = (video_kbps * 1000 / 8 * GROUP_MS / 1000 / (KEYFRAME + gop - 1)) as usize;

		let mut frames = Vec::new();
		for track in 0..tracks {
			for index in 0..VIDEO_FPS * SECONDS {
				let timestamp = index * 1000 / VIDEO_FPS;
				let start = index % gop == 0;
				let size = if start { delta * KEYFRAME as usize } else { delta };
				frames.push((timestamp, track, start, size));
			}
			for index in 0..AUDIO_FPS * SECONDS {
				let timestamp = index * 1000 / AUDIO_FPS;
				let start = timestamp % GROUP_MS < 1000 / AUDIO_FPS;
				frames.push((timestamp, tracks + track, start, AUDIO_FRAME));
			}
		}
		frames.sort_by_key(|(timestamp, track, ..)| (*timestamp, *track));

		// Back to back, each packet filled before the next.
		let mut packets = vec![0];
		let mut payload = 0;
		let frames = frames
			.into_iter()
			.map(|(timestamp, track, start, mut size)| {
				payload += size;
				let mut chunks = Vec::new();
				while size > 0 {
					let packet = packets.len() - 1;
					let offset = packets[packet];
					let len = size.min(PACKET - offset);
					chunks.push((packet, offset, len));
					size -= len;
					packets[packet] += len;
					if packets[packet] == PACKET {
						packets.push(0);
					}
				}
				Frame {
					track,
					timestamp,
					start,
					chunks,
				}
			})
			.collect();

		Self {
			tracks: 2 * tracks,
			frames,
			packets,
			payload,
		}
	}

	fn is_video(&self, track: usize) -> bool {
		track < self.tracks / 2
	}
}

/// The transport's receive path: copies each datagram into its backing and hands out slices.
struct Driver {
	backing: Backing,
	recv: BytesMut,
	/// The packet the last chunk came from.
	current: Option<(usize, Bytes)>,
}

/// What every datagram's bytes are copied from: the content does not matter.
static WIRE: [u8; PACKET] = [0; PACKET];

impl Driver {
	fn new(backing: Backing) -> Self {
		Self {
			backing,
			recv: match backing {
				Backing::Recv => BytesMut::with_capacity(RECV_CAPACITY),
				_ => BytesMut::new(),
			},
			current: None,
		}
	}

	fn chunk(&mut self, media: &Media, (packet, offset, len): (usize, usize, usize)) -> Bytes {
		let bytes = media.packets[packet];
		match self.backing {
			Backing::Exact => Bytes::copy_from_slice(&WIRE[offset..offset + len]),
			Backing::Datagram | Backing::Recv => {
				if self.current.as_ref().is_none_or(|(current, _)| *current != packet) {
					let datagram = match self.backing {
						// As `moq-uring` copies a completion out of its receive pool.
						Backing::Datagram => BytesMut::from(&WIRE[..bytes]).freeze(),
						// As `moq-tokio` receives: reserve, copy in, split off.
						_ => {
							self.recv.reserve(bytes);
							self.recv.extend_from_slice(&WIRE[..bytes]);
							self.recv.split_to(bytes).freeze()
						}
					};
					self.current = Some((packet, datagram));
				}
				let (_, datagram) = self.current.as_ref().expect("set above");
				datagram.slice(offset..offset + len)
			}
		}
	}
}

/// A receiving broadcast, its tracks sharing one pool.
struct Cache {
	pool: cache::Pool,
	_broadcast: broadcast::Producer,
	tracks: Vec<track::Producer>,
	/// Each track's finished groups, oldest first, with their payload bytes.
	groups: Vec<Vec<(group::Producer, usize)>>,
}

impl Cache {
	fn new(media: &Media) -> Self {
		let mut info = broadcast::Info::new();
		info.pool = cache::Pool::unbounded();
		let pool = info.pool.clone();
		let broadcast = info.produce();
		let tracks = (0..media.tracks)
			.map(|index| {
				let info = track::Info::default().with_timescale(Timescale::MILLI);
				broadcast.create_track(format!("{index}").as_str(), info).unwrap()
			})
			.collect();
		Self {
			pool,
			_broadcast: broadcast,
			tracks,
			groups: (0..media.tracks).map(|_| Vec::new()).collect(),
		}
	}

	/// Receive every frame, then evict all but `keep` of each video track's groups.
	fn run(&mut self, media: &Media, backing: Backing, (_, num, den): (&str, u64, u64)) {
		let mut driver = Driver::new(backing);
		let mut open: Vec<Option<(GroupRecv, usize)>> = (0..media.tracks).map(|_| None).collect();
		for frame in &media.frames {
			let slot = &mut open[frame.track];
			if frame.start {
				if let Some((recv, bytes)) = slot.take() {
					self.groups[frame.track].push((recv.finish().unwrap(), bytes));
				}
				*slot = Some((GroupRecv::new(self.tracks[frame.track].append_group().unwrap()), 0));
			}
			let (recv, bytes) = slot.as_mut().expect("every track opens with a group");
			let chunks: Vec<_> = frame.chunks.iter().map(|chunk| driver.chunk(media, *chunk)).collect();
			*bytes += chunks.iter().map(Bytes::len).sum::<usize>();
			let timestamp = Timestamp::new(frame.timestamp, Timescale::MILLI).unwrap();
			recv.frame(Some(timestamp), chunks).unwrap();
		}
		for (track, recv) in open.into_iter().enumerate() {
			if let Some((recv, bytes)) = recv {
				self.groups[track].push((recv.finish().unwrap(), bytes));
			}
		}
		drop(driver);

		for track in (0..media.tracks).filter(|track| media.is_video(*track)) {
			let groups = &mut self.groups[track];
			let keep = groups.len() as u64 * num / den;
			let evict = groups.len() - keep as usize;
			for (group, _) in groups.drain(..evict) {
				group.abort(moq_net::Error::Evicted).unwrap();
			}
		}
	}

	/// Payload bytes still cached.
	fn payload(&self) -> usize {
		self.groups.iter().flatten().map(|(_, bytes)| bytes).sum()
	}
}

/// Every `(name, tracks, video kbps, backing, keep)` case.
fn cases() -> impl Iterator<Item = (String, usize, u64, Backing, (&'static str, u64, u64))> {
	TRACKS.into_iter().flat_map(|tracks| {
		VIDEO_KBPS.into_iter().flat_map(move |kbps| {
			KEEP.into_iter().flat_map(move |keep| {
				Backing::ALL.into_iter().map(move |backing| {
					let name = format!("tracks={tracks}/video={kbps}k/keep={}/{}", keep.0, backing.name());
					(name, tracks, kbps, backing, keep)
				})
			})
		})
	})
}

/// The live heap, once the cache's deferred frees have run.
fn live() -> isize {
	// The group log frees evicted entries through epoch-based reclamation.
	for _ in 0..64 {
		crossbeam_epoch::pin().flush();
	}
	LIVE.load(Ordering::Relaxed)
}

/// Receive one case once, describing the live heap it retains against the pool's bill, the
/// payload retained, and the allocations and bytes allocated per frame received.
fn measure(media: &Media, backing: Backing, keep: (&str, u64, u64)) -> String {
	let before = live();
	let mut cache = Cache::new(media);
	let empty = live();

	ALLOCATIONS.store(0, Ordering::Relaxed);
	ALLOCATED.store(0, Ordering::Relaxed);
	COUNTING.store(true, Ordering::Relaxed);
	cache.run(media, backing, keep);
	COUNTING.store(false, Ordering::Relaxed);

	let retained = live() - empty;
	let used = cache.pool.used();
	let payload = cache.payload();
	drop(cache);
	let left = live() - before;

	// The frames the cache can keep as the chunk they arrived in.
	let whole = media.frames.iter().filter(|frame| frame.chunks.len() == 1).count();
	let frames = media.frames.len();
	let allocations = ALLOCATIONS.load(Ordering::Relaxed) as f64 / frames as f64;
	let allocated = ALLOCATED.load(Ordering::Relaxed) as f64 / frames as f64;
	format!(
		"retained {retained} B, pool {used} B ({:.2}x), payload {payload} B, {whole} of {frames} frames whole, \
		 {allocations:.2} allocations and {allocated:.0} B allocated per frame, {left} B left after drop",
		retained as f64 / used as f64,
	)
}

fn bench(c: &mut Criterion) {
	// Every case's memory, without timing.
	if std::env::var_os("BACKING_ALLOCS").is_some() {
		let filter = std::env::args().skip(1).find(|arg| !arg.starts_with('-'));
		for (name, tracks, kbps, backing, keep) in cases() {
			if filter.as_deref().is_none_or(|filter| name.contains(filter)) {
				println!("{name}: {}", measure(&Media::new(tracks, kbps), backing, keep));
			}
		}
		return;
	}

	// Eviction is a handful of aborts, so only the cases keeping every group are timed, each
	// with its memory printed beside it.
	let mut group = c.benchmark_group("backing");
	for (name, tracks, kbps, backing, keep) in cases().filter(|(.., keep)| keep.0 == "all") {
		let media = Media::new(tracks, kbps);
		eprintln!("backing/{name}: {}", measure(&media, backing, keep));
		group.throughput(Throughput::Bytes(media.payload as u64));
		group.bench_function(BenchmarkId::from_parameter(name), |b| {
			b.iter_batched(
				|| Cache::new(&media),
				|mut cache| {
					cache.run(&media, backing, keep);
					cache
				},
				BatchSize::PerIteration,
			)
		});
	}
	group.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
