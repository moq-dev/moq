//! Lite-07 announce compression: an ANNOUNCE_START may copy the head of a live
//! announcement's path, and an ANNOUNCE_START or ANNOUNCE_UPDATE the tail of a live
//! announcement's hop chain, naming that base by its distance back from the stream's
//! next unassigned Announce ID.
//!
//! The codec in `announce.rs` stays stateless and carries bases as they travel. The
//! decoder here resolves them against the stream's live announcements, which the
//! subscriber has to track anyway, and the encoder picks them.

use std::{cmp::Ordering, collections::BTreeSet, collections::HashMap};

use crate::{Error, Hops, Path, PathOwned, coding::Encode, coding::Sizer};

use super::{HopsRef, PathRef, Version};

/// A live announcement, as the peer holds it: its wire suffix and wire hop chain
/// (the one excluding ANNOUNCE_OK's Hop ID).
#[derive(Clone, Debug)]
struct Entry {
	suffix: PathOwned,
	hops: Hops,
}

/// The live announcements on one stream by Announce ID, which is all a base can name.
#[derive(Debug, Default)]
struct Live {
	next: u64,
	entries: HashMap<u64, Entry>,
}

impl Live {
	/// The live announcement `distance` back from the next id, or `None` for 0.
	fn base(&self, distance: u64) -> Result<Option<&Entry>, Error> {
		if distance == 0 {
			return Ok(None);
		}
		let id = self.next.checked_sub(distance).ok_or(Error::ProtocolViolation)?;
		self.entries.get(&id).map(Some).ok_or(Error::ProtocolViolation)
	}

	fn resolve_path(&self, path: PathRef<'_>) -> Result<PathOwned, Error> {
		let Some(base) = self.base(path.base)? else {
			return Ok(path.rest.into_owned());
		};
		let keep = usize::try_from(path.keep).map_err(|_| Error::ProtocolViolation)?;
		let head = head(&base.suffix, keep).ok_or(Error::ProtocolViolation)?;
		let suffix = head.join(&path.rest);
		if suffix.parts().count() > Path::MAX_PARTS {
			return Err(Error::ProtocolViolation);
		}
		Ok(suffix)
	}

	fn resolve_hops(&self, hops: HopsRef) -> Result<Hops, Error> {
		let Some(base) = self.base(hops.base)? else {
			return Ok(hops.literal);
		};
		let keep = usize::try_from(hops.keep).map_err(|_| Error::ProtocolViolation)?;
		let tail = base.hops.as_slice();
		let start = tail.len().checked_sub(keep).ok_or(Error::ProtocolViolation)?;
		let mut out = hops.literal;
		for hop in &tail[start..] {
			out.push(*hop).map_err(|_| Error::ProtocolViolation)?;
		}
		Ok(out)
	}
}

/// The first `keep` segments of `path`, or `None` if it has fewer.
fn head<'a>(path: &'a Path<'_>, keep: usize) -> Option<Path<'a>> {
	if keep == 0 {
		return Some(Path::empty());
	}
	let s = path.as_str();
	let mut ends = s
		.match_indices('/')
		.map(|(i, _)| i)
		.chain((!s.is_empty()).then_some(s.len()));
	ends.nth(keep - 1).map(|end| Path::new(&s[..end]))
}

/// Resolves the bases on a received announce stream. Only used on versions with
/// announce ids; before lite-06 nothing is ever referenced, so nothing is recorded.
#[derive(Debug, Default)]
pub(crate) struct AnnounceDecoder {
	live: Live,
}

impl AnnounceDecoder {
	/// An ANNOUNCE_START: resolve it and assign it the next id.
	pub fn start(&mut self, suffix: PathRef<'_>, hops: HopsRef) -> Result<(PathOwned, Hops), Error> {
		let suffix = self.live.resolve_path(suffix)?;
		let hops = self.live.resolve_hops(hops)?;
		let id = self.live.next;
		self.live.next += 1;
		self.live.entries.insert(
			id,
			Entry {
				suffix: suffix.clone(),
				hops: hops.clone(),
			},
		);
		Ok((suffix, hops))
	}

