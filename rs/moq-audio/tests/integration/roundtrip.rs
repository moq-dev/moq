//! End-to-end round-trip: configure an [`encode::Producer`], publish a few
//! frames, subscribe via [`decode::Consumer`], and check the signal and timeline. Covers moq-net wiring, the resampler path, and a non-default
//! [`Format`].

use std::time::Duration;

use bytes::Bytes;
use moq_audio::{Format, Frame, Layout, decode, encode};
use moq_net::Timestamp;

fn sine_f32_interleaved(freq: f32, sample_rate: u32, channels: u32, frames: usize) -> Vec<f32> {
	let mut out = Vec::with_capacity(frames * channels as usize);
	for i in 0..frames {
		let t = i as f32 / sample_rate as f32;
		for channel in 0..channels {
			let frequency = freq * (channel + 1) as f32 * if i >= frames / 2 { 2.0 } else { 1.0 };
			out.push((std::f32::consts::TAU * frequency * t).sin() * 0.5);
		}
	}
	out
}

fn f32_bytes(samples: &[f32]) -> Bytes {
	let mut out = Vec::with_capacity(samples.len() * 4);
	for s in samples {
		out.extend_from_slice(&s.to_le_bytes());
	}
	Bytes::from(out)
}

struct Case {
	rate: u32,
	layout: Layout,
	format: Format,
	chunks: usize,
}

#[tokio::test(start_paused = true)]
async fn opus_round_trip_48k_stereo() {
	opus_roundtrip(Case {
		rate: 48_000,
		layout: Layout::Stereo,
		format: Format::F32,
		chunks: 10,
	})
	.await;
}

#[tokio::test(start_paused = true)]
async fn opus_round_trip_44100_s16_resampled() {
	opus_roundtrip(Case {
		rate: 44_100,
		layout: Layout::Mono,
		format: Format::S16,
		chunks: 25,
	})
	.await;
}

async fn opus_roundtrip(case: Case) {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
	let mut snapshots = catalog.consume().unwrap();
	let source = broadcast.consume();
	let mut input = encode::Input::new(case.rate, case.layout);
	input.format = case.format;
	let mut options = encode::Options::default();
	options.track = Some("audio".into());
	options.settings = encode::Settings::new(48_000, case.layout);
	options.settings.codec = encode::Codec::Opus;
	let mut producer = encode::Producer::new(&mut broadcast, catalog.clone(), input, &options).unwrap();
	let channels = case.layout.channels();
	let chunk_frames = case.rate as usize / 50;
	let expected_frames = chunk_frames * case.chunks;
	let samples = sine_f32_interleaved(440.0, case.rate, channels, expected_frames);
	for (index, pcm) in samples.chunks_exact(chunk_frames * channels as usize).enumerate() {
		producer
			.write(&Frame::new(
				Bytes::from(case.format.from_interleaved_f32(pcm, channels).unwrap()),
				Timestamp::from_micros(123_000 + index as u64 * 20_000).unwrap(),
			))
			.unwrap();
	}
	let snapshot = snapshots.next().await.unwrap().unwrap();
	let rendition = &snapshot.audio.renditions["audio"];
	assert_eq!(rendition.sample_rate, 48_000);
	assert_eq!(rendition.channel_count, channels);
	let mut options = decode::Options::default();
	options.output.format = case.format;
	options.output.sample_rate = Some(case.rate);
	options.output.layout = Some(case.layout);
	// The whole clip is queued before subscription; retain every group.
	options.max_delay = Duration::from_secs(30);
	let mut consumer = decode::Consumer::new(&source, rendition, "audio", options)
		.await
		.unwrap();
	producer.finish().unwrap();
	let warmup = rendition.warmup.unwrap_or_default();
	let skipped = (warmup.as_nanos() * case.rate as u128 / 1_000_000_000) as usize;
	let mut pcm = Vec::new();
	while let Some(frame) = next_frame(&mut consumer).await.unwrap() {
		let expected =
			123_000 + warmup.as_micros() + pcm.len() as u128 / channels as u128 * 1_000_000 / case.rate as u128;
		// Microsecond stamping rounds each chunk independently.
		assert!(
			frame.timestamp.as_micros().abs_diff(expected) <= 1,
			"{:?} versus {expected}us",
			frame.timestamp
		);
		pcm.extend_from_slice(&case.format.as_interleaved_f32(&frame.data, channels).unwrap());
	}
	assert_eq!(
		pcm.len(),
		(expected_frames - skipped) * channels as usize,
		"pre-skip, resampling and final padding preserve duration"
	);
	for step in 0..2 {
		let start = (step * expected_frames / 2).max(skipped) - skipped;
		let end = (step + 1) * expected_frames / 2 - skipped;
		let half = &pcm[start * channels as usize..end * channels as usize];
		for channel in 0..channels as usize {
			let samples: Vec<f32> = half.iter().skip(channel).step_by(channels as usize).copied().collect();
			let wanted = 440.0 * (channel + 1) as f64 * (step + 1) as f64;
			let loudest = [440.0, 880.0, 1760.0]
				.into_iter()
				.max_by(|&a, &b| power(&samples, a, case.rate).total_cmp(&power(&samples, b, case.rate)))
				.unwrap();
			assert_eq!(loudest, wanted, "step {step}, channel {channel}");
		}
	}
}

