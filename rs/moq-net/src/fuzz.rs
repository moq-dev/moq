//! Fuzz target bodies for the wire codecs, plus the seeds they start from.
//!
//! Each `pub fn` here is one target: it takes raw fuzzer bytes, drives a decoder, and
//! checks that whatever came out survives a re-encode. The bodies live in the crate
//! rather than in `fuzz/fuzz_targets/` because the `lite` and `ietf` modules are
//! private modules, so an outside harness cannot reach a single decoder. Keeping them
//! here also lets the tests below replay them on stable, which is what turns a crash
//! the fuzzer found into a regression `just check` runs.
//!
//! Compiled only under `cfg(test)` or the `fuzz` feature, so none of this is part of
//! the published API. See `fuzz/README.md` for the workflow.

use bytes::Bytes;

use crate::{
	Hops, Path, Pattern, broadcast, cache, coding,
	coding::{Decode, Decoder, Encode, Encoder, Form, varint},
	frame, group, ietf, lite,
	path::Relative,
	track,
};

/// One fuzz target body: it returns whether the input decoded, which is what
/// [`seeds`] is checked against.
pub type Target = fn(&[u8]) -> bool;

/// Build a cached group with optional private object fields for model benchmarks.
pub fn object_group(frames: usize, properties: bool) -> (broadcast::Producer, track::Producer, group::Producer) {
	let broadcast = broadcast::Producer::new(broadcast::Info::default());
	// Object properties come from a moq-transport publisher, whose draft serialized them.
	let info = track::Info {
		properties: properties.then(|| {
			std::sync::Arc::new(crate::model::object::TrackProperties {
				version: ietf::Version::Draft22,
				values: Vec::new(),
			})
		}),
		..Default::default()
	};
	let track = broadcast.create_track("objects", info).unwrap();
	let mut group = track.append_group().unwrap();
	let payload = Bytes::from_static(&[0; 64]);
	for id in 0..frames {
		let fields = properties.then(|| {
			std::sync::Arc::new(crate::model::object::Fields {
				id: id as u64,
				properties: crate::model::object::Properties {
					block: crate::ietf::object::block(
						&[crate::model::object::Property {
							id: 0x80,
							value: crate::model::object::Value::Int(96_000),
						}],
						ietf::Version::Draft22,
					)
					.unwrap(),
					..Default::default()
				},
			})
		});
		let header = frame::Header {
			info: frame::Info {
				size: payload.len() as u64,
				timestamp: None,
			},
			fields,
			inflated: None,
		};
		let mut frame = group.create_object(header).unwrap();
		frame.write(payload.clone()).unwrap();
		group = frame.finish().unwrap();
	}
	group.finish().unwrap();
	(broadcast, track, group)
}

/// Private canonical-owner fixture for size and final-storage cleanup benchmarks.
pub struct CanonicalStorage(crate::model::group::StorageBench);

impl CanonicalStorage {
	pub fn new(frames: usize, views: usize) -> Self {
		Self(crate::model::group::StorageBench::new(frames, views))
	}
	pub fn size(&self) -> u64 {
		self.0.size()
	}
}

/// Every target, keyed by the name of its `fuzz_targets/<name>.rs` shim.
pub const TARGETS: &[(&str, Target)] = &[
	("lite", lite_wire),
	("announce", announce_stream),
	("ietf", ietf_wire),
	("varint", varint),
	("path", path),
	("pattern", pattern),
];

/// The moq-lite versions a target decodes at, selected by the input's first byte.
const LITE_VERSIONS: &[lite::Version] = &[
	lite::Version::Lite01,
	lite::Version::Lite02,
	lite::Version::Lite03,
	lite::Version::Lite04,
	lite::Version::Lite05,
	lite::Version::Lite06,
	lite::Version::Lite07,
];

/// The moq-transport drafts a target decodes at, selected by the input's first byte.
const IETF_VERSIONS: &[ietf::Version] = &[ietf::Version::Draft18, ietf::Version::Draft22];

/// How many types [`lite_wire`] dispatches over.
const LITE_KINDS: u8 = 30;

/// How many types [`ietf_wire`] dispatches over.
const IETF_KINDS: u8 = 27;

/// Split the two selector bytes off the input: a version and a type.
fn select(data: &[u8], versions: usize) -> Option<(usize, u8, &[u8])> {
	let (&version, rest) = data.split_first()?;
	let (&kind, rest) = rest.split_first()?;
	Some((version as usize % versions, kind, rest))
}

/// Decode a `T`, then check that our own encoding of it reads back as itself.
///
/// Returns whether the input decoded.
///
/// An encode failure is a legitimate outcome rather than a finding: a value can decode
/// at a version that cannot express it again (a duration past the varint range, a
/// message a later draft dropped). What is never legitimate is emitting bytes we then
/// refuse, or refuse to consume in full, since the peer's decoder is this same code.
/// The second encoding must also match the first byte for byte.
///
/// `scratch` holds our own encoding while the second decode borrows it, as the first
/// borrows `data`.
fn roundtrip<'a, T, V>(data: &'a [u8], scratch: &'a mut Vec<u8>, version: V) -> bool
where
	T: Decode<'a, V> + Encode<V>,
	V: Copy + Into<Form>,
{
	let mut reader = Decoder::new(data, version.into());
	let decoded = match T::decode(&mut reader, version) {
		Ok(decoded) => decoded,
		Err(coding::DecodeError::Short) => {
			assert!(reader.need() > data.len(), "short read inside a complete body");
			return false;
		}
		Err(_) => return false,
	};

	let Ok(first) = decoded.encode_bytes(version) else {
		return true;
	};

	scratch.extend_from_slice(&first);
	let scratch: &'a [u8] = scratch;
	let (decoded, used) = T::decode_slice(scratch, version).expect("could not decode our own encoding");
	assert_eq!(used, first.len(), "our own encoding left {} bytes", first.len() - used);

	let Ok(second) = decoded.encode_bytes(version) else {
		panic!("could not re-encode what we just encoded");
	};
	assert_eq!(first, second, "encoding is not stable");

	true
}

/// Assert the invariant every path holds once built: its spelling parses back to it under
/// the strict constructor, and so does every run of its leading segments, which is what
/// stats grouping and announce compression derive from it. Only a run ending at an
/// escaped `/` has no head, since it would end in half an escape.
fn assert_strict(path: &Path) {
	let reparsed = Path::new(path.as_str()).unwrap_or_else(|err| panic!("{path:?} is refused: {err}"));
	assert_eq!(reparsed, *path, "{path:?} did not parse back to itself");
	let count = path.parts().count();
	for n in 0..=count {
		let cut = path.parts().take(n).collect::<Vec<_>>().join("/");
		match path.head(n) {
			Some(head) => assert_eq!(Path::new(&cut), Ok(head), "{path:?} has a wrong head at {n}"),
			None => assert!(cut.ends_with('\\'), "{path:?} has no head at {n}"),
		}
	}
	assert!(path.head(count + 1).is_none(), "{path:?} has a head past its segments");
}

/// Read the input as a moq-lite path, whatever its kind, and hold it to [`assert_strict`].
fn lite_path(data: &[u8], version: lite::Version) {
	if let Ok((path, _)) = Path::decode_slice(data, version) {
		assert_strict(&path);
	}
}

/// Read the input as a namespace tuple and as a Full Track Name, whatever its kind, and
/// hold each path to [`assert_strict`] and the namespace to the wire round trip.
fn ietf_paths(data: &[u8], version: ietf::Version) {
	let form: Form = version.into();
	if let Ok(path) = ietf::namespace::decode_namespace(&mut Decoder::new(data, form)) {
		assert_strict(&path);
		let mut encoded = Vec::new();
		ietf::namespace::encode_namespace(&mut Encoder::new(&mut encoded, form), &path)
			.expect("could not encode a decoded namespace");
		let echo = ietf::namespace::decode_namespace(&mut Decoder::new(&encoded, form))
			.expect("could not decode our own namespace");
		assert_eq!(echo, path, "namespace did not survive a round trip");
	}
	if let Ok((path, _)) = ietf::namespace::decode_track(&mut Decoder::new(data, form)) {
		assert_strict(&path);
	}
}

