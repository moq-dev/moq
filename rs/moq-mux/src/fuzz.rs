//! Fuzz target bodies for the media parsers an untrusted publisher reaches, plus their seeds.
//!
//! Each target takes raw fuzzer bytes, drives one parser through its public entry point the way
//! ingest or a subscriber does, and returns whether the input was accepted. [`run`] wraps a target
//! with the invariant the fuzzer checks beyond "no panic": heap traffic bounded in proportion to
//! the input. The bodies live here rather than in `fuzz/fuzz_targets/` so the tests below replay
//! them on stable, which is what turns a crash the fuzzer found into a regression `just check`
//! runs.
//!
//! Compiled only under `cfg(test)` or the `fuzz` feature, so none of this is part of the published
//! API. See `fuzz/README.md` for the workflow.

use std::task::Poll;

use bytes::Bytes;

pub use crate::allocation::{Counting, Usage, measure};
use crate::import::{Container, ContainerFormat, ContainerInit, TrackStream, VideoFormat, VideoInit};

/// One fuzz target body: it returns whether the input was accepted, which is what [`seeds`] is
/// checked against.
pub type Target = fn(&[u8]) -> bool;

/// Every target, keyed by the name of its `fuzz_targets/<name>.rs` shim.
pub const TARGETS: &[(&str, Target)] = &[
	("fmp4", fmp4),
	("mkv", mkv),
	("ts", ts),
	("flv", flv),
	("cmaf", cmaf),
	("annexb", annexb),
	("catalog", catalog),
];

/// Bytes a target may allocate per input byte, summed over the whole run.
///
/// Counts every allocation, not what stays live, so it bounds copying as well as holding: a parser
/// that rebuffers or reparses its input on every chunk grows with the square of the input and
/// crosses any linear bound once the input is long enough. A keyframe of a dozen bytes opens a
/// group, whose state and HLS record cost a few KiB, which is what sets the slope.
const BYTES_PER_BYTE: usize = 1024;

/// Allocations a target may make per input byte, for the same dozen-byte keyframe.
const ALLOCATIONS_PER_BYTE: usize = 8;

/// What any input may allocate, however short: the broadcast, catalog, and importer a target sets
/// up before reading a byte, and the first catalog and HLS publishes, whose compressed groups each
/// open a DEFLATE encoder of a few hundred KiB.
const BASE_BYTES: usize = 4 << 20;

/// Allocations any input may make, however short.
const BASE_ALLOCATIONS: usize = 4096;

/// Run `target` on `data` and check its heap traffic stays within a bound linear in `data`.
///
/// Measures only when the binary registers [`Counting`] as its global allocator; this crate's tests
/// and every `fuzz/` target do.
pub fn run(target: Target, data: &[u8]) -> bool {
	let (accepted, usage) = measure(|| target(data));
	if !within(usage, data.len()) {
		// Lazily initialized statics (tracing callsites, compiled regexes) allocate once per
		// process, not per input, so only a second run's excess is the input's own.
		let (_, usage) = measure(|| target(data));
		assert!(
			within(usage, data.len()),
			"{} input bytes allocated {} bytes in {} allocations",
			data.len(),
			usage.allocated,
			usage.allocations,
		);
	}
	accepted
}

fn within(usage: Usage, len: usize) -> bool {
	usage.allocated <= BASE_BYTES + BYTES_PER_BYTE * len
		&& usage.allocations <= BASE_ALLOCATIONS + ALLOCATIONS_PER_BYTE * len
}

/// Split off a selector byte, and turn it into a write size: 0 writes the input whole.
fn chunked(data: &[u8], selector: u8) -> std::slice::Chunks<'_, u8> {
	data.chunks(match selector {
		0 => data.len().max(1),
		n => n as usize,
	})
}

fn broadcast() -> (moq_net::broadcast::Producer, crate::catalog::Producer) {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default())
		.expect("a fresh broadcast takes a catalog");
	(broadcast, catalog)
}

/// Import `data` as `format`, written in pieces the leading byte sizes, as ingest reads a socket.
fn container(format: ContainerFormat, data: &[u8]) -> bool {
	let Some((&selector, data)) = data.split_first() else {
		return false;
	};
	let (broadcast, catalog) = broadcast();
	let init = ContainerInit::new(format, Bytes::new());
	let Ok(mut import) = Container::<()>::new(broadcast, catalog.reserve(), &init) else {
		return false;
	};
	for chunk in chunked(data, selector) {
		if import.decode(chunk).is_err() {
			return false;
		}
	}
	import.finish().is_ok()
}

