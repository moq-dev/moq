//! H.264 conformance decode: bitstreams nothing in this crate produced.
//!
//! Every other decode test here encodes its own input first, so an encoder and a
//! decoder that share a misunderstanding agree with each other and pass. These
//! feed committed libx264 streams to whichever backends open on the host and
//! check the pictures against a reference decode, which no backend in this crate
//! had a hand in.
//!
//! The fixtures in `test_data/` are progressive 8-bit 4:2:0, IPPP (no B-frames,
//! so decode order is presentation order) at 30fps, and were produced with
//! ffmpeg's libx264:
//!
//! ```text
//! gen() { ffmpeg -f lavfi -i "$1" -frames:v "$2" -c:v libx264 \
//!     -profile:v "$3" -bf 0 -g "$4" -pix_fmt yuv420p -f h264 "$5"; }
//! gen "color=c=blue:s=64x64:r=30"   2 baseline 1 idr_64x64_blue_2f.h264
//! gen "testsrc2=s=64x64:r=30"       5 baseline 4 seq_64x64_pattern_5f.h264
//! gen "color=c=yellow:s=64x64:r=30" 5 main     4 main_64x64_yellow_5f.h264
//! gen "testsrc2=s=100x66:r=30"      5 baseline 4 seq_100x66_pattern_5f.h264
//!
//! for f in seq_64x64_pattern_5f seq_100x66_pattern_5f; do
//!     ffmpeg -i $f.h264 -f rawvideo -pix_fmt yuv420p $f.yuv
//! done
//! ```
//!
//! | Fixture | Profile | Pictures | Size | What it is for |
//! |---|---|---|---|---|
//! | `idr_64x64_blue_2f.h264` | Constrained Baseline | 2, both IDR | 64x64 | Intra only, and a saturated color a channel swap or bias cannot survive |
//! | `seq_64x64_pattern_5f.h264` | Constrained Baseline | 5 (I P P P I) | 64x64 | Inter prediction: the picture has to be updated, not repeated |
//! | `main_64x64_yellow_5f.h264` | Main | 5 (I P P P I) | 64x64 | CABAC rather than CAVLC entropy coding |
//! | `seq_100x66_pattern_5f.h264` | Constrained Baseline | 5 (I P P P I) | 100x66 | Non-square and not macroblock-aligned, so the SPS crops 112x80 down and a decoder that ignores that returns the wrong picture |
//!
//! Every fixture ends on an IDR, which is what keeps the picture counts
//! assertable. [`Backend`] has no drain, and a decoder that bumps its DPB only
//! when the buffer fills rather than at the reorder depth the VUI declares (the
//! VA-API backend does this, and holds `num_ref_frames` pictures for it) would
//! otherwise still be sitting on most of a short stream when it ended. A closing
//! IDR flushes everything before it, so at most the final picture is ever
//! outstanding.
//!
//! Committed rather than generated at test time on purpose. The whole set is
//! under 90 KB, `rs/moq-mux` already carries fMP4 fixtures the same way, and a
//! suite that shells out to ffmpeg either fails or silently skips wherever
//! ffmpeg is absent, which is the failure mode this is meant to close.
//!
//! The two solid-color fixtures need no reference file: every sample of every
//! plane is one known value, asserted exactly. The two pattern fixtures carry
//! the reference decode as raw I420 next to the bitstream. Both comparisons are
//! exact, because 8-bit H.264 decoding is normatively bit-exact: two conforming
//! decoders given the same stream produce the same samples, so a difference is a
//! defect rather than a tolerance to widen.

use bytes::Bytes;
use moq_mux::codec::annexb;
use moq_net::Timestamp;

use super::backend::{self, Backend, Codec};
use super::{Config, Kind};
use crate::Frame;
use crate::frame::I420;

/// The interval between fixture pictures: 30fps, the rate they were generated at.
const FRAME_MICROS: u64 = 33_333;