/// [`roundtrip`] for a datagram body, which runs to the end of its buffer.
fn datagram(data: &[u8], version: lite::Version) -> bool {
	// The body doesn't say whether it carries a timestamp or an inflated size, whether the
	// session negotiated the `ietf` extension, or which draft its track came from, so try
	// every reading.
	let negotiated: &[bool] = match version.has_ietf() {
		true => &[false, true],
		false => &[false],
	};
	let mut any = false;
	for &negotiated in negotiated {
		for timescale in [Some(crate::Timescale::MILLI), None] {
			for ietf in [None, Some(ietf::Version::Draft18), Some(ietf::Version::Draft22)] {
				for compressed in [false, true] {
					let framing = lite::Framing {
						timescale,
						ietf,
						compressed,
					};
					let Ok(decoded) =
						lite::Datagram::decode(Bytes::copy_from_slice(data), version, negotiated, |_| framing)
					else {
						continue;
					};
					any = true;

					let Ok(first) = decoded.encode_bytes(version) else {
						continue;
					};

					let echo = lite::Datagram::decode(first, version, negotiated, |_| framing)
						.expect("could not decode our own encoding");
					assert_eq!(echo, decoded, "datagram did not survive a round trip");
				}
			}
		}
	}
	any
}

/// Decode one moq-lite wire object from arbitrary bytes.
///
/// Byte 0 picks the negotiated version, byte 1 the object type, and the rest is the
/// encoding, size prefix included for the types that carry one.
pub fn lite_wire(data: &[u8]) -> bool {
	let Some((version, kind, rest)) = select(data, LITE_VERSIONS.len()) else {
		return false;
	};
	let version = LITE_VERSIONS[version];
	let kind = kind % LITE_KINDS;
	lite_path(rest, version);

	match kind {
		0 => roundtrip::<lite::Setup, _>(rest, &mut Vec::new(), version),
		1 => roundtrip::<lite::SessionInfo, _>(rest, &mut Vec::new(), version),
		2 => roundtrip::<lite::AnnounceInit, _>(rest, &mut Vec::new(), version),
		3 => roundtrip::<lite::AnnounceRequest, _>(rest, &mut Vec::new(), version),
		4 => roundtrip::<lite::AnnounceOk, _>(rest, &mut Vec::new(), version),
		5 => roundtrip::<lite::AnnounceBroadcast, _>(rest, &mut Vec::new(), version),
		6 => roundtrip::<lite::Subscribe<'_>, _>(rest, &mut Vec::new(), version),
		7 => roundtrip::<lite::SubscribeOk, _>(rest, &mut Vec::new(), version),
		8 => roundtrip::<lite::SubscribeStart, _>(rest, &mut Vec::new(), version),
		9 => roundtrip::<lite::SubscribeEnd, _>(rest, &mut Vec::new(), version),
		10 => roundtrip::<lite::SubscribeUpdate, _>(rest, &mut Vec::new(), version),
		11 => roundtrip::<lite::SubscribeDrop, _>(rest, &mut Vec::new(), version),
		12 => roundtrip::<lite::SubscribeResponse, _>(rest, &mut Vec::new(), version),
		13 => roundtrip::<lite::Fetch<'_>, _>(rest, &mut Vec::new(), version),
		14 => roundtrip::<lite::Group, _>(rest, &mut Vec::new(), version),
		15 => roundtrip::<lite::Goaway<'_>, _>(rest, &mut Vec::new(), version),
		16 => roundtrip::<lite::Track<'_>, _>(rest, &mut Vec::new(), version),
		17 => roundtrip::<lite::TrackInfo, _>(rest, &mut Vec::new(), version),
		18 => roundtrip::<lite::Probe, _>(rest, &mut Vec::new(), version),
		19 => datagram(rest, version),
		20 => roundtrip::<lite::Parameters, _>(rest, &mut Vec::new(), version),
		21 => roundtrip::<lite::ietf::TrackProperties, _>(rest, &mut Vec::new(), version),
		22 => roundtrip::<lite::Tracks, _>(rest, &mut Vec::new(), version),
		23 => roundtrip::<lite::TracksEvent<'_>, _>(rest, &mut Vec::new(), version),
		24 => roundtrip::<lite::Auth, _>(rest, &mut Vec::new(), version),
		25 => roundtrip::<lite::AuthOk, _>(rest, &mut Vec::new(), version),
		26 => roundtrip::<lite::AuthError, _>(rest, &mut Vec::new(), version),
		27 => roundtrip::<lite::AuthReply, _>(rest, &mut Vec::new(), version),
		28 => roundtrip::<crate::setup::Client, _>(rest, &mut Vec::new(), crate::Version::Lite(version)),
		29 => roundtrip::<crate::setup::Server, _>(rest, &mut Vec::new(), crate::Version::Lite(version)),
		_ => unreachable!("kind is taken modulo LITE_KINDS"),
	}
}

/// One announcement on an announce stream, resolved: what the application sees.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Announced {
	/// ANNOUNCE_START: a route's suffix and wire hop chain, taking the next id.
	Start(Path, Hops),
	/// ANNOUNCE_UPDATE: a live id's new wire hop chain.
	Update(u64, Hops),
	/// ANNOUNCE_END: a live id retired.
	End(u64),
}

fn announce_version(compress: bool) -> lite::Version {
	match compress {
		true => lite::Version::Lite07,
		false => lite::Version::Lite06,
	}
}

/// One publisher's side of an announce stream: lite-07 with compression, or lite-06
/// literal framing.
pub struct AnnounceWriter {
	version: lite::Version,
	encoder: lite::AnnounceEncoder,
}

impl AnnounceWriter {
	/// A new stream, with nothing live.
	pub fn new(compress: bool) -> Self {
		let version = announce_version(compress);
		Self {
			version,
			encoder: lite::AnnounceEncoder::new(version),
		}
	}

	/// Append one announcement to `data`. An update or end must name a live id.
	pub fn write(&mut self, announced: &Announced, data: &mut Vec<u8>) {
		let msg = match announced {
			Announced::Start(suffix, hops) => {
				let (_, suffix, hops) = self.encoder.start(suffix.clone(), hops.clone());
				lite::AnnounceBroadcast::Active {
					epoch: None,
					suffix,
					hops,
					cost: Default::default(),
				}
			}
			Announced::Update(id, hops) => lite::AnnounceBroadcast::Update {
				id: *id,
				hops: self.encoder.update(*id, hops.clone()),
				cost: Default::default(),
			},
			Announced::End(id) => {
				self.encoder.end(*id);
				lite::AnnounceBroadcast::EndedId { id: *id }
			}
		};
		msg.encode(&mut Encoder::new(data, self.version.into()), self.version)
			.expect("could not encode an announcement");
	}
}

/// Mark `track` as a copy received from a remote publisher, as a subscriber session does, for
/// the tail bench.
pub fn received(track: &track::Producer) {
	track.received();
}

/// Fill the `groups` of `track` its cache misses, a run per FETCH, as a publisher answers a
/// range FETCH, for the session bench. The returned guard keeps the filled groups cached.
pub async fn fill_groups(track: &track::Consumer, groups: std::ops::Range<u64>) -> crate::Result<impl Sized> {
	track.info().await?;
	Ok(track.fill_groups(groups, 0).await)
}

/// `origin` as a session connected to `peer` serves it, for the viewers bench.
pub fn excluding(origin: crate::origin::Consumer, peer: crate::Hop) -> crate::origin::Consumer {
	origin.excluding(peer)
}

/// Land a claim with a peer's wire route, as a session does, for the origin bench.
pub fn requests_route(
	origin: &crate::origin::Producer,
	prefix: impl Into<crate::Path>,
	route: crate::origin::Route,
) -> Result<crate::origin::Requests, crate::Error> {
	origin.requests_route(prefix, route)
}

