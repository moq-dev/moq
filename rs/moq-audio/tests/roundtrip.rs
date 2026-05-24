//! End-to-end round-trip: publish raw PCM via [`AudioProducer`], pull it
//! back via [`AudioConsumer`], and assert decoded output is non-trivial.
//! Covers the moq-net wiring (broadcast → catalog → track), the
//! resampler path, and a non-default `AudioFormat`.

use bytes::Bytes;
use moq_audio::{AudioConsumer, AudioFormat, AudioProducer, AudioSamples};

fn sine_f32_interleaved(freq: f32, sample_rate: u32, channels: u32, frames: usize) -> Vec<f32> {
	let mut out = Vec::with_capacity(frames * channels as usize);
	for i in 0..frames {
		let t = i as f32 / sample_rate as f32;
		let v = (2.0 * std::f32::consts::PI * freq * t).sin() * 0.5;
		for _ in 0..channels {
			out.push(v);
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

/// Publish a sine through Opus, subscribe to it, decode it, assert energy survives.
#[tokio::test]
async fn opus_round_trip_48k_stereo() {
	let mut broadcast = moq_net::Broadcast::new().produce();
	let catalog = moq_mux::catalog::hang::Producer::new(&mut broadcast).unwrap();
	let mut catalog_consumer = catalog.consume().unwrap();
	let broadcast_consumer = broadcast.consume();

	let mut producer =
		AudioProducer::new_opus(&mut broadcast, catalog.clone(), "audio", 48_000, 2, Some(96_000)).unwrap();

	// Push ~200 ms of audio in 20 ms chunks. First write registers the
	// audio rendition in the catalog.
	let frames_per_chunk = 48_000 / 50; // 960
	for _ in 0..10 {
		let pcm = sine_f32_interleaved(440.0, 48_000, 2, frames_per_chunk);
		producer
			.write(&AudioSamples {
				format: AudioFormat::F32,
				sample_rate: 48_000,
				channel_count: 2,
				timestamp_us: 0, // producer assigns based on frames produced
				data: f32_bytes(&pcm),
			})
			.unwrap();
	}

	// Snapshot the catalog *before* finish — finish drops the producer,
	// which removes the rendition. Subscribe to audio while we're at it.
	let snapshot = catalog_consumer
		.next()
		.await
		.unwrap()
		.expect("catalog should publish at least once");
	let audio_cfg = snapshot.audio.renditions.get("audio").expect("audio rendition");
	let mut consumer =
		AudioConsumer::subscribe_opus(&broadcast_consumer, audio_cfg, "audio", AudioFormat::F32, None, None).unwrap();

	producer.finish().unwrap();

	let mut total_frames = 0u64;
	let mut total_energy = 0.0f64;
	while let Some(samples) = consumer.read().await.unwrap() {
		let pcm: Vec<f32> = samples
			.data
			.chunks_exact(4)
			.map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
			.collect();
		assert_eq!(samples.sample_rate, 48_000);
		assert_eq!(samples.channel_count, 2);
		total_frames += pcm.len() as u64 / 2;
		for s in &pcm {
			total_energy += (*s as f64) * (*s as f64);
		}
	}

	// We pushed 10 * 960 = 9600 frames per channel. The moq-mux consumer
	// has startup/skip logic so allow a wide window.
	assert!(
		(5_000..=12_000).contains(&total_frames),
		"expected ~9600 frames decoded, got {total_frames}"
	);

	let energy_per_frame = total_energy / total_frames.max(1) as f64;
	// Input sine has amplitude 0.5 → expected energy ~0.125 per sample.
	// Allow generous tolerance: Opus is lossy and startup samples warm up.
	assert!(
		energy_per_frame > 0.01,
		"decoded signal should not be silent (got energy {energy_per_frame:.4})"
	);
}

/// Caller feeds 44.1 kHz S16, producer resamples to 48 kHz for Opus,
/// consumer resamples back down to 44.1 kHz S16.
#[tokio::test]
async fn opus_round_trip_44100_s16_resampled() {
	let mut broadcast = moq_net::Broadcast::new().produce();
	let catalog = moq_mux::catalog::hang::Producer::new(&mut broadcast).unwrap();
	let mut catalog_consumer = catalog.consume().unwrap();
	let broadcast_consumer = broadcast.consume();

	let mut producer =
		AudioProducer::new_opus(&mut broadcast, catalog.clone(), "audio", 44_100, 1, Some(64_000)).unwrap();

	// ~500 ms of audio: the resampler primes over the first few chunks.
	let frames_per_chunk = 44_100 / 50; // 882
	for _ in 0..25 {
		let pcm = sine_f32_interleaved(440.0, 44_100, 1, frames_per_chunk);
		let s16 = AudioFormat::S16.from_interleaved_f32(&pcm, 1).unwrap();
		producer
			.write(&AudioSamples {
				format: AudioFormat::S16,
				sample_rate: 44_100,
				channel_count: 1,
				timestamp_us: 0,
				data: Bytes::from(s16),
			})
			.unwrap();
	}

	let snapshot = catalog_consumer.next().await.unwrap().unwrap();
	let audio_cfg = snapshot.audio.renditions.get("audio").unwrap();
	// Catalog reports what the codec actually got, post-resample: 48 kHz/1 ch.
	assert_eq!(audio_cfg.sample_rate, 48_000);
	assert_eq!(audio_cfg.channel_count, 1);

	let mut consumer = AudioConsumer::subscribe_opus(
		&broadcast_consumer,
		audio_cfg,
		"audio",
		AudioFormat::S16,
		Some(44_100),
		Some(1),
	)
	.unwrap();
	assert_eq!(consumer.output_rate(), 44_100);
	assert_eq!(consumer.output_channels(), 1);

	producer.finish().unwrap();

	let mut total_frames = 0u64;
	while let Some(samples) = consumer.read().await.unwrap() {
		assert_eq!(samples.format, AudioFormat::S16);
		assert_eq!(samples.sample_rate, 44_100);
		assert_eq!(samples.channel_count, 1);
		total_frames += samples.frame_count() as u64;
	}

	// ~22k input frames → expect at least a few thousand survive the
	// resample boundaries on either end.
	assert!(
		total_frames > 5_000,
		"expected several thousand frames after round-trip, got {total_frames}"
	);
}
