//! End-to-end round-trip: configure an [`AudioProducer`], publish a few
//! frames, subscribe via [`AudioConsumer`], assert the decoded signal
//! is non-trivial. Covers moq-net wiring, the resampler path, and a
//! non-default [`AudioFormat`].

use std::time::Duration;

use bytes::Bytes;
use moq_audio::{AudioConsumer, AudioFormat, AudioProducer, Codec, DecoderOutput, EncoderInput, EncoderOutput, Frame};

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

#[tokio::test]
async fn opus_round_trip_48k_stereo() {
	let mut broadcast = moq_net::Broadcast::new().produce();
	let catalog = moq_mux::catalog::Producer::new(&mut broadcast).unwrap();
	let mut catalog_consumer = catalog.consume().unwrap();
	let broadcast_consumer = broadcast.consume();

	let mut producer = AudioProducer::new(
		&mut broadcast,
		catalog.clone(),
		"audio",
		EncoderInput {
			format: AudioFormat::F32,
			sample_rate: 48_000,
			channels: 2,
		},
		EncoderOutput {
			codec: Codec::Opus,
			bitrate: Some(96_000),
			..EncoderOutput::default()
		},
	)
	.unwrap();

	let frames_per_chunk = 48_000 / 50; // 960 frames = 20ms @ 48k
	for _ in 0..10 {
		let pcm = sine_f32_interleaved(440.0, 48_000, 2, frames_per_chunk);
		producer
			.write(&Frame {
				timestamp_us: 0,
				data: f32_bytes(&pcm),
			})
			.unwrap();
	}

	let snapshot = catalog_consumer.next().await.unwrap().expect("catalog should publish");
	let cfg = snapshot.audio.renditions.get("audio").expect("audio rendition");
	let mut consumer = AudioConsumer::new(
		&broadcast_consumer,
		cfg,
		"audio",
		DecoderOutput {
			format: AudioFormat::F32,
			..DecoderOutput::default()
		},
	)
	.unwrap();

	producer.finish().unwrap();

	let mut total_frames = 0u64;
	let mut total_energy = 0.0f64;
	while let Some(frame) = tokio::time::timeout(Duration::from_secs(5), consumer.read())
		.await
		.expect("decoded frame timed out")
		.unwrap()
	{
		let pcm: Vec<f32> = frame
			.data
			.chunks_exact(4)
			.map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
			.collect();
		total_frames += pcm.len() as u64 / 2;
		for s in &pcm {
			total_energy += (*s as f64) * (*s as f64);
		}
	}

	assert!(
		(5_000..=12_000).contains(&total_frames),
		"expected ~9600 frames, got {total_frames}"
	);
	let energy_per_frame = total_energy / total_frames.max(1) as f64;
	assert!(
		energy_per_frame > 0.01,
		"signal should not be silent (got {energy_per_frame:.4})"
	);
}

#[tokio::test]
async fn opus_round_trip_44100_s16_resampled() {
	let mut broadcast = moq_net::Broadcast::new().produce();
	let catalog = moq_mux::catalog::Producer::new(&mut broadcast).unwrap();
	let mut catalog_consumer = catalog.consume().unwrap();
	let broadcast_consumer = broadcast.consume();

	let mut producer = AudioProducer::new(
		&mut broadcast,
		catalog.clone(),
		"audio",
		EncoderInput {
			format: AudioFormat::S16,
			sample_rate: 44_100,
			channels: 1,
		},
		EncoderOutput {
			codec: Codec::Opus,
			bitrate: Some(64_000),
			..EncoderOutput::default()
		},
	)
	.unwrap();

	let frames_per_chunk = 44_100 / 50; // 882 = 20ms @ 44.1k
	for _ in 0..25 {
		let pcm = sine_f32_interleaved(440.0, 44_100, 1, frames_per_chunk);
		let s16 = AudioFormat::S16.from_interleaved_f32(&pcm, 1).unwrap();
		producer
			.write(&Frame {
				timestamp_us: 0,
				data: Bytes::from(s16),
			})
			.unwrap();
	}

	let snapshot = catalog_consumer.next().await.unwrap().unwrap();
	let cfg = snapshot.audio.renditions.get("audio").unwrap();
	// Catalog reports the codec's actual rate (snapped to 48 kHz).
	assert_eq!(cfg.sample_rate, 48_000);
	assert_eq!(cfg.channel_count, 1);

	let mut consumer = AudioConsumer::new(
		&broadcast_consumer,
		cfg,
		"audio",
		DecoderOutput {
			format: AudioFormat::S16,
			sample_rate: Some(44_100),
			channels: Some(1),
			latency_max: Some(Duration::from_millis(500)),
		},
	)
	.unwrap();
	assert_eq!(consumer.sample_rate(), 44_100);
	assert_eq!(consumer.channels(), 1);

	producer.finish().unwrap();

	let mut total_bytes = 0u64;
	while let Some(frame) = tokio::time::timeout(Duration::from_secs(5), consumer.read())
		.await
		.expect("decoded frame timed out")
		.unwrap()
	{
		total_bytes += frame.data.len() as u64;
	}
	assert!(
		total_bytes > 10_000,
		"expected several thousand samples, got {} bytes",
		total_bytes
	);
}