/// fMP4 import, as fMP4 and HLS ingest feed it.
pub fn fmp4(data: &[u8]) -> bool {
	container(ContainerFormat::Fmp4, data)
}

/// Matroska / WebM import.
pub fn mkv(data: &[u8]) -> bool {
	container(ContainerFormat::Mkv, data)
}

/// MPEG-TS import, as SRT and HLS ingest feed it.
pub fn ts(data: &[u8]) -> bool {
	container(ContainerFormat::Ts, data)
}

/// FLV import, as RTMP ingest feeds it.
pub fn flv(data: &[u8]) -> bool {
	container(ContainerFormat::Flv, data)
}

/// A subscriber decoding a CMAF track: a big-endian `u16` init length, the init segment a catalog
/// carries, then one moof+mdat frame.
pub fn cmaf(data: &[u8]) -> bool {
	use crate::container::Container as _;

	let Some((length, rest)) = data.split_first_chunk::<2>() else {
		return false;
	};
	let Some((init, fragment)) = rest.split_at_checked(u16::from_be_bytes(*length) as usize) else {
		return false;
	};
	let Ok(wire) = crate::container::fmp4::Wire::from_init(init) else {
		return false;
	};
	let frame = moq_net::frame::Frame::new(None, Bytes::copy_from_slice(fragment));
	wire.decode(frame).is_ok()
}

/// The codecs [`annexb`] splits, selected by the leading byte modulo their count.
const STREAMS: &[VideoFormat] = &[VideoFormat::Avc3, VideoFormat::Hev1, VideoFormat::Av01];

/// A raw byte stream of unknown frame boundaries: H.264 or H.265 Annex-B, or AV1 OBUs.
///
/// The leading byte picks the codec, and what remains of it once divided by the codec count is
/// the write size.
pub fn annexb(data: &[u8]) -> bool {
	let Some((&selector, data)) = data.split_first() else {
		return false;
	};
	let format = STREAMS[selector as usize % STREAMS.len()];
	let (broadcast, catalog) = broadcast();
	let request = broadcast
		.reserve_track("video")
		.expect("a fresh broadcast has no video track");
	let Ok(mut import) = TrackStream::video(request, catalog.reserve(), VideoInit::new(format, Bytes::new())) else {
		return false;
	};
	for chunk in chunked(data, selector / STREAMS.len() as u8) {
		if import.decode(chunk).is_err() {
			return false;
		}
	}
	import.finish().is_ok()
}

/// A subscriber reading a catalog track: the leading byte's low bit picks hang (0) or MSF (1),
/// and the rest is the frames of one group, split on NUL, which JSON text never contains.
///
/// The consumer reads after each frame, as it does when updates trickle in, and every catalog it
/// yields has each audio and video rendition's container built, as a subscriber does before
/// reading the track.
pub fn catalog(data: &[u8]) -> bool {
	let Some((&selector, data)) = data.split_first() else {
		return false;
	};
	let broadcast = moq_net::broadcast::Info::new().produce();
	let waiter = kio::Waiter::noop();

	let name = if selector & 1 == 0 {
		hang::Catalog::DEFAULT_NAME
	} else {
		moq_msf::DEFAULT_NAME
	};
	// Uncompressed: inflating a compressed group is moq-net's, ahead of any catalog parser.
	let info = hang::Catalog::default_track_info().with_compress(false);
	let track = broadcast
		.create_track(name, info)
		.expect("a fresh broadcast has no catalog");
	let subscriber = track.subscribe(None);
	let mut next: Box<dyn FnMut() -> Poll<crate::Result<Option<crate::catalog::hang::Catalog<()>>>>> =
		if selector & 1 == 0 {
			let mut consumer = crate::catalog::hang::Consumer::<()>::new(subscriber);
			Box::new(move || consumer.poll_next(&waiter))
		} else {
			let mut consumer = crate::catalog::msf::Consumer::<()>::new(subscriber);
			Box::new(move || consumer.poll_next(&waiter))
		};

	let mut group = track.append_group().expect("a fresh track takes a group");
	let mut accepted = false;
	for frame in data.split(|&byte| byte == 0) {
		if group
			.write_frame(moq_net::Timestamp::ZERO, Bytes::copy_from_slice(frame))
			.is_err()
		{
			return false;
		}
		match next() {
			Poll::Ready(Ok(Some(catalog))) => accepted = containers(&catalog),
			Poll::Ready(Ok(None)) | Poll::Pending => {}
			Poll::Ready(Err(_)) => return false,
		}
	}
	accepted
}

