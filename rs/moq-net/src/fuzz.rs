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
	Hops, Path, PathOwned, Pattern,
	coding::{Decode, Decoder, Encode, Encoder, Form, varint},
	ietf, lite,
	path::Relative,
};

/// One fuzz target body: it returns whether the input decoded, which is what
/// [`seeds`] is checked against.
pub type Target = fn(&[u8]) -> bool;

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
const IETF_VERSIONS: &[ietf::Version] = &[
	ietf::Version::Draft14,
	ietf::Version::Draft15,
	ietf::Version::Draft16,
	ietf::Version::Draft17,
	ietf::Version::Draft18,
	ietf::Version::Draft19,
	ietf::Version::Draft20,
	ietf::Version::Draft21,
	ietf::Version::Draft22,
];

/// How many types [`lite_wire`] dispatches over.
const LITE_KINDS: u8 = 21;

/// How many types [`ietf_wire`] dispatches over.
const IETF_KINDS: u8 = 39;

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
fn roundtrip<T, V>(data: &[u8], version: V) -> bool
where
	T: Decode<V> + Encode<V>,
	V: Copy + Into<Form>,
{
	let Ok((decoded, _)) = T::decode_slice(data, version) else {
		return false;
	};

	let Ok(first) = decoded.encode_bytes(version) else {
		return true;
	};

	let (decoded, used) = T::decode_slice(&first, version).expect("could not decode our own encoding");
	assert_eq!(used, first.len(), "our own encoding left {} bytes", first.len() - used);

	let Ok(second) = decoded.encode_bytes(version) else {
		panic!("could not re-encode what we just encoded");
	};
	assert_eq!(first, second, "encoding is not stable");

	true
}

/// [`roundtrip`] for a datagram body, which runs to the end of its buffer.
fn datagram(data: &[u8], version: lite::Version) -> bool {
	let Ok(decoded) = lite::Datagram::decode(Bytes::copy_from_slice(data), version) else {
		return false;
	};

	let Ok(first) = decoded.encode_bytes(version) else {
		return true;
	};

	let echo = lite::Datagram::decode(first, version).expect("could not decode our own encoding");
	assert_eq!(echo, decoded, "datagram did not survive a round trip");

	true
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

	match kind {
		0 => roundtrip::<lite::Setup, _>(rest, version),
		1 => roundtrip::<lite::SessionInfo, _>(rest, version),
		2 => roundtrip::<lite::AnnounceInit<'static>, _>(rest, version),
		3 => roundtrip::<lite::AnnounceRequest<'static>, _>(rest, version),
		4 => roundtrip::<lite::AnnounceOk, _>(rest, version),
		5 => roundtrip::<lite::AnnounceBroadcast<'static>, _>(rest, version),
		6 => roundtrip::<lite::Subscribe<'static>, _>(rest, version),
		7 => roundtrip::<lite::SubscribeOk, _>(rest, version),
		8 => roundtrip::<lite::SubscribeStart, _>(rest, version),
		9 => roundtrip::<lite::SubscribeEnd, _>(rest, version),
		10 => roundtrip::<lite::SubscribeUpdate, _>(rest, version),
		11 => roundtrip::<lite::SubscribeDrop, _>(rest, version),
		12 => roundtrip::<lite::SubscribeResponse, _>(rest, version),
		13 => roundtrip::<lite::Fetch<'static>, _>(rest, version),
		14 => roundtrip::<lite::Group, _>(rest, version),
		15 => roundtrip::<lite::Goaway<'static>, _>(rest, version),
		16 => roundtrip::<lite::Track<'static>, _>(rest, version),
		17 => roundtrip::<lite::TrackInfo, _>(rest, version),
		18 => roundtrip::<lite::Probe, _>(rest, version),
		19 => datagram(rest, version),
		20 => roundtrip::<lite::Parameters, _>(rest, version),
		_ => unreachable!("kind is taken modulo LITE_KINDS"),
	}
}