/// What a conforming decoder has to produce from a fixture's pictures.
enum Expect {
	/// A flat picture: every Y sample is `y`, every U `u`, every V `v`.
	Solid { y: u8, u: u8, v: u8 },
	/// ffmpeg's decode of the same bitstream, raw I420 pictures back to back.
	Reference(&'static [u8]),
}

/// One committed bitstream and what it has to decode to.
struct Vector {
	/// The fixture file name, so a failure names the stream.
	name: &'static str,
	/// The Annex-B elementary stream.
	bitstream: &'static [u8],
	width: u32,
	height: u32,
	/// How many pictures the stream codes.
	pictures: usize,
	expect: Expect,
}

const IDR_BLUE: Vector = Vector {
	name: "idr_64x64_blue_2f.h264",
	bitstream: include_bytes!("test_data/idr_64x64_blue_2f.h264"),
	width: 64,
	height: 64,
	pictures: 2,
	// BT.601 limited-range blue. U at 240 and V at 110 are far enough from
	// neutral that a swapped or shifted chroma plane cannot look plausible.
	expect: Expect::Solid { y: 41, u: 240, v: 110 },
};

const SEQ_PATTERN: Vector = Vector {
	name: "seq_64x64_pattern_5f.h264",
	bitstream: include_bytes!("test_data/seq_64x64_pattern_5f.h264"),
	width: 64,
	height: 64,
	pictures: 5,
	expect: Expect::Reference(include_bytes!("test_data/seq_64x64_pattern_5f.yuv")),
};

const MAIN_YELLOW: Vector = Vector {
	name: "main_64x64_yellow_5f.h264",
	bitstream: include_bytes!("test_data/main_64x64_yellow_5f.h264"),
	width: 64,
	height: 64,
	pictures: 5,
	// BT.601 limited-range yellow: bright luma, chroma pulled hard the other way
	// from the blue fixture's, so a stuck plane fails one of the two.
	expect: Expect::Solid { y: 210, u: 16, v: 146 },
};

const NON_SQUARE: Vector = Vector {
	name: "seq_100x66_pattern_5f.h264",
	bitstream: include_bytes!("test_data/seq_100x66_pattern_5f.h264"),
	width: 100,
	height: 66,
	pictures: 5,
	expect: Expect::Reference(include_bytes!("test_data/seq_100x66_pattern_5f.yuv")),
};

const VECTORS: &[&Vector] = &[&IDR_BLUE, &SEQ_PATTERN, &MAIN_YELLOW, &NON_SQUARE];

/// Every H.264 decode backend this build could contain, under the name
/// [`Kind::Named`] selects it by. Platform-gated to what is compiled in; whether
/// one is actually usable is settled at runtime by [`decoders`].
const CANDIDATES: &[&str] = &[
	"openh264",
	#[cfg(target_os = "macos")]
	"videotoolbox",
	#[cfg(target_os = "windows")]
	"mediafoundation",
	#[cfg(target_os = "android")]
	"mediacodec",
	#[cfg(all(target_os = "linux", feature = "nvidia"))]
	"nvdec",
	#[cfg(all(target_os = "linux", feature = "vaapi"))]
	"vaapi",
	#[cfg(all(target_os = "linux", feature = "v4l2"))]
	"v4l2",
];

/// Open `name`, or `None` where the host cannot: no driver, no render node, no
/// hardware. Same self-skip as the VAAPI round trip rather than `#[ignore]`, so
/// these run wherever the hardware exists and are silent everywhere else.
fn open(name: &str) -> Option<Box<dyn Backend>> {
	let config = Config {
		kind: Kind::Named(name.to_owned()),
		..Config::new()
	};
	backend::open(Codec::H264, &config).ok()
}

/// The backends that opened on this host.
///
/// The software fallback is compiled in unconditionally and needs nothing from
/// the host, so it is always here; asserting that keeps a suite that self-skips
/// per backend from quietly self-skipping altogether.
fn decoders() -> Vec<&'static str> {
	let open: Vec<&'static str> = CANDIDATES.iter().copied().filter(|name| open(name).is_some()).collect();
	assert!(open.contains(&"openh264"), "the software H.264 decoder would not open");
	open
}

/// The NAL unit type, or 0 (unspecified, and neither a slice nor a parameter
/// set) for an empty NAL.
fn nal_type(nal: &Bytes) -> u8 {
	nal.first().map_or(0, |b| b & 0x1f)
}

/// One coded picture, framed the way [`Backend::decode`] wants it.
struct AccessUnit {
	payload: Bytes,
	timestamp: Timestamp,
	keyframe: bool,
}

