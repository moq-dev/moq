//! Committed CMAF and MPEG-TS inputs decoded independently by FFmpeg's native AAC and Opus decoders.
//!
//! `test_data/README.md` records generation, reference alignment, and measured tolerances.
//! This lives under `backend` so every compiled candidate joins without exporting a test API.

use std::time::Duration;

use super::{Backend, PLATFORM, SOFTWARE};
use crate::decode;

#[derive(Clone, Copy)]
struct Errors {
	rms: f64,
	peak: f64,
}

// Each bound is the next binary power above the largest measured reference
// difference, including the consumer's preroll-trimmed result. See test_data/README.md.
#[cfg(feature = "aac")]
const AAC: Errors = Errors {
	rms: 1.0 / (1u64 << 24) as f64,
	peak: 1.0 / (1u64 << 20) as f64,
};
const OPUS: Errors = Errors {
	rms: 1.0 / (1u64 << 26) as f64,
	peak: 1.0 / (1u64 << 23) as f64,
};
const SURROUND: Errors = Errors {
	rms: 1.0 / (1u64 << 13) as f64,
	peak: 1.0 / (1u64 << 8) as f64,
};

#[derive(Clone, Copy)]
enum Container {
	Cmaf(&'static [u8]),
	Ts(&'static [u8]),
}

struct Vector {
	name: &'static str,
	container: Container,
	reference: &'static [u8],
	rate: u32,
	channels: u32,
	errors: Errors,
}

const VECTORS: &[Vector] = &[
	#[cfg(feature = "aac")]
	Vector {
		name: "aac-speech",
		container: Container::Cmaf(include_bytes!("test_data/aac-speech.mp4")),
		reference: include_bytes!("test_data/aac-speech.f32"),
		rate: 44_100,
		channels: 1,
		errors: AAC,
	},
	#[cfg(feature = "aac")]
	Vector {
		name: "aac-44100-stereo",
		container: Container::Cmaf(include_bytes!("test_data/aac-44100-stereo.mp4")),
		reference: include_bytes!("test_data/aac-44100-stereo.f32"),
		rate: 44_100,
		channels: 2,
		errors: AAC,
	},
	#[cfg(feature = "aac")]
	Vector {
		name: "aac-48000-stereo",
		container: Container::Cmaf(include_bytes!("test_data/aac-48000-stereo.mp4")),
		reference: include_bytes!("test_data/aac-48000-stereo.f32"),
		rate: 48_000,
		channels: 2,
		errors: AAC,
	},
	Vector {
		name: "opus-speech",
		container: Container::Cmaf(include_bytes!("test_data/opus-speech.mp4")),
		reference: include_bytes!("test_data/opus-speech.f32"),
		rate: 48_000,
		channels: 1,
		errors: OPUS,
	},
	Vector {
		name: "opus-surround",
		container: Container::Ts(include_bytes!("test_data/opus-surround.ts")),
		reference: include_bytes!("test_data/opus-surround.f32"),
		rate: 48_000,
		channels: 6,
		errors: SURROUND,
	},
];

fn samples(bytes: &[u8]) -> Vec<f32> {
	let (words, tail) = bytes.as_chunks::<4>();
	assert!(tail.is_empty());
	words.iter().map(|word| f32::from_le_bytes(*word)).collect()
}

fn compare(vector: &Vector, name: &str, got: &[f32]) {
	let reference = samples(vector.reference);
	assert_eq!(got.len(), reference.len(), "{} {name}: sample count", vector.name);
	let mut square = 0.0f64;
	let mut peak = 0.0f64;
	let mut signal = 0.0f64;
	for (&got, &want) in got.iter().zip(&reference) {
		assert!(got.is_finite(), "{} {name}: nonfinite sample", vector.name);
		let error = (got as f64 - want as f64).abs();
		square += error * error;
		peak = peak.max(error);
		signal += (want as f64).powi(2);
	}
	let rms = (square / got.len() as f64).sqrt();
	assert!(
		rms <= vector.errors.rms,
		"{} {name}: RMS error {rms} exceeds {}",
		vector.name,
		vector.errors.rms
	);
	assert!(
		peak <= vector.errors.peak,
		"{} {name}: peak error {peak} exceeds {}",
		vector.name,
		vector.errors.peak
	);
	eprintln!(
		"{} {name}: frames={} error_rms={} error_peak={} reference_rms={}",
		vector.name,
		got.len() / vector.channels as usize,
		(square / got.len() as f64).sqrt(),
		peak,
		(signal / got.len() as f64).sqrt()
	);
}

fn import(
	vector: &Vector,
) -> (
	Box<dyn std::any::Any>,
	moq_net::broadcast::Consumer,
	String,
	hang::catalog::AudioConfig,
) {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
	// The importer owns the finished tracks until both consumers have read them.
	let source: Box<dyn std::any::Any> = match vector.container {
		Container::Ts(bytes) => {
			let mut importer = moq_mux::container::ts::Import::new(broadcast, catalog.reserve());
			importer.decode(bytes).unwrap();
			importer.finish().unwrap();
			Box::new(importer)
		}
		Container::Cmaf(bytes) => {
			let mut importer = moq_mux::container::fmp4::Import::new(broadcast, catalog.reserve());
			importer.decode(bytes).unwrap();
			importer.finish().unwrap();
			Box::new(importer)
		}
	};
	let snapshot = catalog.snapshot();
	let (name, config) = snapshot.audio.renditions.iter().next().expect("fixture audio");
	(source, consumer, name.clone(), config.clone())
}

/// Independent references pin frequency, level, speaker order, packet history, and pre-skip.
#[tokio::test(start_paused = true)]
async fn every_backend_matches_independent_reference() {
	for vector in VECTORS {
		let (_source, broadcast, name, config) = import(vector);
		let track = broadcast.track(&name).unwrap();
		let track = track
			.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(1)))
			.await
			.unwrap();
		let wire = moq_mux::catalog::hang::Container::try_from(&config).unwrap();
		let mut encoded = moq_mux::container::Consumer::new(track, wire);
		let mut packets = Vec::new();
		while let Some(read) = encoded.read().await.unwrap() {
			match read {
				moq_mux::container::Read::Frame { frame, .. } => packets.push(frame.payload),
				moq_mux::container::Read::End { .. } => {}
				moq_mux::container::Read::Gap { .. } => panic!("fixture has no gap"),
			}
		}
		let mut backends: Vec<Box<dyn Backend>> = Vec::new();
		for candidate in SOFTWARE.iter().chain(PLATFORM) {
			if (candidate.supports)(&config.codec) {
				backends.push((candidate.open)(&config).unwrap());
			}
		}
		assert!(!backends.is_empty(), "{}: no decoder tested", vector.name);
		for mut decoder in backends {
			assert_eq!(decoder.sample_rate(), vector.rate);
			assert_eq!(decoder.layout().channels(), vector.channels);
			let delay = decoder.delay() * vector.channels as usize;

			let mut pcm = Vec::new();
			for packet in &packets {
				pcm.extend(decoder.decode(packet).unwrap().samples);
			}
			compare(vector, decoder.name(), &pcm[delay..]);
		}
	}
}