/// Build every audio and video rendition's container, returning whether all of them parsed.
fn containers(catalog: &crate::catalog::hang::Catalog<()>) -> bool {
	use crate::catalog::hang::Container;

	let video = catalog.video.renditions.values().map(Container::try_from);
	let audio = catalog.audio.renditions.values().map(Container::try_from);
	video.chain(audio).all(|container| container.is_ok())
}

/// One input to start a target from.
pub struct Seed {
	/// The target it feeds, a key of [`TARGETS`].
	pub target: &'static str,
	/// The input, selector bytes included.
	pub data: Vec<u8>,
}

/// The starting corpus: the crate's media fixtures, wrapped in each target's framing.
///
/// Every seed is accepted, so the fuzzer starts from inputs that reach past the first header.
pub fn seeds() -> Vec<Seed> {
	let mut seeds = Vec::new();
	let mut add = |target: &'static str, selector: u8, data: &[u8]| {
		let mut input = vec![selector];
		input.extend_from_slice(data);
		seeds.push(Seed { target, data: input });
	};

	// bbb.mp4 ends on a moof without its mdat, so keep its first four whole fragments.
	let bbb = &include_bytes!("container/fmp4/test_data/bbb.mp4")[..2035];
	for fmp4 in [
		bbb,
		include_bytes!("container/fmp4/test_data/av1.mp4"),
		include_bytes!("container/fmp4/test_data/vp9.mp4"),
	] {
		add("fmp4", 0, fmp4);
	}
	// Small writes too, so the corpus covers an atom split across reads.
	add("fmp4", 100, bbb);

	add("mkv", 0, include_bytes!("container/mkv/test_data/opus.webm"));
	add("mkv", 100, include_bytes!("container/mkv/test_data/opus.webm"));

	for ts in [
		&include_bytes!("container/ts/test_data/bbb.ts")[..],
		include_bytes!("container/ts/test_data/aac_quad.ts"),
		include_bytes!("container/ts/test_data/poc-avc.ts"),
		include_bytes!("container/ts/test_data/poc-hevc.ts"),
	] {
		add("ts", 0, ts);
	}
	add("ts", 100, include_bytes!("container/ts/test_data/bbb.ts"));

	add("flv", 0, include_bytes!("container/ts/test_data/he_aac.flv"));
	add("flv", 100, include_bytes!("container/ts/test_data/he_aac_v2.flv"));

	for (stream, unit) in [h264_stream(), h265_stream(), av1_stream()].into_iter().enumerate() {
		let selector = stream as u8;
		let data = [unit.clone(), unit].concat();
		add("annexb", selector, &data);
		// The write size is the selector divided by the codec count.
		add("annexb", selector + 7 * STREAMS.len() as u8, &data);
	}

	for fixture in cmaf_fixtures() {
		let mut data = u16::try_from(fixture.init.len())
			.expect("a fixture init fits a u16 length")
			.to_be_bytes()
			.to_vec();
		data.extend_from_slice(&fixture.init);
		data.extend_from_slice(&fixture.fragment);
		seeds.push(Seed { target: "cmaf", data });
	}

	let mut add = |selector: u8, frames: &[&[u8]]| {
		let mut data = vec![selector];
		data.extend_from_slice(&frames.join(&0));
		seeds.push(Seed {
			target: "catalog",
			data,
		});
	};
	for (cmaf, catalog) in [(false, imported(false)), (true, imported(true))] {
		let hang = catalog.to_json().expect("a published catalog serializes");
		add(0, &[hang.as_bytes()]);
		let msf = crate::catalog::to_msf(&catalog, &moq_net::Path::empty());
		let msf = msf.to_json().expect("a published catalog serializes");
		add(1, &[msf.as_bytes()]);
		if cmaf {
			// A snapshot, then a merge-patch delta in the same group.
			add(0, &[hang.as_bytes(), br#"{"user":{"name":"bunny"}}"#]);
		}
	}
	add(0, &[include_bytes!("../../hang/fixtures/catalog-clock.json")]);
	add(0, &[include_bytes!("../../hang/fixtures/catalog-stats-echo.json")]);

	seeds
}

/// The catalog an import of bbb.mp4 publishes, with CMAF passthrough or without.
fn imported(passthrough: bool) -> crate::catalog::hang::Catalog<()> {
	let (broadcast, catalog) = broadcast();
	let mut import = crate::container::fmp4::Import::new(broadcast, catalog.reserve()).with_passthrough(passthrough);
	import
		.decode(&include_bytes!("container/fmp4/test_data/bbb.mp4")[..2035])
		.expect("the fixture imports");
	catalog.snapshot()
}

/// The moov of an MP4 fixture.
fn moov(mp4: &[u8]) -> mp4_atom::Moov {
	use mp4_atom::DecodeMaybe;

	let mut cursor = std::io::Cursor::new(mp4);
	while let Some(atom) = mp4_atom::Any::decode_maybe(&mut cursor).expect("the fixture parses") {
		if let mp4_atom::Any::Moov(moov) = atom {
			return moov;
		}
	}
	panic!("the fixture has no moov");
}

/// The codec config of an MP4 fixture's first track.
fn codec(mp4: &[u8]) -> mp4_atom::Codec {
	moov(mp4).trak.swap_remove(0).mdia.minf.stbl.stsd.codecs.swap_remove(0)
}

/// Prefix each NAL unit with a start code.
fn start_codes(nals: &[&[u8]]) -> Vec<u8> {
	nals.iter().flat_map(|nal| [&[0, 0, 0, 1][..], nal].concat()).collect()
}

/// An H.264 access unit: bbb.mp4's SPS and PPS, then an IDR slice.
fn h264_stream() -> Vec<u8> {
	let mp4_atom::Codec::Avc1(avc1) = codec(include_bytes!("container/fmp4/test_data/bbb.mp4")) else {
		panic!("bbb.mp4 opens with its H.264 track");
	};
	let (sps, pps) = (
		&avc1.avcc.sequence_parameter_sets[0],
		&avc1.avcc.picture_parameter_sets[0],
	);
	start_codes(&[&[0x09, 0xf0], sps, pps, &[0x65, 0x88, 0x84, 0x21]])
}

/// An H.265 access unit: real parameter sets, then an IDR slice.
fn h265_stream() -> Vec<u8> {
	use crate::codec::h265::fixtures::{PPS, SPS, VPS};
	start_codes(&[&[0x46, 0x01, 0x50], VPS, SPS, PPS, &[0x26, 0x01, 0xaf, 0x80]])
}

/// An AV1 temporal unit: a temporal delimiter, av1.mp4's sequence header, and a key frame.
fn av1_stream() -> Vec<u8> {
	let mp4_atom::Codec::Av01(av01) = codec(include_bytes!("container/fmp4/test_data/av1.mp4")) else {
		panic!("av1.mp4 carries AV1");
	};
	[&[0x12, 0x00][..], &av01.av1c.config_obus, &[0x32, 0x02, 0x10, 0x00]].concat()
}

/// A CMAF init segment and a fragment to decode against it.
#[serde_with::serde_as]
#[derive(serde::Deserialize)]
struct CmafFixture {
	#[serde_as(as = "serde_with::base64::Base64")]
	init: Vec<u8>,
	#[serde_as(as = "serde_with::base64::Base64")]
	fragment: Vec<u8>,
}

/// The fragments `js/hang` decodes too, each leaving sample values to a different default.
fn cmaf_fixtures() -> Vec<CmafFixture> {
	#[derive(serde::Deserialize)]
	struct File {
		cases: Vec<CmafFixture>,
	}
	let file: File = serde_json::from_str(include_str!("container/fmp4/test_data/sample-defaults.json"))
		.expect("the fixture parses");
	file.cases
}

#[cfg(test)]
mod tests {
	use super::*;

	fn target(name: &str) -> Target {
		TARGETS
			.iter()
			.find(|(target, _)| *target == name)
			.unwrap_or_else(|| panic!("no such target: {name}"))
			.1
	}

	/// Every seed is accepted within the bound, and every target has one.
	///
	/// A corpus the parser refuses leaves the fuzzer mutating noise against an early `return`.
	#[test]
	fn seeds_are_accepted() {
		let seeds = seeds();
		for (name, _) in TARGETS {
			assert!(seeds.iter().any(|seed| seed.target == *name), "no seed for {name}");
		}
		for (index, seed) in seeds.iter().enumerate() {
			assert!(
				run(target(seed.target), &seed.data),
				"{} seed {index} refused",
				seed.target
			);
		}
	}

	/// Replay the inputs committed under `fuzz/regressions/`, so a bug the fuzzer found stays
	/// fixed on the stable toolchain, in the suite CI already runs.
	#[test]
	fn regressions() {
		let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fuzz/regressions");

		for (name, target) in TARGETS {
			// Nothing has failed yet for this target; `fuzz/README.md` says what to drop in here
			// when something does.
			let Ok(entries) = std::fs::read_dir(root.join(name)) else {
				continue;
			};
			for entry in entries {
				let path = entry.expect("could not read the regression directory").path();
				let data = std::fs::read(&path).expect("could not read a regression input");
				run(*target, &data);
			}
		}
	}
}