/// Replace a peer claim's route in place, as a session does on a route update.
pub fn update_route(dynamic: &crate::origin::Requests, route: crate::origin::Route) -> Result<(), crate::Error> {
	dynamic.update_route(route)
}

/// Answer a peer claim's request with the broadcast its session already holds.
pub fn accept(request: crate::origin::Request, broadcast: impl crate::Consume<broadcast::Consumer>) {
	request.accept(broadcast)
}

/// A session's snapshot of its peer's initial announcements, for the synced bench.
/// Drop it to land the snapshot.
pub struct Snapshot(#[allow(dead_code, reason = "held for its drop, which lands the snapshot")] crate::model::Snapshot);

/// Start a snapshot under `prefix`, as a session requesting announcements there does.
pub fn snapshot(origin: &crate::origin::Producer, prefix: &str) -> Snapshot {
	Snapshot(origin.snapshot(prefix))
}

/// What a client session fed into an origin hears it should ask its peer about, for
/// the interests bench.
pub struct Interests(crate::model::Interests);

/// Register as a client session fed into `origin` does.
pub fn interests(origin: &crate::origin::Producer) -> Interests {
	Interests(origin.interests())
}

impl Interests {
	/// Take every change heard so far, returning how many there were.
	pub fn drain(&self) -> usize {
		let mut changes = 0;
		while let std::task::Poll::Ready(Some(_)) = self.0.poll_next(&kio::Waiter::noop()) {
			changes += 1;
		}
		changes
	}
}

/// Add `n` publisher-side payload bytes on `(path, tier)`, creating the entry
/// when the path is tracked. The stats producer bench marks a share of held
/// paths changed this way, without a media frame. A later [`crate::stats::Registry::report`]
/// drops the entry unless some guard still holds it. Not a public API.
pub fn bump_publisher_bytes(
	registry: &crate::stats::Registry,
	path: impl Into<crate::Path>,
	tier: &crate::stats::Tier,
	n: u64,
) {
	registry.bump_publisher_bytes(path, tier, n);
}

/// An allocator's probe target as its session follows it, for the bandwidth bench.
pub struct ProbeTarget(crate::bandwidth::Target);

impl ProbeTarget {
	/// Follow `allocator`'s target.
	pub fn new(allocator: &crate::bandwidth::Allocator) -> Self {
		Self(allocator.target())
	}

	/// The target in bits per second once it moved, without parking.
	pub fn poll(&mut self) -> Option<u64> {
		match self.0.poll_changed(&kio::Waiter::noop()) {
			std::task::Poll::Ready(target) => Some(target.as_bps()),
			std::task::Poll::Pending => None,
		}
	}
}

/// A payload-byte meter for the stats write-path benchmark.
pub struct Meter(crate::stats::Meter);

impl Meter {
	/// Add publisher-side payload bytes.
	pub fn bytes(&self, n: u64) {
		self.0.bytes(n);
	}
}

/// Build a session-local payload meter without exposing the model's private stats carriers.
pub fn session_meter(registry: &crate::stats::Registry, tier: crate::stats::Tier, label: &str, path: &str) -> Meter {
	let session = registry.tier(tier).session(Path::new(label).expect("label fits"));
	Meter(session.egress(Path::new(path).expect("path fits")).meter())
}

/// Build the original per-path-only payload meter as the benchmark baseline.
pub fn path_meter(registry: &crate::stats::Registry, tier: &crate::stats::Tier, path: &str) -> Meter {
	Meter(registry.publisher_meter(Path::new(path).expect("path fits"), tier))
}

/// Encode `announced` as a fresh [`AnnounceWriter`] stream.
pub fn encode_announces(announced: &[Announced], compress: bool) -> Vec<u8> {
	let mut writer = AnnounceWriter::new(compress);
	let mut data = Vec::new();
	for announced in announced {
		writer.write(announced, &mut data);
	}
	data
}

/// Decode and resolve an announce stream, as [`encode_announces`] writes it, stopping
/// at the first message that fails to decode or resolve: a subscriber closes the
/// session there.
pub fn decode_announces(data: &[u8], compress: bool) -> Vec<Announced> {
	let version = announce_version(compress);
	let mut data = Decoder::new(data, version.into());
	let mut decoder = lite::AnnounceDecoder::default();
	let mut resolved = Vec::new();
	while let Ok(msg) = lite::AnnounceBroadcast::decode(&mut data, version) {
		let announced = match msg {
			lite::AnnounceBroadcast::Active { suffix, hops, .. } => decoder
				.start(suffix, hops)
				.map(|(suffix, hops)| Announced::Start(suffix, hops)),
			lite::AnnounceBroadcast::Update { id, hops, .. } => {
				decoder.update(id, hops).map(|(_, hops)| Announced::Update(id, hops))
			}
			lite::AnnounceBroadcast::EndedId { id } => decoder.end(id).map(|_| Announced::End(id)),
			_ => continue,
		};
		let Ok(announced) = announced else {
			break;
		};
		resolved.push(announced);
	}
	resolved
}

/// Encode a moq-transport NAMESPACE message for each of `paths`.
pub fn encode_namespaces(paths: &[Path]) -> Vec<u8> {
	let mut data = Vec::new();
	let w = &mut Encoder::new(&mut data, BENCH_IETF.into());
	for path in paths {
		let msg = ietf::Namespace {
			suffix: path.clone(),
			params: None,
		};
		msg.encode(w, BENCH_IETF).expect("a namespace that fits");
	}
	data
}

/// Decode the NAMESPACE messages [`encode_namespaces`] wrote.
pub fn decode_namespaces(data: &[u8]) -> Vec<Path> {
	let r = &mut Decoder::new(data, BENCH_IETF.into());
	let mut paths = Vec::new();
	while !r.is_empty() {
		paths.push(ietf::Namespace::decode(r, BENCH_IETF).expect("our own encoding").suffix);
	}
	paths
}

/// A varint-heavy lite wire object, as the varint bench times it: what a publisher
/// pays per frame, per group, and per request.
#[derive(Clone, Copy, Debug)]
pub enum LiteSample {
	/// A 30 fps video group's FRAME headers: a zigzag timestamp delta in microseconds and
	/// a size, 60 of them, led by a 60 KB keyframe. Payloads are left out.
	Video,
	/// A 50 Hz Opus group's FRAME headers: 50 of 20 ms and 160 bytes.
	Audio,
	/// A GROUP header, well into a long-running track.
	Group,
	/// A SUBSCRIBE for a track, the way a player opens one.
	Subscribe,
	/// A datagram's header, with an empty payload.
	Datagram,
	/// A SETUP declaring a probe level, a cost, and a random Hop ID.
	Setup,
}

impl LiteSample {
	/// Every sample, in the order the bench reports them.
	pub const ALL: [Self; 6] = [
		Self::Video,
		Self::Audio,
		Self::Group,
		Self::Subscribe,
		Self::Datagram,
		Self::Setup,
	];

	/// The FRAME headers for [`Self::Video`] and [`Self::Audio`], as (delta, size) pairs.
	fn frames(self) -> Vec<(i64, u64)> {
		match self {
			Self::Video => (0..60)
				.map(|n| match n {
					0 => (0, 60_000),
					n if n % 10 == 0 => (33_333, 17_000),
					_ => (33_333, 8_000),
				})
				.collect(),
			Self::Audio => (0..50).map(|n| (if n == 0 { 0 } else { 20_000 }, 160)).collect(),
			_ => Vec::new(),
		}
	}

	/// Encode this sample at `version`, which must be a moq-lite version.
	pub fn encode(self, version: crate::Version) -> Vec<u8> {
		let version = lite::Version::try_from(version).expect("a moq-lite version");
		let mut buf = Vec::new();
		let w = &mut Encoder::new(&mut buf, version.into());
		match self {
			Self::Video | Self::Audio => {
				for (delta, size) in self.frames() {
					w.varint(varint::zigzag(delta)).unwrap();
					w.varint(size).unwrap();
				}
			}
			Self::Group => lite::Group {
				subscribe: 3,
				sequence: 1_234,
				frame_start: 0,
			}
			.encode(w, version)
			.unwrap(),
			Self::Subscribe => lite::Subscribe {
				order: crate::track::Order::Descending,
				epoch: None,
				id: 3,
				broadcast: Path::from_static("room/alice"),
				track: "video".into(),
				priority: 2,
				max_delay: std::time::Duration::from_secs(10),
				ranges: vec![lite::Range::OLDEST],
			}
			.encode(w, version)
			.unwrap(),
			Self::Datagram => lite::Datagram {
				subscribe: 3,
				sequence: 1_234,
				timestamp: Some(1_234_567_890),
				inflated: None,
				payload: bytes::Bytes::new(),
				ietf: None,
			}
			.encode(w, version)
			.unwrap(),
			Self::Setup => lite::Setup {
				probe: lite::ProbeLevel::Report,
				cost: Some(1),
				hop: Some(crate::Hop::new(0x1d_2c3b_4a59_6877).unwrap()),
				..Default::default()
			}
			.encode(w, version)
			.unwrap(),
		}
		buf
	}

	/// Decode what [`Self::encode`] wrote at `version`, returning how many objects it read.
	pub fn decode(self, version: crate::Version, data: &[u8]) -> usize {
		let version = lite::Version::try_from(version).expect("a moq-lite version");
		let r = &mut Decoder::new(data, version.into());
		match self {
			Self::Video | Self::Audio => {
				let mut frames = 0;
				while !r.is_empty() {
					r.varint().unwrap();
					r.varint().unwrap();
					frames += 1;
				}
				frames
			}
			Self::Group => lite::Group::decode(r, version).map(|_| 1).unwrap(),
			Self::Subscribe => lite::Subscribe::decode(r, version).map(|_| 1).unwrap(),
			Self::Datagram => {
				lite::Datagram::decode(bytes::Bytes::copy_from_slice(data), version, false, |_| lite::Framing {
					timescale: Some(crate::Timescale::MILLI),
					ietf: None,
					compressed: false,
				})
				.map(|_| 1)
				.unwrap()
			}
			Self::Setup => lite::Setup::decode(r, version).map(|_| 1).unwrap(),
		}
	}
}

/// Feed a lite-07 announce stream through the stateful decoder, then check that our
/// encoder's compression of what it resolved reads back as the same announcements.
///
/// Returns whether anything resolved.
pub fn announce_stream(data: &[u8]) -> bool {
	let resolved = decode_announces(data, true);
	for announced in &resolved {
		if let Announced::Start(suffix, _) = announced {
			assert_strict(suffix);
		}
	}
	let echo = decode_announces(&encode_announces(&resolved, true), true);
	assert_eq!(echo, resolved, "compression did not survive a round trip");
	!resolved.is_empty()
}

/// Decode one IETF moq-transport wire object from arbitrary bytes.
///
/// Byte 0 picks the negotiated draft, byte 1 the object type, and the rest is the
/// encoding, size prefix included for the types that carry one.
pub fn ietf_wire(data: &[u8]) -> bool {
	let Some((version, kind, rest)) = select(data, IETF_VERSIONS.len()) else {
		return false;
	};
	let version = IETF_VERSIONS[version];
	let kind = kind % IETF_KINDS;
	ietf_paths(rest, version);

	match kind {
		0 => roundtrip::<ietf::GoAway<'_>, _>(rest, &mut Vec::new(), version),
		1 => roundtrip::<ietf::Fetch<'_>, _>(rest, &mut Vec::new(), version),
		2 => roundtrip::<ietf::FetchOk, _>(rest, &mut Vec::new(), version),
		3 => roundtrip::<ietf::FetchHeader, _>(rest, &mut Vec::new(), version),
		4 => roundtrip::<ietf::FetchType<'_>, _>(rest, &mut Vec::new(), version),
		5 => roundtrip::<ietf::PublishNamespace, _>(rest, &mut Vec::new(), version),
		6 => roundtrip::<ietf::TrackStatus<'_>, _>(rest, &mut Vec::new(), version),
		7 => roundtrip::<ietf::Publish<'_>, _>(rest, &mut Vec::new(), version),
		8 => roundtrip::<ietf::PublishOk, _>(rest, &mut Vec::new(), version),
		9 => roundtrip::<ietf::PublishDone<'_>, _>(rest, &mut Vec::new(), version),
		10 => roundtrip::<ietf::PublishSkipped<'_>, _>(rest, &mut Vec::new(), version),
		11 => roundtrip::<ietf::Subscribe<'_>, _>(rest, &mut Vec::new(), version),
		12 => roundtrip::<ietf::SubscribeOk, _>(rest, &mut Vec::new(), version),
		13 => roundtrip::<ietf::SubscribeUpdate, _>(rest, &mut Vec::new(), version),
		14 => roundtrip::<ietf::SubscribeNamespace, _>(rest, &mut Vec::new(), version),
		15 => roundtrip::<ietf::Namespace, _>(rest, &mut Vec::new(), version),
		16 => roundtrip::<ietf::NamespaceDone, _>(rest, &mut Vec::new(), version),
		17 => roundtrip::<ietf::RequestOk, _>(rest, &mut Vec::new(), version),
		18 => roundtrip::<ietf::RequestError<'_>, _>(rest, &mut Vec::new(), version),
		19 => roundtrip::<ietf::GroupHeader, _>(rest, &mut Vec::new(), version),
		20 => roundtrip::<ietf::Parameters, _>(rest, &mut Vec::new(), version),
		21 => roundtrip::<ietf::Location, _>(rest, &mut Vec::new(), version),
		22 => roundtrip::<ietf::FetchObject, _>(rest, &mut Vec::new(), version),
		23 => roundtrip::<ietf::PublishNamespaceUpdate, _>(rest, &mut Vec::new(), version),
		24 => roundtrip::<ietf::ObjectDatagram, _>(rest, &mut Vec::new(), version),
		25 => roundtrip::<ietf::SubscribeTracks, _>(rest, &mut Vec::new(), version),
		26 => setup::<crate::setup::Setup>(rest, &mut Vec::new(), version, |setup| &setup.parameters),
		_ => unreachable!("kind is taken modulo IETF_KINDS"),
	}
}

/// [`roundtrip`] for a SETUP envelope, plus the Setup Options inside it read the way a
/// session reads them.
///
/// A server may not send PATH or AUTHORITY but is otherwise held to the same options, so
/// any block a client accepts from its server, a server accepts from its client.
fn setup<'a, T>(data: &'a [u8], scratch: &'a mut Vec<u8>, version: ietf::Version, parameters: fn(&T) -> &Bytes) -> bool
where
	T: Decode<'a, crate::Version> + Encode<crate::Version>,
{
	let outer = crate::Version::Ietf(version);
	if let Ok((message, _)) = T::decode_slice(data, outer) {
		let parameters = parameters(&message);
		if ietf::decode_peer_setup(parameters.clone(), version, true).is_ok() {
			assert!(
				ietf::decode_peer_setup(parameters.clone(), version, false).is_ok(),
				"a server's Setup Options are refused from a client"
			);
		}
	}
	roundtrip::<T, _>(data, scratch, outer)
}

/// Decode a varint with whichever codec the selected version uses.
///
/// Byte 0 picks the version, which is the whole point: lite-01 to lite-06 use the QUIC
/// two-bit length tag, while lite-07 and the moq-transport drafts count leading ones, and
/// the two disagree about which byte sequences are even legal.
///
/// The leading-ones form spans the full `u64`, while the QUIC form stops at
/// `2^62 - 1`; a value always re-encodes in the form it was read in.
pub fn varint(data: &[u8]) -> bool {
	let Some((&selector, rest)) = data.split_first() else {
		return false;
	};

	// Half the space to each family, so both codecs get exercised.
	let version: crate::Version = match selector % 2 {
		0 => LITE_VERSIONS[(selector as usize / 2) % LITE_VERSIONS.len()].into(),
		_ => IETF_VERSIONS[(selector as usize / 2) % IETF_VERSIONS.len()].into(),
	};

	let Ok(value) = Decoder::new(rest, version.into()).varint() else {
		return false;
	};

	// Zigzag is a bijection on top of the wire value, so it must round-trip.
	let signed = varint::unzigzag(value);
	assert_eq!(varint::zigzag(signed), value, "zigzag is not its own inverse");

	let mut encoded = Vec::new();
	Encoder::new(&mut encoded, version.into())
		.varint(value)
		.expect("a varint re-encodes in the form it was read in");
	let mut echo = Decoder::new(&encoded, version.into());
	let again = echo.varint().expect("could not decode our own encoding");
	assert!(echo.is_empty(), "our own encoding left {} bytes", echo.remaining());
	assert_eq!(value, again, "varint did not survive a round trip");

	true
}

/// A fixed mix of control and data-stream messages, for the codec benchmark.
///
/// Encoding appends every message to a buffer; decoding reads them back in the same
/// order. The mix leans on the messages every subscription pays for.
pub struct Messages {
	lite_subscribe: lite::Subscribe<'static>,
	lite_update: lite::SubscribeUpdate,
	lite_start: lite::SubscribeResponse,
	lite_info: lite::TrackInfo,
	lite_group: lite::Group,
	ietf_subscribe: ietf::Subscribe<'static>,
	ietf_ok: ietf::SubscribeOk,
	ietf_group: ietf::GroupHeader,
}

impl Default for Messages {
	fn default() -> Self {
		use std::time::Duration;

		Self {
			lite_subscribe: lite::Subscribe {
				order: crate::track::Order::Descending,
				epoch: None,
				id: 7,
				broadcast: Path::from_static("room/alice"),
				track: "video".into(),
				priority: 3,
				max_delay: Duration::from_millis(500),
				ranges: vec![lite::Range {
					start: Some(lite::Anchor::Absolute(crate::track::Position::group(1_000))),
					end: None,
				}],
			},
			lite_update: lite::SubscribeUpdate {
				wanted_group: None,
				order: crate::track::Order::Descending,
				priority: 4,
				max_delay: Duration::from_millis(500),
				ranges: vec![lite::Range {
					start: Some(lite::Anchor::Absolute(crate::track::Position::group(1_000))),
					end: Some(lite::Anchor::Absolute(crate::track::Position::group(2_001))),
				}],
			},
			lite_start: lite::SubscribeResponse::Start(lite::SubscribeStart {
				largest: None,
				starts: vec![Some(1_234)],
			}),
			lite_info: lite::TrackInfo {
				refresh: None,
				priority: 1,
				max_age: Some(Duration::from_secs(10)),
				timescale: Some(crate::Timescale::MICRO),
				compress: false,
			},
			lite_group: lite::Group {
				subscribe: 7,
				sequence: 123_456,
				frame_start: 0,
			},
			ietf_subscribe: ietf::Subscribe {
				max_age: None,
				new_group: None,
				request_id: ietf::RequestId(2),
				track_namespace: Path::from_static("room/alice"),
				track_name: "video".into(),
				subscriber_priority: 128,
				group_order: ietf::GroupOrder::Descending,
				filter: ietf::Filter::NextObject,
				fill: None,
				properties_wanted: false,
				forward: true,
				rendezvous: false,
				range_filters: false,
				authorization_token: None,
				epoch: None,
			},
			ietf_ok: ietf::SubscribeOk {
				track_alias: 5,
				largest: Some(ietf::Location {
					group: 1_000,
					object: 3,
				}),
				properties: Default::default(),
			},
			ietf_group: ietf::GroupHeader {
				track_alias: 5,
				group_id: 1_000,
				sub_group_id: 0,
				publisher_priority: 128,
				flags: Default::default(),
			},
		}
	}
}

/// The moq-lite version the [`Messages`] mix is encoded at.
const BENCH_LITE: lite::Version = lite::Version::Lite06;

/// The moq-transport draft the [`Messages`] mix is encoded at: a leading-ones varint draft.
const BENCH_IETF: ietf::Version = ietf::Version::Draft22;

impl Messages {
	/// Append the moq-lite messages to `out`.
	pub fn encode_lite(&self, out: &mut Vec<u8>) {
		let v = BENCH_LITE;
		let w = &mut Encoder::new(out, v.into());
		self.lite_subscribe.encode(w, v).unwrap();
		self.lite_update.encode(w, v).unwrap();
		self.lite_start.encode(w, v).unwrap();
		self.lite_info.encode(w, v).unwrap();
		self.lite_group.encode(w, v).unwrap();
	}

	/// Decode what [`Self::encode_lite`] wrote.
	pub fn decode_lite(&self, data: &[u8]) {
		let v = BENCH_LITE;
		let r = &mut Decoder::new(data, v.into());
		lite::Subscribe::decode(r, v).unwrap();
		lite::SubscribeUpdate::decode(r, v).unwrap();
		lite::SubscribeResponse::decode(r, v).unwrap();
		lite::TrackInfo::decode(r, v).unwrap();
		lite::Group::decode(r, v).unwrap();
		assert!(r.is_empty());
	}

	/// Append the moq-transport messages to `out`.
	pub fn encode_ietf(&self, out: &mut Vec<u8>) {
		let v = BENCH_IETF;
		let w = &mut Encoder::new(out, v.into());
		self.ietf_subscribe.encode(w, v).unwrap();
		self.ietf_ok.encode(w, v).unwrap();
		self.ietf_group.encode(w, v).unwrap();
	}

	/// Decode what [`Self::encode_ietf`] wrote.
	pub fn decode_ietf(&self, data: &[u8]) {
		let v = BENCH_IETF;
		let r = &mut Decoder::new(data, v.into());
		ietf::Subscribe::decode(r, v).unwrap();
		ietf::SubscribeOk::decode(r, v).unwrap();
		ietf::GroupHeader::decode(r, v).unwrap();
		assert!(r.is_empty());
	}
}

/// The varint form of moq-lite (`ietf == false`) or of a leading-ones moq-transport draft.
fn bench_form(ietf: bool) -> Form {
	match ietf {
		false => BENCH_LITE.into(),
		true => BENCH_IETF.into(),
	}
}

/// Append `values` as varints in the wire form of moq-lite (`ietf == false`) or of a
/// leading-ones moq-transport draft.
pub fn encode_varints(values: &[u64], ietf: bool, out: &mut Vec<u8>) {
	let mut w = Encoder::new(out, bench_form(ietf));
	for value in values {
		w.varint(*value).unwrap();
	}
}

/// Sum the varints [`encode_varints`] wrote.
pub fn decode_varints(data: &[u8], ietf: bool) -> u64 {
	let mut r = Decoder::new(data, bench_form(ietf));
	let mut sum = 0u64;
	while !r.is_empty() {
		sum = sum.wrapping_add(r.varint().unwrap());
	}
	sum
}

/// Decode one broadcast path as moq-lite carries it (`ietf == false`), a string, or as
/// a moq-transport namespace tuple.
pub fn decode_path(data: &[u8], ietf: bool) -> Path {
	let r = &mut Decoder::new(data, bench_form(ietf));
	let path = match ietf {
		false => Path::decode(r, BENCH_LITE).unwrap(),
		true => ietf::namespace::decode_namespace(r).unwrap(),
	};
	assert!(r.is_empty());
	path
}

/// Append `path` as moq-lite carries it (`ietf == false`), a string, or as a
/// moq-transport namespace tuple.
pub fn encode_path(path: &Path, ietf: bool, out: &mut Vec<u8>) {
	let w = &mut Encoder::new(out, bench_form(ietf));
	match ietf {
		false => path.encode(w, BENCH_LITE).unwrap(),
		true => ietf::namespace::encode_namespace(w, path).unwrap(),
	}
}

/// Append a SUBSCRIBE for `track` in `broadcast`, as moq-lite (`ietf == false`) or
/// moq-transport carries it.
pub fn encode_subscribe(broadcast: &Path, track: &str, ietf: bool, out: &mut Vec<u8>) {
	let w = &mut Encoder::new(out, bench_form(ietf));
	match ietf {
		false => lite::Subscribe {
			id: 0,
			broadcast: broadcast.clone(),
			epoch: None,
			track: track.into(),
			priority: 0,
			max_delay: std::time::Duration::ZERO,
			ranges: vec![lite::Range::OLDEST],
			order: track::Order::Descending,
		}
		.encode(w, BENCH_LITE)
		.unwrap(),
		true => ietf::Subscribe {
			request_id: ietf::RequestId(0),
			track_namespace: broadcast.clone(),
			track_name: track.into(),
			subscriber_priority: 128,
			group_order: ietf::GroupOrder::Descending,
			filter: ietf::Filter::NextObject,
			fill: None,
			properties_wanted: true,
			forward: true,
			rendezvous: false,
			range_filters: false,
			authorization_token: None,
			new_group: None,
			max_age: None,
			epoch: None,
		}
		.encode(w, BENCH_IETF)
		.unwrap(),
	}
}

/// Decode the SUBSCRIBE [`encode_subscribe`] wrote, as a lookup would: its broadcast,
/// and the length of its track name.
pub fn decode_subscribe(data: &[u8], ietf: bool) -> (Path, usize) {
	let r = &mut Decoder::new(data, bench_form(ietf));
	let decoded = match ietf {
		false => {
			let msg = lite::Subscribe::decode(r, BENCH_LITE).unwrap();
			(msg.broadcast, msg.track.len())
		}
		true => {
			let msg = ietf::Subscribe::decode(r, BENCH_IETF).unwrap();
			(msg.track_namespace, msg.track_name.len())
		}
	};
	assert!(r.is_empty());
	decoded
}

/// Exercise the [`Path`] invariants against arbitrary text.
///
/// The input is UTF-8, split at the first newline into a target path and a base. The
/// base doubles as a prefix and as a relative reference, so one input drives prefix
/// matching, joining, and the [`Path::relative`] / [`Path::resolve`] inverse pair.
pub fn path(data: &[u8]) -> bool {
	let Ok(text) = std::str::from_utf8(data) else {
		return false;
	};

	let (target, base) = text.split_once('\n').unwrap_or((text, ""));

	// A spelling is refused exactly when it is over the limit, has an empty segment, or has
	// a `\` escaping anything but `\` or `/`, so each path has one spelling.
	for s in [target, base] {
		let expected = if s.len() > Path::MAX_BYTES {
			Err(crate::path::Error::TooLong)
		} else if !s.is_empty() && s.split('/').any(str::is_empty) {
			Err(crate::path::Error::EmptySegment)
		} else if s.replace(r"\\", "").replace(r"\/", "").contains('\\') {
			Err(crate::path::Error::InvalidEscape)
		} else {
			Ok(())
		};
		assert_eq!(Path::new(s).map(|_| ()), expected, "{s:?} was judged wrongly");
	}
	let (Ok(target), Ok(base)) = (Path::new(target), Path::new(base)) else {
		return true;
	};

	// A path's own spelling parses back to it, or every path that made a round trip
	// through the wire would drift.
	assert_strict(&target);
	assert_eq!(target.parts().collect::<Vec<_>>().join("/"), target.as_str());

	// A prefix is a prefix whichever way you ask.
	assert_eq!(
		target.has_prefix(&base),
		target.strip_prefix(&base).is_some(),
		"has_prefix and strip_prefix disagree"
	);
	if let Some(rest) = target.strip_prefix(&base) {
		assert_eq!(base.join(&rest), Ok(target.clone()), "strip_prefix did not invert join");
	}

	// Joining descends, so the result is always under what it started from, and fails
	// only past the limit.
	match target.join(&base) {
		Ok(joined) => assert!(joined.has_prefix(&target), "join escaped its base"),
		Err(_) => assert!(
			target.len() + 1 + base.len() > Path::MAX_BYTES,
			"a join within the limit failed"
		),
	}

	// `relative` is documented as the inverse of `resolve`, and as never walking above
	// the root, so `try_resolve` has to accept whatever it produces.
	if let Some(rel) = target.relative(&base) {
		assert_eq!(
			base.resolve(&rel),
			Ok(target.clone()),
			"relative did not invert resolve"
		);
		assert_eq!(
			base.try_resolve(&rel),
			Some(target.clone()),
			"relative escaped the root"
		);
	}

	// Resolving arbitrary references must stay inside the clamped/unclamped contract:
	// `try_resolve` only refuses by walking above the root, so whenever it answers, it
	// answers the same as `resolve`.
	match Relative::new(base.as_str()) {
		Ok(rel) => {
			if let Some(resolved) = target.try_resolve(&rel) {
				assert_eq!(Ok(resolved), target.resolve(&rel), "try_resolve disagreed with resolve");
			}
		}
		// Only dropping a `.` segment can make a valid path an invalid reference, by stranding
		// the `\` of the escaped `/` before it.
		Err(err) => assert_eq!(err, crate::path::Error::InvalidEscape, "{base:?} is no reference"),
	}

	// The moq-transport namespace tuple is the same path back, so the mapping is symmetric.
	let mut tuple = Vec::new();
	let form = Form::LeadingOnes;
	if ietf::namespace::encode_namespace(&mut Encoder::new(&mut tuple, form), &target).is_ok() {
		let decoded = ietf::namespace::decode_namespace(&mut Decoder::new(&tuple, form))
			.expect("could not decode our own namespace");
		assert_eq!(decoded, target, "path did not survive the namespace tuple");
	}

	// The wire form is the same path back, whenever the path is expressible at all.
	let version = lite::Version::Lite05;
	if let Ok(encoded) = target.encode_bytes(version) {
		let (decoded, used) = Path::decode_slice(&encoded, version).expect("could not decode our own encoding");
		assert_eq!(
			used,
			encoded.len(),
			"our own encoding left {} bytes",
			encoded.len() - used
		);
		assert_eq!(decoded, target, "path did not survive a round trip");
	}

	true
}

/// A generated fuzzer input.
pub struct Seed {
	/// The target whose corpus it belongs in.
	pub target: &'static str,
	/// The dispatch arm it aims at. Only the tests use this, to check that no arm is
	/// left with nothing that reaches it.
	pub kind: u8,
	/// The bytes to feed the target.
	pub data: Vec<u8>,
}

/// A pattern's text, a newline, and a path: the pattern algebra's invariants.
///
/// Parsing is the fixed point Display prints; a pattern contains and overlaps itself;
/// rebasing at the path describes exactly what the pattern matches beneath it; and
/// rooting at a literal path inverts rebasing.
pub fn pattern(data: &[u8]) -> bool {
	let Ok(text) = std::str::from_utf8(data) else {
		return false;
	};
	let (pattern, path) = text.split_once('\n').unwrap_or((text, ""));
	let Ok(pattern) = pattern.parse::<Pattern>() else {
		return false;
	};

	assert_eq!(pattern.to_string(), pattern.as_str());
	assert_eq!(
		pattern.to_string().parse::<Pattern>().as_ref(),
		Ok(&pattern),
		"text is not canonical"
	);
	assert_eq!(
		Pattern::new(pattern.segments().to_vec()).as_ref(),
		Ok(&pattern),
		"segments do not rebuild"
	);
	assert!(pattern.contains(&pattern) && pattern.overlaps(&pattern));
	assert!(pattern.matches(pattern.head()) || !pattern.is_literal());

	let rebased = pattern.rebase(path);
	assert_eq!(
		rebased.matches(""),
		pattern.matches(path),
		"rebase disagrees with matches at the root"
	);
	for member in rebased.iter() {
		// Arbitrary roots may contain wildcard bytes or exceed the segment limit.
		if let Ok(rooted) = member.rooted(path) {
			assert!(
				pattern.contains(&rooted),
				"rebase produced a pattern outside the original"
			);
		}
	}
	if let Ok(rooted) = pattern.rooted(path) {
		assert!(rooted.rebase(path).contains(&pattern), "rooted did not invert rebase");
	}
	true
}

/// The inputs the fuzzer starts from: every (version, type) pair the dispatch knows,
/// bodied with byte patterns that decode as small valid fields.
///
/// Generated rather than committed so the corpus follows the dispatch instead of
/// rotting next to it. `just rs fuzz` writes these out before each run; the tests
/// below replay them.
pub fn seeds() -> Vec<Seed> {
	// Each is a small valid varint (and an empty or short string): 0x10 additionally
	// opens the IETF group header's flag range, and 0x02 its FETCH_TYPE range, neither
	// of which the others reach.
	const FILLS: &[u8] = &[0x00, 0x01, 0x02, 0x10];
	const LENGTHS: std::ops::RangeInclusive<u8> = 0..=6;

	let mut seeds = Vec::new();

	// Two body framings per type, because only the control messages carry a size
	// prefix: the stream headers, datagrams, and parameter maps are read raw.
	for version in 0..LITE_VERSIONS.len() as u8 {
		for kind in 0..LITE_KINDS {
			for fill in FILLS {
				for len in LENGTHS {
					let body = std::iter::repeat_n(*fill, len as usize);

					// A one-byte varint size prefix covers anything this small.
					let mut prefixed = vec![version, kind, len];
					prefixed.extend(body.clone());
					seeds.push(Seed {
						target: "lite",
						kind,
						data: prefixed,
					});

					let mut raw = vec![version, kind];
					raw.extend(body);
					seeds.push(Seed {
						target: "lite",
						kind,
						data: raw,
					});
				}
			}
		}
	}

	// A typed empty TRACKS_SYNCED and AUTH_OK reply need nonuniform envelope bytes.
	for (kind, body) in [(23, &[4, 0][..]), (27, &[0, 3, 0, 0, 0][..])] {
		let mut data = vec![(LITE_VERSIONS.len() - 1) as u8, kind];
		data.extend_from_slice(body);
		seeds.push(Seed {
			target: "lite",
			kind,
			data,
		});
	}

	for version in 0..IETF_VERSIONS.len() as u8 {
		for kind in 0..IETF_KINDS {
			for fill in FILLS {
				for len in LENGTHS {
					let body = std::iter::repeat_n(*fill, len as usize);

					// IETF control messages carry a two-byte size prefix, not a varint.
					let mut prefixed = vec![version, kind, 0x00, len];
					prefixed.extend(body.clone());
					seeds.push(Seed {
						target: "ietf",
						kind,
						data: prefixed,
					});

					let mut raw = vec![version, kind];
					raw.extend(body);
					seeds.push(Seed {
						target: "ietf",
						kind,
						data: raw,
					});
				}
			}
		}
	}

	// SETUP has its own type prefix, which uniform bodies cannot synthesize.
	for (index, version) in IETF_VERSIONS.iter().enumerate() {
		let outer = crate::Version::Ietf(*version);
		let parameters = ietf::Parameters::default().encode_bytes(*version).unwrap();
		let messages = vec![(26, crate::setup::Setup { parameters }.encode_bytes(outer).unwrap())];
		for (kind, message) in messages {
			let mut data = vec![index as u8, kind];
			data.extend(message);
			seeds.push(Seed {
				target: "ietf",
				kind,
				data,
			});
		}
	}

	// The `moql` SETUP pair, framed as lite-01/02, also has its own type prefix.
	for (index, version) in LITE_VERSIONS.iter().enumerate() {
		let outer = crate::Version::Lite(*version);
		let client = crate::setup::Client {
			versions: coding::Versions::from([outer.into()]),
			parameters: Default::default(),
		};
		let server = crate::setup::Server {
			version: outer.into(),
			parameters: Default::default(),
		};
		let (Ok(client), Ok(server)) = (client.encode_bytes(outer), server.encode_bytes(outer)) else {
			continue;
		};
		for (kind, message) in [(28, client), (29, server)] {
			let mut data = vec![index as u8, kind];
			data.extend(message);
			seeds.push(Seed {
				target: "lite",
				kind,
				data,
			});
		}
	}

	// FETCH is the one arm the sweep above cannot reach: its body ends in a parameter
	// count, and a uniform fill never lands a zero there. Build it from the encoder
	// instead, which also means a field change breaks the build rather than the seed.
	// Draft-22 dropped the joining forms, so each draft gets whichever layout it has.
	for (index, version) in IETF_VERSIONS.iter().enumerate() {
		let fetch_types = [
			ietf::FetchType::AbsoluteJoining {
				subscriber_request_id: ietf::RequestId(0),
				group_id: 0,
			},
			ietf::FetchType::Filtered {
				namespace: crate::Path::from_static("a"),
				track: "b".into(),
				filter: ietf::Filter::Relative(1),
			},
		];
		let Some(encoded) = fetch_types.into_iter().find_map(|fetch_type| {
			let fetch = ietf::Fetch {
				request_id: ietf::RequestId(0),
				subscriber_priority: 128,
				group_order: ietf::GroupOrder::Ascending,
				fetch_type,
				range_filters: false,
				fill_timeout: false,
				properties_wanted: true,
				epoch: None,
			};
			fetch.encode_bytes(*version).ok()
		}) else {
			continue;
		};

		let mut data = vec![index as u8, 1];
		data.extend_from_slice(&encoded);
		seeds.push(Seed {
			target: "ietf",
			kind: 1,
			data,
		});
	}

	// A SUBSCRIBE_OK carrying LARGEST_OBJECT, whose value is two bare varints. A uniform
	// fill never lands a valid parameter count followed by a Location, so build it from
	// the encoder.
	for (index, version) in IETF_VERSIONS.iter().enumerate() {
		let subscribe_ok = ietf::SubscribeOk {
			track_alias: 0,
			largest: Some(ietf::Location { group: 427, object: 0 }),
			properties: Default::default(),
		};

		let Ok(encoded) = subscribe_ok.encode_bytes(*version) else {
			continue;
		};

		let mut data = vec![index as u8, 12];
		data.extend_from_slice(&encoded);
		seeds.push(Seed {
			target: "ietf",
			kind: 12,
			data,
		});
	}

	// Announce streams from our own encoder: routes sharing path heads and relay tails,
	// with retractions and updates moving the bases around.
	let mut announced = Vec::new();
	let hop = |id: u64| crate::Hop::new(id).expect("a valid hop");
	for n in 0..12u64 {
		let suffix = Path::new(&format!("pid{}/private/channel_{}/health-{n}", n % 3, n % 2)).unwrap();
		let hops = Hops::try_from(vec![hop(100 + n % 3), hop(1 << 40), hop(1 << 41)]).expect("valid hops");
		announced.push(Announced::Start(suffix, hops));
		if n % 4 == 3 {
			let hops = Hops::try_from(vec![hop(200), hop(1 << 41)]).expect("valid hops");
			announced.push(Announced::Update(n, hops));
		}
		if n % 5 == 4 {
			announced.push(Announced::End(n));
		}
		seeds.push(Seed {
			target: "announce",
			kind: 0,
			data: encode_announces(&announced, true),
		});
	}

	// One of each length tag in both codecs, plus the all-ones forms that only the
	// leading-ones codec accepts.
	for selector in 0..12u8 {
		for fill in [0x00u8, 0x01, 0x40, 0x80, 0xC0, 0xFE, 0xFF] {
			for len in 1..=9u8 {
				let mut data = vec![selector];
				data.extend(std::iter::repeat_n(fill, len as usize));
				seeds.push(Seed {
					target: "varint",
					kind: 0,
					data,
				});
			}
		}
	}

	for text in [
		"",
		"/",
		"//",
		"a",
		"a/b",
		"a/b/c",
		"a//b",
		"/a/b/",
		".",
		"..",
		"a/.",
		"a/..",
		"./a",
		"../a",
		"a\nb",
		"a/b\na/b",
		"a/b/c\na/b",
		"a/c\na/b",
		"c\na/b",
		"a\na/b",
		"a/..\na/b",
		"a/b\n",
		"\na/b",
		"a/b\n..",
		"a/b\n../..",
		"a/b/c\n.",
		"a/b\na/b/c",
	] {
		seeds.push(Seed {
			target: "path",
			kind: 0,
			data: text.as_bytes().to_vec(),
		});
	}

	for text in [
		"**\na",
		"**/a\na",
		"a/*/**/b\na/x",
		"**/*.hang\npid/cam.hang",
		"foo.*.hang/**\nfoo.1.hang/x",
		"*\n",
		"\n",
		"a/b\na/b",
		"a*b*c\n",
	] {
		seeds.push(Seed {
			target: "pattern",
			kind: 0,
			data: text.as_bytes().to_vec(),
		});
	}

	seeds
}

type FrameReader = coding::Reader<crate::transport::poll::Buffered<Chunks>, lite::Version>;

/// Frames arriving on concurrent group streams, as the frame bench times them: one
/// frame per stream, read a chunk per stream per poll turn so the streams interleave
/// the way a session's do.
///
/// `budget` is what the session may allocate up front, so `Some(usize::MAX)` allocates
/// every declared size when the first byte lands and `Some(0)` grows every buffer with
/// the bytes received.
pub struct FrameRecv {
	streams: Vec<Option<(FrameReader, frame::Producer)>>,
	// Keeps the groups' track alive.
	_track: track::Producer,
	_broadcast: broadcast::Producer,
}

impl FrameRecv {
	/// `streams` frames of `size` bytes, delivered `chunk` bytes at a time.
	/// `None` gives the session's default budget.
	pub fn new(streams: usize, size: usize, chunk: usize, budget: Option<usize>) -> Self {
		// Unbounded, so eviction never aborts a group mid-frame.
		let mut info = broadcast::Info::new();
		info.pool = cache::Pool::unbounded();
		info.frames = budget.map(frame::Budget::new).unwrap_or_default();
		let broadcast = info.produce();
		let track = broadcast
			.create_track(
				"frames",
				crate::track::Info::default().with_timescale(crate::Timescale::MILLI),
			)
			.unwrap();
		let payload = bytes::Bytes::from(vec![0u8; size]);

		let streams = (0..streams)
			.map(|_| {
				let group = track.append_group().unwrap();
				let info = frame::Info {
					size: size as u64,
					timestamp: Some(crate::Timestamp::ZERO),
				};
				let frame = group.create_frame(info).unwrap();
				let stream = Chunks {
					payload: payload.clone(),
					chunk,
					ready: true,
				};
				Some((
					coding::Reader::new(crate::transport::poll::Buffered::new(stream), lite::Version::Lite05),
					frame,
				))
			})
			.collect();

		Self {
			streams,
			_track: track,
			_broadcast: broadcast,
		}
	}

	/// Receive every frame, round-robin a chunk at a time, then drop them.
	pub fn run(mut self) {
		let waiter = &kio::Waiter::noop();
		let mut open = self.streams.len();
		while open > 0 {
			for slot in &mut self.streams {
				let Some((reader, frame)) = slot else { continue };
				if let std::task::Poll::Ready(res) = reader.poll_read_frame(waiter, frame) {
					res.unwrap();
					let (_, frame) = slot.take().unwrap();
					let mut group = frame.finish().unwrap();
					group.finish().unwrap();
					open -= 1;
				}
			}
		}
	}
}

/// A stream that hands over one chunk of its payload, then blocks until polled again.
struct Chunks {
	payload: bytes::Bytes,
	chunk: usize,
	ready: bool,
}

impl crate::transport::poll::Chunks for Chunks {
	type Error = NoError;

	// Zero-copy, like the QUIC stacks, so only the frame buffer's own copy is timed.
	fn poll_read_chunk(
		&mut self,
		_waiter: &kio::Waiter,
		max: usize,
	) -> std::task::Poll<Result<Option<bytes::Bytes>, NoError>> {
		self.ready = !self.ready;
		if self.ready {
			return std::task::Poll::Pending;
		}
		if self.payload.is_empty() {
			return std::task::Poll::Ready(Ok(None));
		}
		let n = self.chunk.min(max).min(self.payload.len());
		std::task::Poll::Ready(Ok(Some(self.payload.split_to(n))))
	}

	fn stop(&mut self, _code: u32) {}
}

/// [`Chunks`] never fails.
#[derive(Debug)]
struct NoError;

impl std::fmt::Display for NoError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.write_str("no error")
	}
}