	/// An ANNOUNCE_UPDATE: resolve its chain before replacing the old one, which it
	/// may itself be based on.
	pub fn update(&mut self, id: u64, hops: HopsRef) -> Result<(PathOwned, Hops), Error> {
		let hops = self.live.resolve_hops(hops)?;
		let entry = self.live.entries.get_mut(&id).ok_or(Error::ProtocolViolation)?;
		entry.hops = hops.clone();
		Ok((entry.suffix.clone(), hops))
	}

	/// An ANNOUNCE_END: retire the id, returning its suffix.
	pub fn end(&mut self, id: u64) -> Result<PathOwned, Error> {
		let entry = self.live.entries.remove(&id).ok_or(Error::ProtocolViolation)?;
		Ok(entry.suffix)
	}
}

/// Orders paths segment by segment, so the path sharing the most leading segments
/// with a query is always one of its two neighbours. Byte order would not do:
/// `a-b` sorts between `a` and `a/x`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Segments(PathOwned);

impl Ord for Segments {
	fn cmp(&self, other: &Self) -> Ordering {
		// A normalized path has no empty segments, so segment order is byte order with
		// the separator ranked below every other byte, which avoids splitting.
		let (a, b) = (self.0.as_str().as_bytes(), other.0.as_str().as_bytes());
		// Paths under one prefix share long heads, so skip them in chunks.
		let skip = (a.chunks(16).zip(b.chunks(16)).take_while(|(x, y)| x == y).count() * 16).min(a.len().min(b.len()));
		let common = skip + a[skip..].iter().zip(&b[skip..]).take_while(|(x, y)| x == y).count();
		let rank = |byte: u8| if byte == b'/' { 0 } else { u16::from(byte) + 1 };
		match (a.get(common), b.get(common)) {
			(Some(&x), Some(&y)) => rank(x).cmp(&rank(y)),
			_ => a.len().cmp(&b.len()),
		}
	}
}

impl PartialOrd for Segments {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

/// Orders hop chains from the last hop back, so the chain sharing the longest tail
/// with a query is always one of its two neighbours.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Tail(Hops);

impl Ord for Tail {
	fn cmp(&self, other: &Self) -> Ordering {
		self.0
			.iter()
			.rev()
			.map(|hop| hop.id())
			.cmp(other.0.iter().rev().map(|hop| hop.id()))
	}
}

impl PartialOrd for Tail {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

/// Picks the bases for an outgoing announce stream, and assigns Announce IDs on
/// versions that have them. Only lite-07 keeps the live set, since nothing earlier
/// can name a base.
///
/// Two ordered indexes over the live announcements, one by path segments and one by
/// reversed hop chain, make the longest shared head or tail a neighbour lookup. A base
/// is used only when it encodes smaller than the literal.
#[derive(Debug)]
pub(crate) struct AnnounceEncoder {
	version: Version,
	live: Live,
	paths: BTreeSet<(Segments, u64)>,
	tails: BTreeSet<(Tail, u64)>,
}

impl AnnounceEncoder {
	pub fn new(version: Version) -> Self {
		Self {
			version,
			live: Live::default(),
			paths: BTreeSet::new(),
			tails: BTreeSet::new(),
		}
	}

	/// An ANNOUNCE_START: its id (on versions that assign one) and its wire form.
	pub fn start(&mut self, suffix: PathOwned, hops: Hops) -> (Option<u64>, PathRef<'static>, HopsRef) {
		if !self.version.has_announce_id() {
			return (None, PathRef::literal(suffix), HopsRef::literal(hops));
		}

		let id = self.live.next;
		self.live.next += 1;

		if !self.version.has_announce_compression() {
			return (Some(id), PathRef::literal(suffix), HopsRef::literal(hops));
		}

		// Distances count back from the id this START takes.
		let path = self.path_ref(&suffix, id);
		let chain = self.hops_ref(&hops, id);
		self.insert(id, suffix, hops);
		(Some(id), path, chain)
	}