/// Split an Annex-B elementary stream into one access unit per coded picture.
///
/// A slice NAL (1 non-IDR, 5 IDR) opens a new access unit unless the one being
/// built has no slice yet, so the parameter sets and SEI ahead of a picture
/// travel with it. That is enough for these fixtures: single-slice pictures, no
/// redundant slices, no access unit delimiters.
fn access_units(vector: &Vector) -> Vec<AccessUnit> {
	let mut buf = Bytes::from_static(vector.bitstream);
	let mut iter = annexb::NalIterator::new(&mut buf);
	let mut nals: Vec<Bytes> = iter
		.by_ref()
		.map(|nal| nal.expect("fixture is valid Annex-B"))
		.collect();
	// The iterator stops at the last NAL, which no start code follows.
	nals.extend(iter.flush().expect("fixture is valid Annex-B"));

	let is_slice = |nal: &Bytes| matches!(nal_type(nal), 1 | 5);
	let mut units: Vec<AccessUnit> = Vec::new();
	let mut current: Vec<Bytes> = Vec::new();

	for nal in nals {
		if is_slice(&nal) && current.iter().any(is_slice) {
			units.push(access_unit(&current, units.len()));
			current.clear();
		}
		current.push(nal);
	}
	if current.iter().any(is_slice) {
		units.push(access_unit(&current, units.len()));
	}

	assert_eq!(
		units.len(),
		vector.pictures,
		"{} splits into {} access units, expected {}",
		vector.name,
		units.len(),
		vector.pictures
	);
	units
}

fn access_unit(nals: &[Bytes], index: usize) -> AccessUnit {
	AccessUnit {
		payload: annexb::build_prefix(nals.iter()),
		timestamp: Timestamp::from_micros(index as u64 * FRAME_MICROS).expect("fixture timestamp"),
		keyframe: nals.iter().any(|nal| nal_type(nal) == 5),
	}
}

/// Decode a whole fixture with one backend, in stream order.
fn decode(name: &str, vector: &Vector) -> Vec<Frame> {
	let mut decoder = open(name).expect("backend opened during probing");
	let mut decoded = Vec::new();
	for unit in access_units(vector) {
		let frames = decoder
			.decode(unit.payload, unit.timestamp, unit.keyframe)
			.unwrap_or_else(|e| panic!("{name} failed to decode {}: {e}", vector.name));
		decoded.extend(frames);
	}
	decoded
}

/// The decoded pictures as tightly-packed I420, with the size each one declares
/// checked against the fixture on the way through.
fn pictures(name: &str, vector: &Vector) -> Vec<I420> {
	let decoded = decode(name, vector);

	assert!(!decoded.is_empty(), "{name} decoded no pictures from {}", vector.name);
	// A decoder whose DPB releases a picture only once a later one needs its slot
	// (VA-API) still holds the last one when the stream ends, and the `Backend`
	// trait has no drain. Every fixture codes at least two pictures, so at least
	// one is always checked.
	assert!(
		decoded.len() <= vector.pictures && vector.pictures - decoded.len() <= 1,
		"{name} returned {} pictures from {}, which codes {}",
		decoded.len(),
		vector.name,
		vector.pictures
	);

	decoded
		.into_iter()
		.enumerate()
		.map(|(i, frame)| {
			// IPPP with no reordering, so pictures come back in the order they were
			// fed, each carrying the timestamp its access unit went in with.
			assert_eq!(
				frame.timestamp.as_micros(),
				i as u128 * FRAME_MICROS as u128,
				"{name}: {} picture {i} lost its timestamp",
				vector.name
			);
			let i420 = frame
				.surface
				.to_i420()
				.unwrap_or_else(|e| panic!("{name}: {} picture {i} would not download: {e}", vector.name))
				.into_owned();
			assert_eq!(
				(i420.width(), i420.height()),
				(vector.width, vector.height),
				"{name}: {} picture {i} is the wrong size",
				vector.name
			);
			i420
		})
		.collect()
}