fn power(samples: &[f32], frequency: f64, rate: u32) -> f64 {
	let coefficient = 2.0 * (std::f64::consts::TAU * frequency / rate as f64).cos();
	let (mut previous, mut older) = (0.0, 0.0);
	for &sample in samples {
		let value = sample as f64 + coefficient * previous - older;
		older = previous;
		previous = value;
	}
	previous * previous + older * older - coefficient * previous * older
}

#[tokio::test]
async fn pcm_round_trip_is_lossless() {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
	let mut catalog_consumer = catalog.consume().unwrap();
	let broadcast_consumer = broadcast.consume();

	let input = encode::Input::new(48_000, Layout::Stereo);
	let mut options = encode::Options::default();
	options.track = Some("pcm".to_string());
	options.settings.codec = encode::Codec::Pcm;

	let mut producer = encode::Producer::new(&mut broadcast, catalog.clone(), input, &options).unwrap();
	let samples = sine_f32_interleaved(440.0, 48_000, 2, 960);
	producer
		.write(&Frame::new(
			f32_bytes(&samples),
			Timestamp::from_micros(123_000).unwrap(),
		))
		.unwrap();

	let snapshot = catalog_consumer.next().await.unwrap().unwrap();
	let catalog = snapshot.audio.renditions.get("pcm").unwrap();
	assert_eq!(catalog.codec, hang::catalog::AudioCodec::Pcm);
	assert_eq!(catalog.bitrate, Some(3_072_000));

	let mut consumer = decode::Consumer::new(&broadcast_consumer, catalog, "pcm", decode::Options::default())
		.await
		.unwrap();
	producer.finish().unwrap();

	let frame = next_frame(&mut consumer).await.unwrap().unwrap();
	assert_eq!(frame.timestamp, Timestamp::from_micros(123_000).unwrap());
	let decoded: Vec<f32> = frame
		.data
		.as_chunks::<4>()
		.0
		.iter()
		.map(|sample| f32::from_le_bytes([sample[0], sample[1], sample[2], sample[3]]))
		.collect();
	assert_eq!(decoded, samples);
	assert!(next_frame(&mut consumer).await.unwrap().is_none());
}

/// A 5.1 broadcast carries only its channel count in the catalog, which reads
/// back as 5.1 in canonical order and remixes to whatever the subscriber asks
/// for.
#[tokio::test]
async fn pcm_five_one_reads_back_at_any_layout() {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
	let mut catalog_consumer = catalog.consume().unwrap();
	let broadcast_consumer = broadcast.consume();

	let input = encode::Input::new(48_000, Layout::FivePointOne);
	let mut options = encode::Options::default();
	options.track = Some("pcm".to_string());
	options.settings = encode::Settings::new(48_000, Layout::FivePointOne);
	options.settings.codec = encode::Codec::Pcm;

	// A level per speaker: left, right, center, LFE, side left, side right.
	let speakers = [0.1f32, 0.2, 0.3, 0.4, 0.05, 0.06];
	let mut producer = encode::Producer::new(&mut broadcast, catalog.clone(), input, &options).unwrap();
	producer
		.write(&Frame::new(
			f32_bytes(&speakers.repeat(960)),
			Timestamp::from_micros(0).unwrap(),
		))
		.unwrap();

	let snapshot = catalog_consumer.next().await.unwrap().unwrap();
	let rendition = snapshot.audio.renditions.get("pcm").unwrap();
	assert_eq!(rendition.channel_count, 6);

	let h = std::f32::consts::FRAC_1_SQRT_2;
	let left = 0.1 + h * 0.3 + h * 0.05;
	let right = 0.2 + h * 0.3 + h * 0.06;
	let cases = [
		(None, Layout::FivePointOne, speakers.to_vec()),
		(Some(Layout::Stereo), Layout::Stereo, vec![left, right]),
		(Some(Layout::Mono), Layout::Mono, vec![(left + right) * 0.5]),
	];

	let mut consumers = Vec::new();
	for (output, _, _) in &cases {
		let mut options = decode::Options::default();
		options.output.layout = *output;
		consumers.push(
			decode::Consumer::new(&broadcast_consumer, rendition, "pcm", options)
				.await
				.unwrap(),
		);
	}
	producer.finish().unwrap();

	for (mut consumer, (_, layout, frame)) in consumers.into_iter().zip(cases) {
		assert_eq!(consumer.layout(), layout);
		let decoded = next_frame(&mut consumer).await.unwrap().unwrap();
		let samples: Vec<f32> = decoded
			.data
			.as_chunks::<4>()
			.0
			.iter()
			.map(|sample| f32::from_le_bytes([sample[0], sample[1], sample[2], sample[3]]))
			.collect();
		assert_eq!(samples.len(), 960 * frame.len(), "{layout:?}");
		for (got, want) in samples.iter().zip(frame.iter().cycle()) {
			assert!((got - want).abs() < 1e-6, "{layout:?}: got {got}, want {want}");
		}
	}
}

async fn next_frame(consumer: &mut decode::Consumer) -> Result<Option<moq_audio::Frame>, moq_audio::Error> {
	loop {
		match consumer.read().await? {
			Some(decode::Read::Frame(frame)) => return Ok(Some(frame)),
			Some(decode::Read::Gap { .. } | decode::Read::End { .. }) => {}
			None => return Ok(None),
		}
	}
}