	/// An ANNOUNCE_UPDATE for a live `id`: its wire hop chain.
	pub fn update(&mut self, id: u64, hops: Hops) -> HopsRef {
		if !self.version.has_announce_compression() {
			return HopsRef::literal(hops);
		}
		// The decoder resolves against the chain this replaces, so it is a valid base.
		let chain = self.hops_ref(&hops, self.live.next);
		let entry = self.live.entries.get_mut(&id).expect("update for a live id");
		let old = std::mem::replace(&mut entry.hops, hops.clone());
		self.tails.remove(&(Tail(old), id));
		self.tails.insert((Tail(hops), id));
		chain
	}

	/// An ANNOUNCE_END for a live `id`.
	pub fn end(&mut self, id: u64) {
		if !self.version.has_announce_compression() {
			return;
		}
		let entry = self.live.entries.remove(&id).expect("end for a live id");
		self.paths.remove(&(Segments(entry.suffix), id));
		self.tails.remove(&(Tail(entry.hops), id));
	}

	fn insert(&mut self, id: u64, suffix: PathOwned, hops: Hops) {
		self.paths.insert((Segments(suffix.clone()), id));
		self.tails.insert((Tail(hops.clone()), id));
		self.live.entries.insert(id, Entry { suffix, hops });
	}

	fn size<T: Encode<Version>>(&self, value: &T) -> usize {
		let mut sizer = Sizer::default();
		value
			.encode(&mut sizer, self.version)
			.expect("sizing an encodable value");
		sizer.size
	}

	fn path_ref(&self, suffix: &PathOwned, next: u64) -> PathRef<'static> {
		let literal = PathRef::literal(suffix.clone());
		let query = (Segments(suffix.clone()), u64::MAX);
		let before = self.paths.range(..&query).next_back();
		let after = self.paths.range(&query..).next();

		let mut best = literal;
		let mut best_size = self.size(&best);
		for (Segments(path), id) in before.into_iter().chain(after) {
			let keep = path.parts().zip(suffix.parts()).take_while(|(a, b)| a == b).count();
			if keep == 0 {
				continue;
			}
			let rest = suffix.parts().skip(keep).collect::<Vec<_>>().join("/");
			let candidate = PathRef {
				base: next - id,
				keep: keep as u64,
				rest: Path::from(rest),
			};
			let size = self.size(&candidate);
			if size < best_size {
				best = candidate;
				best_size = size;
			}
		}
		best
	}

	fn hops_ref(&self, hops: &Hops, next: u64) -> HopsRef {
		let literal = HopsRef::literal(hops.clone());
		let query = (Tail(hops.clone()), u64::MAX);
		let before = self.tails.range(..&query).next_back();
		let after = self.tails.range(&query..).next();

		let mut best = literal;
		let mut best_size = self.size(&best);
		for (Tail(chain), id) in before.into_iter().chain(after) {
			let keep = chain
				.iter()
				.rev()
				.zip(hops.iter().rev())
				.take_while(|(a, b)| a == b)
				.count();
			if keep == 0 {
				continue;
			}
			let literal = Hops::try_from(hops.as_slice()[..hops.len() - keep].to_vec())
				.expect("a prefix of a valid chain is valid");
			let candidate = HopsRef {
				base: next - id,
				literal,
				keep: keep as u64,
			};
			let size = self.size(&candidate);
			if size < best_size {
				best = candidate;
				best_size = size;
			}
		}
		best
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::Hop;

	const VERSION: Version = Version::Lite07;

	fn hops(ids: &[u64]) -> Hops {
		Hops::try_from(ids.iter().map(|id| Hop::from_wire(*id).unwrap()).collect::<Vec<_>>()).unwrap()
	}

	fn path(base: u64, keep: u64, rest: &str) -> PathRef<'static> {
		PathRef {
			base,
			keep,
			rest: Path::new(rest).to_owned(),
		}
	}