/// One announcement on an announce stream, resolved: what the application sees.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Announced {
	/// ANNOUNCE_START: a route's suffix and wire hop chain, taking the next id.
	Start(PathOwned, Hops),
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
					suffix,
					hops,
					cost: Default::default(),
				}
			}
			Announced::Update(id, hops) => lite::AnnounceBroadcast::Restart {
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
			lite::AnnounceBroadcast::Restart { id, hops, .. } => {
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

/// Feed a lite-07 announce stream through the stateful decoder, then check that our
/// encoder's compression of what it resolved reads back as the same announcements.
///
/// Returns whether anything resolved.
pub fn announce_stream(data: &[u8]) -> bool {
	let resolved = decode_announces(data, true);
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

	match kind {
		0 => roundtrip::<ietf::GoAway<'static>, _>(rest, version),
		1 => roundtrip::<ietf::Fetch<'static>, _>(rest, version),
		2 => roundtrip::<ietf::FetchOk, _>(rest, version),
		3 => roundtrip::<ietf::FetchError<'static>, _>(rest, version),
		4 => roundtrip::<ietf::FetchCancel, _>(rest, version),
		5 => roundtrip::<ietf::FetchHeader, _>(rest, version),
		6 => roundtrip::<ietf::FetchType<'static>, _>(rest, version),
		7 => roundtrip::<ietf::PublishNamespace<'static>, _>(rest, version),
		8 => roundtrip::<ietf::PublishNamespaceOk, _>(rest, version),
		9 => roundtrip::<ietf::PublishNamespaceError<'static>, _>(rest, version),
		10 => roundtrip::<ietf::PublishNamespaceDone<'static>, _>(rest, version),
		11 => roundtrip::<ietf::PublishNamespaceCancel<'static>, _>(rest, version),
		12 => roundtrip::<ietf::TrackStatus<'static>, _>(rest, version),
		13 => roundtrip::<ietf::Publish<'static>, _>(rest, version),
		14 => roundtrip::<ietf::PublishOk, _>(rest, version),
		15 => roundtrip::<ietf::PublishError<'static>, _>(rest, version),
		16 => roundtrip::<ietf::PublishDone<'static>, _>(rest, version),
		17 => roundtrip::<ietf::PublishBlocked<'static>, _>(rest, version),
		18 => roundtrip::<ietf::Subscribe<'static>, _>(rest, version),
		19 => roundtrip::<ietf::SubscribeOk, _>(rest, version),
		20 => roundtrip::<ietf::SubscribeError<'static>, _>(rest, version),
		21 => roundtrip::<ietf::SubscribeUpdate, _>(rest, version),
		22 => roundtrip::<ietf::Unsubscribe, _>(rest, version),
		23 => roundtrip::<ietf::SubscribeNamespace<'static>, _>(rest, version),
		24 => roundtrip::<ietf::SubscribeNamespaceLegacy<'static>, _>(rest, version),
		25 => roundtrip::<ietf::SubscribeNamespaceOk, _>(rest, version),
		26 => roundtrip::<ietf::SubscribeNamespaceError<'static>, _>(rest, version),
		27 => roundtrip::<ietf::UnsubscribeNamespace, _>(rest, version),
		28 => roundtrip::<ietf::Namespace<'static>, _>(rest, version),
		29 => roundtrip::<ietf::NamespaceDone<'static>, _>(rest, version),
		30 => roundtrip::<ietf::MaxRequestId, _>(rest, version),
		31 => roundtrip::<ietf::RequestsBlocked, _>(rest, version),
		32 => roundtrip::<ietf::RequestOk, _>(rest, version),
		33 => roundtrip::<ietf::RequestError<'static>, _>(rest, version),
		34 => roundtrip::<ietf::GroupHeader, _>(rest, version),
		35 => roundtrip::<ietf::Parameters, _>(rest, version),
		36 => roundtrip::<ietf::Location, _>(rest, version),
		37 => roundtrip::<ietf::FetchObject, _>(rest, version),
		38 => roundtrip::<ietf::PublishNamespaceUpdate, _>(rest, version),
		_ => unreachable!("kind is taken modulo IETF_KINDS"),
	}
}

/// Decode a varint with whichever codec the selected version uses.
///
/// Byte 0 picks the version, which is the whole point: moq-lite and drafts 14-16 use
/// the QUIC two-bit length tag, while draft-17+ counts leading ones, and the two
/// disagree about which byte sequences are even legal.
///
/// The leading-ones form spans the full `u64`, while the QUIC form stops at
/// [`crate::coding::varint::MAX_QUIC`]; a value always re-encodes in the form it was read in.
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
				id: 7,
				broadcast: Path::new("room/alice"),
				track: "video".into(),
				priority: 3,
				max_age: Duration::from_millis(500),
				start_group: Some(1_000),
				end_group: None,
				start_frame: 0,
				end_frame: None,
			},
			lite_update: lite::SubscribeUpdate {
				priority: 4,
				max_age: Duration::from_millis(500),
				start_group: Some(1_000),
				end_group: Some(2_000),
				start_frame: 0,
				end_frame: None,
			},
			lite_start: lite::SubscribeResponse::Start(lite::SubscribeStart { group: 1_234 }),
			lite_info: lite::TrackInfo {
				priority: 1,
				max_age: Some(Duration::from_secs(10)),
				timescale: crate::Timescale::MICRO,
			},
			lite_group: lite::Group {
				subscribe: 7,
				sequence: 123_456,
				frame_start: 0,
			},
			ietf_subscribe: ietf::Subscribe {
				request_id: ietf::RequestId(2),
				track_namespace: Path::new("room/alice"),
				track_name: "video".into(),
				subscriber_priority: 128,
				group_order: ietf::GroupOrder::Descending,
				filter: ietf::Filter::NextObject,
				fill: None,
				properties_wanted: false,
			},
			// Draft-17+ carries the request id in the control message framing instead.
			ietf_ok: ietf::SubscribeOk {
				request_id: None,
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
const BENCH_IETF: ietf::Version = ietf::Version::Draft20;

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
	let target = Path::new(target);
	let base = Path::new(base);

	// Normalization is a fixed point: re-parsing what `as_str` printed changes nothing,
	// or every path that made a round trip through the wire would drift.
	assert_eq!(Path::new(target.as_str()), target, "normalization is not idempotent");
	assert_eq!(target.to_owned(), target, "owning a path changed it");
	assert_eq!(target.borrow(), target, "borrowing a path changed it");
	assert_eq!(target.parts().collect::<Vec<_>>().join("/"), target.as_str());

	// A prefix is a prefix whichever way you ask.
	assert_eq!(
		target.has_prefix(&base),
		target.strip_prefix(&base).is_some(),
		"has_prefix and strip_prefix disagree"
	);
	if let Some(rest) = target.strip_prefix(&base) {
		assert_eq!(base.join(&rest), target, "strip_prefix did not invert join");
	}

	// Joining descends, so the result is always under what it started from.
	let joined = target.join(&base);
	assert!(joined.has_prefix(&target), "join escaped its base");

	// `relative` is documented as the inverse of `resolve`, and as never walking above
	// the root, so `try_resolve` has to accept whatever it produces.
	if let Some(rel) = target.relative(&base) {
		assert_eq!(base.resolve(&rel), target, "relative did not invert resolve");
		assert_eq!(
			base.try_resolve(&rel),
			Some(target.to_owned()),
			"relative escaped the root"
		);
	}

	// Resolving arbitrary references must stay inside the clamped/unclamped contract:
	// `try_resolve` only refuses by walking above the root, so whenever it answers, it
	// answers the same as `resolve`.
	let rel = Relative::new(base.as_str());
	if let Some(resolved) = target.try_resolve(&rel) {
		assert_eq!(resolved, target.resolve(&rel), "try_resolve disagreed with resolve");
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

	// FETCH is the one arm the sweep above cannot reach: its body ends in a parameter
	// count, and a uniform fill never lands a zero there. Build it from the encoder
	// instead, which also means a field change breaks the build rather than the seed.
	for (index, version) in IETF_VERSIONS.iter().enumerate() {
		let fetch = ietf::Fetch {
			request_id: ietf::RequestId(0),
			subscriber_priority: 128,
			group_order: ietf::GroupOrder::Ascending,
			fetch_type: ietf::FetchType::AbsoluteJoining {
				subscriber_request_id: ietf::RequestId(0),
				group_id: 0,
			},
		};

		let Ok(encoded) = fetch.encode_bytes(*version) else {
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

	// A SUBSCRIBE_OK carrying LARGEST_OBJECT, whose value encoding changed at draft-17
	// from a length-prefixed Location to two bare varints. A uniform fill never lands a
	// valid parameter count followed by a Location, so build it from the encoder.
	for (index, version) in IETF_VERSIONS.iter().enumerate() {
		let subscribe_ok = ietf::SubscribeOk {
			request_id: matches!(
				version,
				ietf::Version::Draft14 | ietf::Version::Draft15 | ietf::Version::Draft16
			)
			.then_some(ietf::RequestId(0)),
			track_alias: 0,
			largest: Some(ietf::Location { group: 427, object: 0 }),
			properties: Default::default(),
		};

		let Ok(encoded) = subscribe_ok.encode_bytes(*version) else {
			continue;
		};

		let mut data = vec![index as u8, 19];
		data.extend_from_slice(&encoded);
		seeds.push(Seed {
			target: "ietf",
			kind: 19,
			data,
		});
	}

	// Announce streams from our own encoder: routes sharing path heads and relay tails,
	// with retractions and updates moving the bases around.
	let mut announced = Vec::new();
	let hop = |id: u64| crate::Hop::new(id).expect("a valid hop");
	for n in 0..12u64 {
		let suffix = PathOwned::from(format!("pid{}/private/channel_{}/health-{n}", n % 3, n % 2));
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

#[cfg(test)]
mod tests {
	use super::*;

	use std::collections::BTreeSet;

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
