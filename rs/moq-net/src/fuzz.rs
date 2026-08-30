//! Fuzz target bodies for the wire codecs, plus the seeds they start from.
//!
//! Each `pub fn` here is one target: it takes raw fuzzer bytes, drives a decoder, and
//! checks that whatever came out survives a re-encode. The bodies live in the crate
//! rather than in `fuzz/fuzz_targets/` because the `lite` and `ietf` modules are
//! private modules, so an outside harness cannot reach a single decoder. Keeping them
//! here also lets the tests below replay them on stable, which is what turns a crash
//! the fuzzer found into a regression `just test` runs.
//!
//! Compiled only under `cfg(test)` or the `fuzz` feature, so none of this is part of
//! the published API. See `fuzz/README.md` for the workflow.

use bytes::Buf;

use crate::{
	Path, PathRelative,
	coding::{Decode, Encode, VarInt},
	ietf, lite,
};

/// One fuzz target body: it returns whether the input decoded, which is what
/// [`seeds`] is checked against.
pub type Target = fn(&[u8]) -> bool;

/// Every target, keyed by the name of its `fuzz_targets/<name>.rs` shim.
pub const TARGETS: &[(&str, Target)] = &[
	("lite", lite_wire),
	("ietf", ietf_wire),
	("varint", varint),
	("path", path),
];

/// The moq-lite versions a target decodes at, selected by the input's first byte.
const LITE_VERSIONS: &[lite::Version] = &[
	lite::Version::Lite01,
	lite::Version::Lite02,
	lite::Version::Lite03,
	lite::Version::Lite04,
	lite::Version::Lite05,
	lite::Version::Lite06Wip,
];

/// The moq-transport drafts a target decodes at, selected by the input's first byte.
const IETF_VERSIONS: &[ietf::Version] = &[
	ietf::Version::Draft14,
	ietf::Version::Draft15,
	ietf::Version::Draft16,
	ietf::Version::Draft17,
	ietf::Version::Draft18,
	ietf::Version::Draft19,
];

/// How many types [`lite_wire`] dispatches over.
const LITE_KINDS: u8 = 21;

/// How many types [`ietf_wire`] dispatches over.
const IETF_KINDS: u8 = 37;

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
///
/// `stable` asks for the stronger check that the second encoding matches the first,
/// byte for byte. It is off wherever a parameter map reaches the wire, because those
/// encoders walk a `HashMap`, whose iteration order differs per instance.
fn roundtrip<T, V>(data: &[u8], version: V, stable: bool) -> bool
where
	T: Decode<V> + Encode<V>,
	V: Copy,
{
	let mut buf = data;
	let Ok(decoded) = T::decode(&mut buf, version) else {
		return false;
	};

	let Ok(first) = decoded.encode_bytes(version) else {
		return true;
	};

	let mut echo = first.clone();
	let decoded = T::decode(&mut echo, version).expect("could not decode our own encoding");
	assert!(
		!echo.has_remaining(),
		"our own encoding left {} bytes",
		echo.remaining()
	);

	let Ok(second) = decoded.encode_bytes(version) else {
		panic!("could not re-encode what we just encoded");
	};

	if stable {
		assert_eq!(first, second, "encoding is not stable");
	}

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

	// SETUP is a parameter map, and so is the map itself.
	let stable = !matches!(kind, 0 | 20);

	match kind {
		0 => roundtrip::<lite::Setup, _>(rest, version, stable),
		1 => roundtrip::<lite::SessionInfo, _>(rest, version, stable),
		2 => roundtrip::<lite::AnnounceInit<'static>, _>(rest, version, stable),
		3 => roundtrip::<lite::AnnounceRequest<'static>, _>(rest, version, stable),
		4 => roundtrip::<lite::AnnounceOk, _>(rest, version, stable),
		5 => roundtrip::<lite::AnnounceBroadcast<'static>, _>(rest, version, stable),
		6 => roundtrip::<lite::Subscribe<'static>, _>(rest, version, stable),
		7 => roundtrip::<lite::SubscribeOk, _>(rest, version, stable),
		8 => roundtrip::<lite::SubscribeStart, _>(rest, version, stable),
		9 => roundtrip::<lite::SubscribeEnd, _>(rest, version, stable),
		10 => roundtrip::<lite::SubscribeUpdate, _>(rest, version, stable),
		11 => roundtrip::<lite::SubscribeDrop, _>(rest, version, stable),
		12 => roundtrip::<lite::SubscribeResponse, _>(rest, version, stable),
		13 => roundtrip::<lite::Fetch<'static>, _>(rest, version, stable),
		14 => roundtrip::<lite::Group, _>(rest, version, stable),
		15 => roundtrip::<lite::Goaway<'static>, _>(rest, version, stable),
		16 => roundtrip::<lite::Track<'static>, _>(rest, version, stable),
		17 => roundtrip::<lite::TrackInfo, _>(rest, version, stable),
		18 => roundtrip::<lite::Probe, _>(rest, version, stable),
		19 => roundtrip::<lite::Datagram, _>(rest, version, stable),
		20 => roundtrip::<lite::Parameters, _>(rest, version, stable),
		_ => unreachable!("kind is taken modulo LITE_KINDS"),
	}
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

	// Draft-14 and draft-15 write a parameter map straight out of a `HashMap`; every
	// later draft sorts by key first, so only these two are order-dependent.
	let stable = !matches!(version, ietf::Version::Draft14 | ietf::Version::Draft15);

	match kind {
		0 => roundtrip::<ietf::GoAway<'static>, _>(rest, version, stable),
		1 => roundtrip::<ietf::Fetch<'static>, _>(rest, version, stable),
		2 => roundtrip::<ietf::FetchOk, _>(rest, version, stable),
		3 => roundtrip::<ietf::FetchError<'static>, _>(rest, version, stable),
		4 => roundtrip::<ietf::FetchCancel, _>(rest, version, stable),
		5 => roundtrip::<ietf::FetchHeader, _>(rest, version, stable),
		6 => roundtrip::<ietf::FetchType<'static>, _>(rest, version, stable),
		7 => roundtrip::<ietf::PublishNamespace<'static>, _>(rest, version, stable),
		8 => roundtrip::<ietf::PublishNamespaceOk, _>(rest, version, stable),
		9 => roundtrip::<ietf::PublishNamespaceError<'static>, _>(rest, version, stable),
		10 => roundtrip::<ietf::PublishNamespaceDone<'static>, _>(rest, version, stable),
		11 => roundtrip::<ietf::PublishNamespaceCancel<'static>, _>(rest, version, stable),
		12 => roundtrip::<ietf::TrackStatus<'static>, _>(rest, version, stable),
		13 => roundtrip::<ietf::Publish<'static>, _>(rest, version, stable),
		14 => roundtrip::<ietf::PublishOk, _>(rest, version, stable),
		15 => roundtrip::<ietf::PublishError<'static>, _>(rest, version, stable),
		16 => roundtrip::<ietf::PublishDone<'static>, _>(rest, version, stable),
		17 => roundtrip::<ietf::PublishBlocked<'static>, _>(rest, version, stable),
		18 => roundtrip::<ietf::Subscribe<'static>, _>(rest, version, stable),
		19 => roundtrip::<ietf::SubscribeOk, _>(rest, version, stable),
		20 => roundtrip::<ietf::SubscribeError<'static>, _>(rest, version, stable),
		21 => roundtrip::<ietf::SubscribeUpdate, _>(rest, version, stable),
		22 => roundtrip::<ietf::Unsubscribe, _>(rest, version, stable),
		23 => roundtrip::<ietf::SubscribeNamespace<'static>, _>(rest, version, stable),
		24 => roundtrip::<ietf::SubscribeNamespaceLegacy<'static>, _>(rest, version, stable),
		25 => roundtrip::<ietf::SubscribeNamespaceOk, _>(rest, version, stable),
		26 => roundtrip::<ietf::SubscribeNamespaceError<'static>, _>(rest, version, stable),
		27 => roundtrip::<ietf::UnsubscribeNamespace, _>(rest, version, stable),
		28 => roundtrip::<ietf::Namespace<'static>, _>(rest, version, stable),
		29 => roundtrip::<ietf::NamespaceDone<'static>, _>(rest, version, stable),
		30 => roundtrip::<ietf::MaxRequestId, _>(rest, version, stable),
		31 => roundtrip::<ietf::RequestsBlocked, _>(rest, version, stable),
		32 => roundtrip::<ietf::RequestOk, _>(rest, version, stable),
		33 => roundtrip::<ietf::RequestError<'static>, _>(rest, version, stable),
		34 => roundtrip::<ietf::GroupHeader, _>(rest, version, stable),
		35 => roundtrip::<ietf::Parameters, _>(rest, version, stable),
		36 => roundtrip::<ietf::Location, _>(rest, version, stable),
		_ => unreachable!("kind is taken modulo IETF_KINDS"),
	}
}