	fn chain(base: u64, literal: &[u64], keep: u64) -> HopsRef {
		HopsRef {
			base,
			literal: hops(literal),
			keep,
		}
	}

	fn literal(suffix: &str, ids: &[u64]) -> (PathRef<'static>, HopsRef) {
		(path(0, 0, suffix), chain(0, ids, 0))
	}

	#[test]
	fn resolves_a_path_head_and_hop_tail() {
		let mut decoder = AnnounceDecoder::default();
		let (p, h) = literal("pid/private/channel_1/health-1", &[1, 2, 3]);
		decoder.start(p, h).unwrap();

		// Three leading segments of the latest START, and its last two hops.
		let (suffix, chain) = decoder.start(path(1, 3, "health-2"), chain(1, &[7], 2)).unwrap();
		assert_eq!(suffix.as_str(), "pid/private/channel_1/health-2");
		assert_eq!(chain, hops(&[7, 2, 3]));
	}

	#[test]
	fn keeps_the_whole_path_and_chain() {
		let mut decoder = AnnounceDecoder::default();
		let (p, h) = literal("a/b", &[1, 2]);
		decoder.start(p, h).unwrap();
		// A full keep with an empty rest names a route above the base's: not a
		// duplicate, since the rest extends it.
		let (suffix, chain) = decoder.start(path(1, 2, "c"), chain(1, &[], 2)).unwrap();
		assert_eq!(suffix.as_str(), "a/b/c");
		assert_eq!(chain, hops(&[1, 2]));
		// An update copying its own full chain.
		let (suffix, chain) = decoder.update(0, chain_ref(2, 2)).unwrap();
		assert_eq!(suffix.as_str(), "a/b");
		assert_eq!(chain, hops(&[1, 2]));
	}

	fn chain_ref(base: u64, keep: u64) -> HopsRef {
		chain(base, &[], keep)
	}

	#[test]
	fn a_new_stream_has_no_base() {
		let mut decoder = AnnounceDecoder::default();
		assert!(matches!(
			decoder.start(path(1, 0, "a"), chain(0, &[], 0)),
			Err(Error::ProtocolViolation)
		));
		let mut decoder = AnnounceDecoder::default();
		assert!(matches!(
			decoder.start(path(0, 0, "a"), chain(1, &[], 0)),
			Err(Error::ProtocolViolation)
		));
	}

	#[test]
	fn rejects_a_retired_or_unassigned_base() {
		let mut decoder = AnnounceDecoder::default();
		let (p, h) = literal("a", &[1]);
		decoder.start(p, h).unwrap();
		let (p, h) = literal("b", &[2]);
		decoder.start(p, h).unwrap();
		decoder.end(0).unwrap();

		// Id 0 is two back, and retired.
		assert!(matches!(
			decoder.start(path(2, 1, "x"), chain(0, &[], 0)),
			Err(Error::ProtocolViolation)
		));
		// Three back was never assigned.
		assert!(matches!(
			decoder.start(path(3, 0, "x"), chain(0, &[], 0)),
			Err(Error::ProtocolViolation)
		));
		// Id 1 is still live, so reusing it works after the END.
		let (suffix, _) = decoder.start(path(1, 1, "x"), chain(1, &[], 1)).unwrap();
		assert_eq!(suffix.as_str(), "b/x");
	}

	#[test]
	fn rejects_a_keep_longer_than_the_base() {
		let mut decoder = AnnounceDecoder::default();
		let (p, h) = literal("a/b", &[1]);
		decoder.start(p, h).unwrap();
		assert!(matches!(
			decoder.start(path(1, 3, "x"), chain(0, &[], 0)),
			Err(Error::ProtocolViolation)
		));
		assert!(matches!(
			decoder.update(0, chain_ref(1, 2)),
			Err(Error::ProtocolViolation)
		));
	}

