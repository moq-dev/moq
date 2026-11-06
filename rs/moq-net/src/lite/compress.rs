//! Lite-07 announce compression: an ANNOUNCE_START may copy the
//! head of a live announcement's path, naming that base by its distance back from the
//! stream's next unassigned Announce ID.
//!
//! The codec in `announce.rs` stays stateless and carries bases as they travel. The
//! decoder here resolves them against the stream's live announcements, which the
//! subscriber has to track anyway, and the encoder picks them.

use std::collections::{BTreeSet, HashMap};

use crate::{
	Error, Path,
	coding::{Form, varint},
};

use super::{PathRef, Version};

/// The live announcements on one stream by Announce ID, each with its wire suffix,
/// which is all a base can name.
#[derive(Debug, Default)]
struct Live {
	next: u64,
	suffixes: HashMap<u64, Path>,
}

impl Live {
	/// The suffix of the live announcement with this id.
	fn suffix(&self, id: u64) -> Result<&Path, Error> {
		self.suffixes.get(&id).ok_or(Error::ProtocolViolation)
	}

	/// The live announcement `distance` back from the next id, or `None` for 0.
	fn base(&self, distance: u64) -> Result<Option<&Path>, Error> {
		if distance == 0 {
			return Ok(None);
		}
		let id = self.next.checked_sub(distance).ok_or(Error::ProtocolViolation)?;
		self.suffix(id).map(Some)
	}

	fn resolve(&self, path: PathRef) -> Result<Path, Error> {
		let Some(base) = self.base(path.base)? else {
			return Ok(path.rest);
		};
		let keep = usize::try_from(path.keep).map_err(|_| Error::ProtocolViolation)?;
		let head = head(base, keep).ok_or(Error::ProtocolViolation)?;
		let suffix = match (head.is_empty(), path.rest.is_empty()) {
			(true, _) => Ok(path.rest),
			(false, true) => Path::new(head),
			(false, false) => Path::concat(head, path.rest.as_str()),
		}
		.map_err(|_| Error::ProtocolViolation)?;
		if suffix.parts().count() > Path::MAX_PARTS {
			return Err(Error::ProtocolViolation);
		}
		Ok(suffix)
	}
}

/// The first `keep` segments of `path`, or `None` if it has fewer.
fn head(path: &Path, keep: usize) -> Option<&str> {
	if keep == 0 {
		return Some("");
	}
	let s = path.as_str();
	let mut ends = s
		.match_indices('/')
		.map(|(i, _)| i)
		.chain((!s.is_empty()).then_some(s.len()));
	ends.nth(keep - 1).map(|end| &s[..end])
}

/// Resolves the bases on a received announce stream. Only used on versions with
/// announce ids; before lite-06 nothing is ever referenced, so nothing is recorded.
#[derive(Debug, Default)]
pub(crate) struct AnnounceDecoder {
	live: Live,
}

impl AnnounceDecoder {
	/// An ANNOUNCE_START: resolve its suffix and assign it the next id.
	pub fn start(&mut self, suffix: PathRef) -> Result<Path, Error> {
		let suffix = self.live.resolve(suffix)?;
		let id = self.live.next;
		self.live.next += 1;
		self.live.suffixes.insert(id, suffix.clone());
		Ok(suffix)
	}

	/// The id the last start assigned.
	pub fn last(&self) -> u64 {
		self.live.next.saturating_sub(1)
	}

	/// The suffix of the live id an ANNOUNCE_UPDATE or ANNOUNCE_RESTART names, which
	/// stays live.
	pub fn get(&self, id: u64) -> Result<Path, Error> {
		self.live.suffix(id).cloned()
	}

	/// An ANNOUNCE_END: retire the id, returning its suffix.
	pub fn end(&mut self, id: u64) -> Result<Path, Error> {
		self.live.suffixes.remove(&id).ok_or(Error::ProtocolViolation)
	}
}

/// The live ids under a run of leading path segments. Every node holds the ids of the
/// entries beneath it, so the entry sharing the longest head with a query is found by
/// walking the query down once, at a cost that grows with its length rather than with
/// the number of entries.
#[derive(Debug, Default)]
struct Trie {
	children: HashMap<String, Node>,
}