/// Decode a varint with whichever codec the selected version uses.
///
/// Byte 0 picks the version, which is the whole point: moq-lite and drafts 14-16 use
/// the QUIC two-bit length tag, while draft-17+ counts leading ones, and the two
/// disagree about which byte sequences are even legal.
///
/// The decoded value is deliberately not asserted to be within [`VarInt::MAX`]: the
/// leading-ones form spans the full `u64` by design, so a 9-byte encoding decodes
/// above the 62-bit ceiling and only fails when re-encoded for a QUIC-form version.
pub fn varint(data: &[u8]) -> bool {
	let Some((&selector, rest)) = data.split_first() else {
		return false;
	};

	// Half the space to each family, so both codecs get exercised.
	let version: crate::Version = match selector % 2 {
		0 => LITE_VERSIONS[(selector as usize / 2) % LITE_VERSIONS.len()].into(),
		_ => IETF_VERSIONS[(selector as usize / 2) % IETF_VERSIONS.len()].into(),
	};

	let mut buf = rest;
	let Ok(value) = VarInt::decode(&mut buf, version) else {
		return false;
	};

	// Zigzag is a pure mapping on top of the wire value, so it must round-trip
	// whenever the signed value is back in range.
	let signed = value.to_zigzag();
	if let Ok(mapped) = VarInt::from_zigzag(signed) {
		assert_eq!(mapped.to_zigzag(), signed, "zigzag is not its own inverse");
	}

	let Ok(encoded) = value.encode_bytes(version) else {
		return true;
	};

	let mut echo = encoded.clone();
	let again = VarInt::decode(&mut echo, version).expect("could not decode our own encoding");
	assert!(
		!echo.has_remaining(),
		"our own encoding left {} bytes",
		echo.remaining()
	);
	assert_eq!(value, again, "varint did not survive a round trip");

	true
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
	let rel = PathRelative::new(base.as_str());
	if let Some(resolved) = target.try_resolve(&rel) {
		assert_eq!(resolved, target.resolve(&rel), "try_resolve disagreed with resolve");
	}

	// The wire form is the same path back, whenever the path is expressible at all.
	let version = lite::Version::Lite05;
	if let Ok(encoded) = target.encode_bytes(version) {
		let mut echo = encoded;
		let decoded = Path::decode(&mut echo, version).expect("could not decode our own encoding");
		assert!(
			!echo.has_remaining(),
			"our own encoding left {} bytes",
			echo.remaining()
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