	#[test]
	fn rejects_a_resolved_chain_breaking_the_hop_rules() {
		let mut decoder = AnnounceDecoder::default();
		let (p, h) = literal("a", &[1, 2]);
		decoder.start(p, h).unwrap();
		// A literal hop the kept tail repeats.
		assert!(matches!(
			decoder.start(path(0, 0, "b"), chain(1, &[2], 1)),
			Err(Error::ProtocolViolation)
		));

		// Anonymous hops may repeat across the two halves.
		let (p, h) = literal("c", &[0]);
		decoder.start(p, h).unwrap();
		let (_, resolved) = decoder.start(path(0, 0, "d"), chain(1, &[0], 1)).unwrap();
		assert_eq!(resolved, hops(&[0, 0]));

		// And the resolved chain is still capped.
		// MAX_HOPS entries.
		let full: Vec<u64> = (1..=32).collect();
		let (p, h) = literal("e", &full);
		decoder.start(p, h).unwrap();
		assert!(matches!(
			decoder.start(path(0, 0, "f"), chain(1, &[100], 32)),
			Err(Error::ProtocolViolation)
		));
	}

	#[test]
	fn rejects_a_resolved_path_too_deep() {
		let mut decoder = AnnounceDecoder::default();
		let deep = vec!["x"; Path::MAX_PARTS].join("/");
		let (p, h) = literal(&deep, &[]);
		decoder.start(p, h).unwrap();
		assert!(matches!(
			decoder.start(path(1, Path::MAX_PARTS as u64, "y"), chain(0, &[], 0)),
			Err(Error::ProtocolViolation)
		));
	}

	/// Encode a message with `encoder`, decode it with `decoder`, and check that the
	/// resolved values match the input.
	fn start(encoder: &mut AnnounceEncoder, decoder: &mut AnnounceDecoder, suffix: &str, ids: &[u64]) -> (u64, usize) {
		let (id, wire_path, wire_hops) = encoder.start(Path::new(suffix).to_owned(), hops(ids));
		let size = encoder.size(&wire_path) + encoder.size(&wire_hops);
		let (got_path, got_hops) = decoder.start(wire_path, wire_hops).unwrap();
		assert_eq!(got_path.as_str(), suffix);
		assert_eq!(got_hops, hops(ids));
		(id.unwrap(), size)
	}

	#[test]
	fn encoder_output_decodes_to_its_input() {
		let mut encoder = AnnounceEncoder::new(VERSION);
		let mut decoder = AnnounceDecoder::default();

		// Several origins behind a shared pair of relays.
		let relays = [1u64 << 40, 1u64 << 41];
		let mut ids = Vec::new();
		for n in 0..40u64 {
			let origin = 1000 + n % 4;
			let suffix = format!("pid{}/private/channel_{}/stream-health-{}", n % 4, n % 7, n);
			let (id, _) = start(&mut encoder, &mut decoder, &suffix, &[origin, relays[0], relays[1]]);
			ids.push(id);
			// Retire every third one so bases come and go.
			if n % 3 == 0 {
				let id = ids.remove(0);
				encoder.end(id);
				decoder.end(id).unwrap();
			}
		}

		// Updates resolve against the chain they replace.
		for id in ids.iter().copied().take(5) {
			let chain = hops(&[2000, relays[1]]);
			let wire = encoder.update(id, chain.clone());
			let (_, got) = decoder.update(id, wire).unwrap();
			assert_eq!(got, chain);
		}

		// And a later START still matches what the encoder remembers.
		start(
			&mut encoder,
			&mut decoder,
			"pid0/private/channel_0/x",
			&[1000, relays[0], relays[1]],
		);
	}

	#[test]
	fn encoder_picks_the_longest_shared_head_and_tail() {
		let mut encoder = AnnounceEncoder::new(VERSION);
		let mut decoder = AnnounceDecoder::default();
		start(&mut encoder, &mut decoder, "a/b/c", &[1u64 << 50, 1u64 << 51]);
		// Byte order puts `a/b-z` between `a/b` and `a/b/...`; segment order must not.
		start(&mut encoder, &mut decoder, "a/b-z", &[3]);

		let (_, wire_path, wire_hops) = encoder.start(Path::new("a/b/d").to_owned(), hops(&[5, 1u64 << 51]));
		assert_eq!(wire_path, path(2, 2, "d"));
		assert_eq!(wire_hops, chain(2, &[5], 1));
	}