#[derive(Debug)]
struct Node {
	ids: BTreeSet<u64>,
	children: HashMap<String, Node>,
}

impl Trie {
	fn insert<'a>(&mut self, keys: impl Iterator<Item = &'a str>, id: u64) {
		let mut children = &mut self.children;
		for key in keys {
			let node = children.entry(key.to_owned()).or_insert_with(|| Node {
				ids: BTreeSet::new(),
				children: HashMap::new(),
			});
			node.ids.insert(id);
			children = &mut node.children;
		}
	}

	fn remove<'a>(&mut self, keys: impl Iterator<Item = &'a str>, id: u64) {
		Self::remove_from(&mut self.children, keys, id);
	}

	fn remove_from<'a>(children: &mut HashMap<String, Node>, mut keys: impl Iterator<Item = &'a str>, id: u64) {
		let Some(key) = keys.next() else {
			return;
		};
		let Some(node) = children.get_mut(key) else {
			return;
		};
		node.ids.remove(&id);
		// Nothing else passes through here, so nothing else is beneath it either.
		if node.ids.is_empty() {
			children.remove(key);
			return;
		}
		Self::remove_from(&mut node.children, keys, id);
	}

	/// How many leading keys the query shares with its closest entry, and that entry's
	/// id, preferring the newest (the smallest distance to encode).
	fn longest<'a>(&self, keys: impl Iterator<Item = &'a str>) -> Option<(usize, u64)> {
		let mut children = &self.children;
		let mut best = None;
		for (depth, key) in keys.enumerate() {
			let Some(node) = children.get(key) else {
				break;
			};
			let newest = *node.ids.last().expect("a node without ids is pruned");
			best = Some((depth + 1, newest));
			children = &node.children;
		}
		best
	}
}

/// Picks the bases for an outgoing announce stream, and assigns Announce IDs on
/// versions that have them. Only lite-07 keeps the live set, since nothing earlier
/// can name a base.
///
/// A trie over the live paths by segment finds the longest shared head. A base is
/// used only when it encodes smaller than the literal.
#[derive(Debug)]
pub(crate) struct AnnounceEncoder {
	version: Version,
	live: Live,
	heads: Trie,
}

impl AnnounceEncoder {
	pub fn new(version: Version) -> Self {
		Self {
			version,
			live: Live::default(),
			heads: Trie::default(),
		}
	}

	/// An ANNOUNCE_START: its id (on versions that assign one) and its wire suffix.
	pub fn start(&mut self, suffix: Path) -> (Option<u64>, PathRef) {
		if !self.version.has_announce_id() {
			return (None, PathRef::literal(suffix));
		}

		let id = self.live.next;
		self.live.next += 1;

		if !self.version.has_announce_compression() {
			return (Some(id), PathRef::literal(suffix));
		}

		// Distances count back from the id this START takes.
		let path = self.path_ref(&suffix, id);
		self.heads.insert(suffix.parts(), id);
		self.live.suffixes.insert(id, suffix);
		(Some(id), path)
	}

	/// An ANNOUNCE_END for a live `id`.
	pub fn end(&mut self, id: u64) {
		if !self.version.has_announce_compression() {
			return;
		}
		let suffix = self.live.suffixes.remove(&id).expect("end for a live id");
		self.heads.remove(suffix.parts(), id);
	}

	/// The wire form of `suffix`, sized from its parts so only the winner is built.
	fn path_ref(&self, suffix: &Path, next: u64) -> PathRef {
		let form: Form = self.version.into();
		let literal = || PathRef::literal(suffix.clone());
		let Some((keep, id)) = self.heads.longest(suffix.parts()) else {
			return literal();
		};
		// Where the segments after `keep` start, sharing the suffix's buffer.
		let s = suffix.as_str();
		let start = s.match_indices('/').nth(keep - 1).map_or(s.len(), |(i, _)| i + 1);
		let rest = suffix.slice_from(start);

		let base = next - id;
		let keep = keep as u64;
		let size = varint_size(base, form) + varint_size(keep, form) + string_size(&rest, form);
		match size < varint_size(0, form) * 2 + string_size(suffix, form) {
			true => PathRef { base, keep, rest },
			false => literal(),
		}
	}
}