impl std::error::Error for NoError {}

impl crate::transport::Error for NoError {
	fn session_error(&self) -> Option<(u32, String)> {
		None
	}
}

/// A source-selection watch for the private request-lifetime benchmark.
pub struct RequestValidity(crate::model::broadcast::RequestValidity);

impl RequestValidity {
	/// Watch which instance this route's peer selects for a path.
	pub fn new(route: &crate::origin::Requests, path: &crate::Path) -> Self {
		Self(crate::model::broadcast::RequestValidity::for_source(route, path))
	}

	/// Capture the lifetime of one pending request.
	pub fn start(&self) -> kio::Consumer<()> {
		match self.0.start() {
			crate::model::broadcast::RequestValidity::Period(route, ..) => route,
			_ => unreachable!("a source watch always captures a route lease"),
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	use std::collections::BTreeSet;

	/// The bump lands on the publisher side and survives a second drain while
	/// an announce guard still holds the entry.
	#[test]
	fn bump_publisher_bytes_stays_while_announced() {
		use futures::FutureExt;

		let registry = crate::stats::Registry::new(crate::stats::Config::new());
		let (origin, driver) = crate::origin::Producer::new(crate::origin::Config::default());
		let _driver = driver;
		let tier = crate::stats::Tier::new(crate::Path::from_static("t"));
		let session = registry.tier(tier.clone()).session(crate::Path::from_static("root"));
		let _broadcast = origin.publish("room/1", crate::origin::Route::default()).unwrap();
		let mut cursor = origin.consume().with_stats(session).announced();
		while cursor.next().now_or_never().flatten().is_some() {}

		bump_publisher_bytes(&registry, crate::Path::from_static("room/1"), &tier, 5);
		let mut report = crate::stats::Report::default();
		registry.report(&mut report);
		registry.report(&mut report);
		let entry = report
			.traffic
			.iter()
			.find(|entry| entry.path.as_str() == "room/1")
			.expect("announced path");
		assert_eq!(entry.publisher.bytes, 5);
		assert_eq!(entry.tier, tier);
	}

	fn target(name: &str) -> Target {
		TARGETS
			.iter()
			.find(|(target, _)| *target == name)
			.unwrap_or_else(|| panic!("no such target: {name}"))
			.1
	}

	/// Every seed runs, and every dispatch arm decodes at least one of them.
	///
	/// The second half is the point: a corpus that decodes nothing leaves the fuzzer
	/// mutating noise against an early `return`, and an arm nothing reaches is a
	/// dispatch that silently covers less than it lists.
	#[test]
	fn seeds_reach_every_arm() {
		let mut expected = BTreeSet::new();
		let mut decoded = BTreeSet::new();

		for seed in seeds() {
			expected.insert((seed.target, seed.kind));
			if target(seed.target)(&seed.data) {
				decoded.insert((seed.target, seed.kind));
			}
		}

		let missing: Vec<_> = expected.difference(&decoded).collect();
		assert!(missing.is_empty(), "no seed decodes for {missing:?}");
	}

	/// Replay the crash inputs committed under `fuzz/regressions/`, so a bug the fuzzer
	/// found stays fixed on the stable toolchain, in the suite CI already runs.
	#[test]
	fn regressions() {
		let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fuzz/regressions");

		for (name, target) in TARGETS {
			let dir = root.join(name);
			// Nothing has crashed yet for this target; `fuzz/README.md` says what to
			// drop in here when something does.
			let Ok(entries) = std::fs::read_dir(&dir) else {
				continue;
			};

			for entry in entries {
				let path = entry.expect("could not read the regression directory").path();
				if path.extension().is_some_and(|ext| ext == "md") {
					continue;
				}
				let data = std::fs::read(&path).expect("could not read a regression input");
				target(&data);
			}
		}
	}
}