/// Mean absolute error and signed mean bias between two equal-length planes.
///
/// The bias is what catches a decoder that produces a plausible-looking picture
/// with one channel shifted: the error stays small everywhere while the whole
/// plane sits above or below where it should.
fn plane_diff(a: &[u8], b: &[u8]) -> (f64, f64) {
	assert_eq!(a.len(), b.len());
	let n = a.len() as f64;
	let mae = a.iter().zip(b).map(|(x, y)| x.abs_diff(*y) as u64).sum::<u64>() as f64 / n;
	let bias = (a.iter().map(|&x| x as i64).sum::<i64>() - b.iter().map(|&x| x as i64).sum::<i64>()) as f64 / n;
	(mae, bias)
}

/// A one-line per-plane account of how `got` differs from `want`, so a failure
/// says which channel moved and by how much rather than that two buffers are
/// unequal.
fn describe(got: &I420, want: &I420) -> String {
	["Y", "U", "V"]
		.into_iter()
		.zip([(got.y(), want.y()), (got.u(), want.u()), (got.v(), want.v())])
		.map(|(label, (a, b))| {
			let (mae, bias) = plane_diff(a, b);
			format!("{label}: mae {mae:.2}, bias {bias:+.2}")
		})
		.collect::<Vec<_>>()
		.join("; ")
}

/// Check every picture a fixture decodes to against what it has to be, for every
/// backend the host can open.
fn check(vector: &Vector) {
	for name in decoders() {
		for (i, got) in pictures(name, vector).into_iter().enumerate() {
			match vector.expect {
				Expect::Solid { y, u, v } => {
					for (label, plane, value) in [("Y", got.y(), y), ("U", got.u(), u), ("V", got.v(), v)] {
						if let Some((at, &sample)) = plane.iter().enumerate().find(|&(_, &b)| b != value) {
							panic!(
								"{name}: {} picture {i} {label} sample {at} is {sample}, expected {value}",
								vector.name
							);
						}
					}
				}
				Expect::Reference(reference) => {
					let len = I420::len(vector.width, vector.height);
					assert_eq!(
						reference.len(),
						len * vector.pictures,
						"the reference decode for {} is not {} pictures of {}x{}",
						vector.name,
						vector.pictures,
						vector.width,
						vector.height
					);
					let want = I420::new(vector.width, vector.height, reference[i * len..(i + 1) * len].to_vec())
						.expect("reference picture");
					assert!(
						got.data() == want.data(),
						"{name}: {} picture {i} differs from the reference decode ({})",
						vector.name,
						describe(&got, &want)
					);
				}
			}
		}
	}
}

/// Intra-only decode: two IDR pictures, each with its own parameter sets, and a
/// saturated color that fixes both chroma planes.
#[test]
fn idr_only_baseline() {
	check(&IDR_BLUE);
}

/// Inter prediction: three P pictures between the opening and closing IDR, each
/// moving the content, so a decoder that stops applying residuals after the
/// keyframe diverges from the reference.
#[test]
fn multi_frame_baseline() {
	check(&SEQ_PATTERN);
}

/// Main profile, which is CABAC rather than the Baseline fixtures' CAVLC: a
/// different entropy decoder reaching the same pixels.
#[test]
fn main_profile_cabac() {
	check(&MAIN_YELLOW);
}

/// 100x66 codes as 112x80 macroblocks with the SPS cropping the remainder away.
/// A decoder that hands back the coded size, or crops from the wrong edge, fails
/// on the size or on the pixels.
#[test]
fn non_square_cropped() {
	check(&NON_SQUARE);
}

/// Every available backend decoding the same streams has to reach the same
/// samples: 8-bit H.264 decode is normatively exact, so two conforming decoders
/// cannot legitimately disagree. This needs no reference and no encoder, only a
/// second implementation, which is what makes it able to catch a backend whose
/// output is plausible but shifted.
///
/// Self-skips on a host with one usable decoder, which is every CI runner today
/// and any box without a GPU.
#[test]
fn backends_agree() {
	let backends = decoders();
	if backends.len() < 2 {
		return;
	}
	let (anchor, others) = backends.split_first().expect("at least two backends");

	for vector in VECTORS {
		let reference = pictures(anchor, vector);
		for name in others {
			for (i, (got, want)) in pictures(name, vector).iter().zip(&reference).enumerate() {
				assert!(
					got.data() == want.data(),
					"{name} and {anchor} disagree on {} picture {i} ({})",
					vector.name,
					describe(got, want)
				);
			}
		}
	}
}