/// The bytes `value` takes as a varint in `form`.
fn varint_size(value: u64, form: Form) -> usize {
	varint::size(value, form).expect("sizing a value in varint range")
}

/// The bytes `path` takes on the wire: a varint length, then the string.
fn string_size(path: &Path, form: Form) -> usize {
	let len = path.as_str().len();
	varint_size(len as u64, form) + len
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::coding::Encode;

	const VERSION: Version = Version::Lite07;

	fn path(base: u64, keep: u64, rest: &str) -> PathRef {
		PathRef {
			base,
			keep,
			rest: Path::new(rest).unwrap(),
		}
	}

	fn literal(suffix: &str) -> PathRef {
		path(0, 0, suffix)
	}

	#[test]
	fn resolves_a_path_head() {
		let mut decoder = AnnounceDecoder::default();
		decoder.start(literal("pid/private/channel_1/health-1")).unwrap();

		// Three leading segments of the latest START.
		let suffix = decoder.start(path(1, 3, "health-2")).unwrap();
		assert_eq!(suffix.as_str(), "pid/private/channel_1/health-2");
	}

	#[test]
	fn keeps_the_whole_path() {
		let mut decoder = AnnounceDecoder::default();
		decoder.start(literal("a/b")).unwrap();
		// A full keep with a rest names a route beneath the base's: not a duplicate,
		// since the rest extends it.
		let suffix = decoder.start(path(1, 2, "c")).unwrap();
		assert_eq!(suffix.as_str(), "a/b/c");
		// And a full keep with an empty rest copies it whole.
		let suffix = decoder.start(path(1, 3, "")).unwrap();
		assert_eq!(suffix.as_str(), "a/b/c");
	}

	#[test]
	fn a_new_stream_has_no_base() {
		let mut decoder = AnnounceDecoder::default();
		assert!(matches!(decoder.start(path(1, 0, "a")), Err(Error::ProtocolViolation)));
	}

	#[test]
	fn rejects_a_retired_or_unassigned_base() {
		let mut decoder = AnnounceDecoder::default();
		decoder.start(literal("a")).unwrap();
		decoder.start(literal("b")).unwrap();
		decoder.end(0).unwrap();

		// Id 0 is two back, and retired.
		assert!(matches!(decoder.start(path(2, 1, "x")), Err(Error::ProtocolViolation)));
		// Three back was never assigned.
		assert!(matches!(decoder.start(path(3, 0, "x")), Err(Error::ProtocolViolation)));
		// Id 1 is still live, so reusing it works after the END.
		let suffix = decoder.start(path(1, 1, "x")).unwrap();
		assert_eq!(suffix.as_str(), "b/x");
	}

	#[test]
	fn rejects_a_keep_longer_than_the_base() {
		let mut decoder = AnnounceDecoder::default();
		decoder.start(literal("a/b")).unwrap();
		assert!(matches!(decoder.start(path(1, 3, "x")), Err(Error::ProtocolViolation)));
	}

	#[test]
	fn rejects_a_resolved_path_too_deep() {
		let mut decoder = AnnounceDecoder::default();
		let deep = vec!["x"; Path::MAX_PARTS].join("/");
		decoder.start(literal(&deep)).unwrap();
		assert!(matches!(
			decoder.start(path(1, Path::MAX_PARTS as u64, "y")),
			Err(Error::ProtocolViolation)
		));
	}

	#[test]
	fn rejects_a_resolved_path_too_long() {
		let mut decoder = AnnounceDecoder::default();
		let head = "x".repeat(Path::MAX_BYTES - 2);
		decoder.start(literal(&head)).unwrap();

		// `head/y` is exactly the limit, and one byte more is refused.
		let suffix = decoder.start(path(1, 1, "y")).unwrap();
		assert_eq!(suffix.len(), Path::MAX_BYTES);
		assert!(matches!(decoder.start(path(1, 1, "yz")), Err(Error::ProtocolViolation)));
	}

	/// Encode a START with `encoder`, decode it with `decoder`, and check that the
	/// resolved suffix matches the input. Returns its id and wire size.
	fn start(encoder: &mut AnnounceEncoder, decoder: &mut AnnounceDecoder, suffix: &str) -> (u64, usize) {
		let (id, wire) = encoder.start(Path::new(suffix).unwrap());
		let size = wire.encode_bytes(VERSION).unwrap().len();
		assert_eq!(decoder.start(wire).unwrap().as_str(), suffix);
		(id.unwrap(), size)
	}

	#[test]
	fn encoder_output_decodes_to_its_input() {
		let mut encoder = AnnounceEncoder::new(VERSION);
		let mut decoder = AnnounceDecoder::default();

		let mut ids = Vec::new();
		for n in 0..40u64 {
			let suffix = format!("pid{}/private/channel_{}/stream-health-{}", n % 4, n % 7, n);
			let (id, _) = start(&mut encoder, &mut decoder, &suffix);
			ids.push(id);
			// Retire every third one so bases come and go.
			if n % 3 == 0 {
				let id = ids.remove(0);
				encoder.end(id);
				decoder.end(id).unwrap();
			}
		}

		// A later START still matches what the encoder remembers.
		start(&mut encoder, &mut decoder, "pid0/private/channel_0/x");
	}

	#[test]
	fn encoder_picks_the_longest_shared_head() {
		let mut encoder = AnnounceEncoder::new(VERSION);
		let mut decoder = AnnounceDecoder::default();
		start(&mut encoder, &mut decoder, "a/b/c");
		// Shares a byte prefix with `a/b/d`, but no second segment.
		start(&mut encoder, &mut decoder, "a/b-z");

		let (_, wire) = encoder.start(Path::from_static("a/b/d"));
		assert_eq!(wire, path(2, 2, "d"));
	}

	#[test]
	fn encoder_sends_literal_when_nothing_is_shared() {
		let mut encoder = AnnounceEncoder::new(VERSION);
		let mut decoder = AnnounceDecoder::default();
		start(&mut encoder, &mut decoder, "a");
		let (_, wire) = encoder.start(Path::from_static("b"));
		assert_eq!(wire, PathRef::literal(Path::from_static("b")));
	}

	#[test]
	fn lite06_assigns_ids_without_bases() {
		let mut encoder = AnnounceEncoder::new(Version::Lite06);
		let (id, _) = encoder.start(Path::from_static("a/b"));
		assert_eq!(id, Some(0));
		let (id, wire) = encoder.start(Path::from_static("a/c"));
		assert_eq!(id, Some(1));
		assert_eq!(wire, PathRef::literal(Path::from_static("a/c")));
		encoder.end(0);
	}

	/// A compressed stream pinned byte for byte: `js/net/src/lite/announce.test.ts`
	/// decodes the same hex, so a codec change on either side breaks both.
	const GOLDEN: &str =
		"001200000a726f6f6d2f612f63616d0001610000000b0102036d69630001610000020200000101010009020101620001610000";

	#[cfg(feature = "async")]
	fn golden() -> Vec<crate::fuzz::Announced> {
		use crate::fuzz::Announced;
		// Lite-07 names a route instead of a hop chain, so none travels.
		vec![
			Announced::Start(Path::from_static("room/a/cam"), vec![]),
			Announced::Start(Path::from_static("room/a/mic"), vec![]),
			Announced::Update(0, vec![]),
			Announced::End(1),
			Announced::Start(Path::from_static("room/b"), vec![]),
		]
	}

	#[test]
	#[cfg(feature = "async")]
	fn golden_stream_is_pinned() {
		let encoded = crate::fuzz::encode_announces(&golden(), true);
		let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
		assert_eq!(hex, GOLDEN);
		assert_eq!(crate::fuzz::decode_announces(&encoded, true), golden());
	}

	/// The literal lite-07 stream `js/net` writes for the same announcements, which
	/// never picks a base.
	const JS_LITERAL: &str = "001200000a726f6f6d2f612f63616d0001610000001200000a726f6f6d2f612f6d6963000161000002020000010101000e000006726f6f6d2f620001610000";

	#[test]
	#[cfg(feature = "async")]
	fn js_literal_stream_decodes() {
		let bytes: Vec<u8> = (0..JS_LITERAL.len())
			.step_by(2)
			.map(|i| u8::from_str_radix(&JS_LITERAL[i..i + 2], 16).unwrap())
			.collect();
		assert_eq!(crate::fuzz::decode_announces(&bytes, true), golden());
	}
}
