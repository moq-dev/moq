//! What cached frames received from the network keep alive beyond the bytes the cache bills.
//!
//! A receiving session hands each frame's payload to the cache as the transport delivered
//! it. A frame that arrives whole in one chunk would be cached as that chunk, a slice of
//! whatever receive allocation the packet was copied into, while a frame spanning packets is
//! copied into storage of its own. The pool bills each frame's length, so a small frame could
//! keep a much larger receive allocation alive, including the other frames' bytes that were
//! copied out of it. The session's frame reader therefore copies a whole frame of at most
//! 2 KiB out of its chunk, and this bench measures every backing beside the control.
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
//! Each case keeps every group it receives.
//!
//! Run with `cargo bench -p moq-net --features fuzz --bench backing`.

use std::fmt;

use bytes::{Bytes, BytesMut};
use moq_net::{Timescale, Timestamp, broadcast, cache, fuzz::GroupRecv, group, track};
use swole::bench;

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
	_pool: cache::Pool,
	_broadcast: broadcast::Producer,
	tracks: Vec<track::Producer>,
	/// Each track's finished groups, held so the cache keeps them.
	groups: Vec<group::Finished>,
}

impl Cache {
	fn new(media: &Media) -> Self {
		let mut info = broadcast::Info::new();
		info.pool = cache::Pool::unbounded();
		let pool = info.pool.clone();
		let broadcast = info.produce();
		let tracks = (0..media.tracks)
			.map(|index| {
				let info = track::Info::default()
					.with_max_age(None)
					.with_timescale(Timescale::MILLI);
				broadcast.create_track(format!("{index}").as_str(), info).unwrap()
			})
			.collect();
		Self {
			_pool: pool,
			_broadcast: broadcast,
			tracks,
			groups: Vec::new(),
		}
	}

	/// Receive every frame.
	fn run(&mut self, media: &Media, backing: Backing) {
		let mut driver = Driver::new(backing);
		let mut open: Vec<Option<GroupRecv>> = (0..media.tracks).map(|_| None).collect();
		for frame in &media.frames {
			let slot = &mut open[frame.track];
			if frame.start {
				if let Some(recv) = slot.take() {
					self.groups.push(recv.finish().unwrap());
				}
				// A group starts at its first frame's timestamp, in the track's milliseconds.
				let group = self.tracks[frame.track]
					.append_group(Timestamp::from_millis(frame.timestamp).unwrap())
					.unwrap();
				*slot = Some(GroupRecv::new(group));
			}
			let recv = slot.as_mut().expect("every track opens with a group");
			let chunks: Vec<_> = frame.chunks.iter().map(|chunk| driver.chunk(media, *chunk)).collect();
			let timestamp = Timestamp::new(frame.timestamp, Timescale::MILLI).unwrap();
			recv.frame(timestamp, chunks).unwrap();
		}
		for recv in open.into_iter().flatten() {
			self.groups.push(recv.finish().unwrap());
		}
	}
}

/// One workload received through one backing, named `tracks=<tracks>/video=<kbps>k/<backing>`.
#[derive(Clone, Copy)]
struct Case {
	tracks: usize,
	kbps: u64,
	backing: Backing,
}

impl Case {
	fn payload(self) -> usize {
		Media::new(self.tracks, self.kbps).payload
	}
}

impl fmt::Display for Case {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "tracks={}/video={}k/{}", self.tracks, self.kbps, self.backing.name())
	}
}

fn cases() -> Vec<Case> {
	TRACKS
		.into_iter()
		.flat_map(|tracks| {
			VIDEO_KBPS.into_iter().flat_map(move |kbps| {
				Backing::ALL
					.into_iter()
					.map(move |backing| Case { tracks, kbps, backing })
			})
		})
		.collect()
}

/// A case's workload and the empty cache it is received into.
struct Receiving {
	media: Media,
	backing: Backing,
	cache: Cache,
}

fn receiving(case: Case) -> Receiving {
	let media = Media::new(case.tracks, case.kbps);
	let cache = Cache::new(&media);
	Receiving {
		media,
		backing: case.backing,
		cache,
	}
}

/// Returns what it received, so tearing the cache down stays unmeasured.
#[bench(args = cases(), setup = receiving, bytes = Case::payload)]
fn backing(mut input: Receiving) -> Receiving {
	input.cache.run(&input.media, input.backing);
	input
}

fn main() {
	swole::main();
}