/// The consumer stamps decoded samples, including codec delay and every CMAF sample boundary.
#[tokio::test(start_paused = true)]
async fn consumer_preserves_reference_samples_and_timeline() {
	for vector in VECTORS {
		let (_source, broadcast, name, config) = import(vector);
		let options = decode::Options {
			max_delay: Duration::from_secs(1),
			..Default::default()
		};
		let mut decoder = decode::Consumer::new(&broadcast, &config, name, options).await.unwrap();
		let mut pcm = Vec::new();
		let mut first = None;
		while let Some(read) = decoder.read().await.unwrap() {
			assert!(
				!matches!(read, decode::Read::Gap { .. }),
				"{}: unexpected gap",
				vector.name
			);
			if let decode::Read::Frame(frame) = read {
				let start = *first.get_or_insert(frame.timestamp);
				let elapsed = Duration::from_secs_f64(pcm.len() as f64 / vector.channels as f64 / vector.rate as f64);
				let actual = Duration::from(frame.timestamp)
					.checked_sub(Duration::from(start))
					.expect("monotonic PCM");
				assert!(
					actual.abs_diff(elapsed) <= Duration::from_micros(1),
					"{}: {:?} versus {:?}",
					vector.name,
					actual,
					elapsed
				);
				pcm.extend(samples(&frame.data));
			}
		}
		// Decoded from the track's first packet, so no warmup is trimmed.
		compare(vector, "consumer", &pcm);
	}
}