	// The byte-level comparison must agree with comparing segment lists.
	#[test]
	fn segments_order_matches_segment_lists() {
		let paths = [
			"", "a", "a/b", "a-b", "a/b-z", "a/b/c", "ab", "a/ba", "b", "a.b/c", "a\u{0}b",
		];
		for x in paths {
			for y in paths {
				let (px, py) = (Path::new(x).to_owned(), Path::new(y).to_owned());
				let expected = px.parts().cmp(py.parts());
				assert_eq!(Segments(px).cmp(&Segments(py)), expected, "{x:?} vs {y:?}");
			}
		}
	}

	#[test]
	fn encoder_sends_literal_when_nothing_is_shared() {
		let mut encoder = AnnounceEncoder::new(VERSION);
		let mut decoder = AnnounceDecoder::default();
		start(&mut encoder, &mut decoder, "a", &[1]);
		let (_, wire_path, wire_hops) = encoder.start(Path::new("b").to_owned(), hops(&[2]));
		assert_eq!(wire_path, PathRef::literal(Path::new("b")));
		assert_eq!(wire_hops, HopsRef::literal(hops(&[2])));
	}

	#[test]
	fn lite06_assigns_ids_without_bases() {
		let mut encoder = AnnounceEncoder::new(Version::Lite06);
		let (id, _, _) = encoder.start(Path::new("a/b").to_owned(), hops(&[1]));
		assert_eq!(id, Some(0));
		let (id, wire_path, wire_hops) = encoder.start(Path::new("a/c").to_owned(), hops(&[1]));
		assert_eq!(id, Some(1));
		assert_eq!(wire_path, PathRef::literal(Path::new("a/c")));
		assert_eq!(wire_hops, HopsRef::literal(hops(&[1])));
		encoder.end(0);
		assert_eq!(encoder.update(1, hops(&[1])), HopsRef::literal(hops(&[1])));
	}

	/// A compressed stream pinned byte for byte: `js/net/src/lite/announce.test.ts`
	/// decodes the same hex, so a codec change on either side breaks both.
	const GOLDEN: &str = "001600000a726f6f6d2f612f63616d000251116222000000000d0102036d696301017333010000020a00010180004444010000010101000d02010162020180005555010000";

	fn golden() -> Vec<crate::fuzz::Announced> {
		use crate::fuzz::Announced;
		vec![
			Announced::Start(Path::new("room/a/cam").to_owned(), hops(&[0x1111, 0x2222])),
			Announced::Start(Path::new("room/a/mic").to_owned(), hops(&[0x3333, 0x2222])),
			Announced::Update(0, hops(&[0x4444, 0x2222])),
			Announced::End(1),
			Announced::Start(Path::new("room/b").to_owned(), hops(&[0x5555, 0x2222])),
		]
	}

	#[test]
	fn golden_stream_is_pinned() {
		let encoded = crate::fuzz::encode_announces(&golden(), true);
		let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
		assert_eq!(hex, GOLDEN);
		assert_eq!(crate::fuzz::decode_announces(&encoded, true), golden());
	}

	/// The literal lite-07 stream `js/net` writes for the same announcements, which
	/// never picks a base.
	const JS_LITERAL: &str = "001600000a726f6f6d2f612f63616d000251116222000000001600000a726f6f6d2f612f6d6963000273336222000000020c0000028000444462220000000101010014000006726f6f6d2f620002800055556222000000";

	#[test]
	fn js_literal_stream_decodes() {
		let bytes: Vec<u8> = (0..JS_LITERAL.len())
			.step_by(2)
			.map(|i| u8::from_str_radix(&JS_LITERAL[i..i + 2], 16).unwrap())
			.collect();
		assert_eq!(crate::fuzz::decode_announces(&bytes, true), golden());
	}
}
