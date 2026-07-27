//! Device round trip for [`moq_audio::playback`].
//!
//! Ignored by default: these open the real output device, which CI runners
//! don't have (and which would make noise if they did). Run them on a machine
//! with speakers via `cargo test -p moq-audio --features playback -- --ignored`.
//! Nothing here asserts on what you hear, so muting the system volume is fine:
//! the mix is metered before it reaches the device.

#![cfg(feature = "playback")]

use std::time::Duration;

use moq_audio::playback;

const RATE: u32 = 48_000;
const CHANNELS: u32 = 2;

/// A second of 440 Hz at half scale, as interleaved `f32` bytes.
fn tone(seconds: f32) -> Vec<u8> {
	let frames = (RATE as f32 * seconds) as usize;
	let mut samples = Vec::with_capacity(frames * CHANNELS as usize * 4);
	for frame in 0..frames {
		let value = (std::f32::consts::TAU * 440.0 * frame as f32 / RATE as f32).sin() * 0.5;
		for _ in 0..CHANNELS {
			samples.extend_from_slice(&value.to_le_bytes());
		}
	}
	samples
}

fn input() -> playback::Input {
	playback::Input {
		sample_rate: RATE,
		channels: CHANNELS,
		..Default::default()
	}
}

/// Open `$MOQ_AUDIO_DEVICE`, the system default, or the first device that will
/// have us.
///
/// A developer machine's default is not always openable from a test binary: a
/// sandboxed or differently-linked build may not reach the sound server that
/// backs it, while the hardware devices underneath it open fine. Falling back
/// keeps these tests about our code rather than about the box's audio routing,
/// and the environment variable pins a specific device when you want to hear
/// the result rather than only meter it.
async fn open() -> playback::Engine {
	if let Ok(id) = std::env::var("MOQ_AUDIO_DEVICE") {
		let mut config = playback::Config::default();
		config.device = Some(id);
		return playback::Engine::open(config).await.expect("MOQ_AUDIO_DEVICE");
	}

	if let Ok(engine) = playback::Engine::open(playback::Config::default()).await {
		return engine;
	}

	for device in playback::devices().await.expect("device enumeration") {
		let mut config = playback::Config::default();
		config.device = Some(device.id.clone());
		if let Ok(engine) = playback::Engine::open(config).await {
			eprintln!("default device unusable, playing through {}", device.name);
			return engine;
		}
	}

	panic!("no usable output device");
}

/// The whole path: open the default device, play a tone, and confirm the mixer
/// saw it and the device drained it.
#[tokio::test]
#[ignore]
async fn plays_a_tone_through_the_default_device() {
	let engine = open().await;
	let mut sink = engine.sink(input()).expect("a sink");

	// Write in chunks, as a decoder would, so the buffer stays bounded instead
	// of overflowing the channel in one go.
	let tone = tone(0.1);
	let mut peaked = false;
	let mut drained = false;

	for _ in 0..10 {
		sink.write(&tone).expect("write");
		tokio::time::sleep(Duration::from_millis(100)).await;

		peaked |= sink.peak() > 0.4;
		// The device is consuming if the queue stays near the target latency
		// rather than growing by a full chunk every write.
		drained |= sink.buffered() < Duration::from_millis(250);
	}

	assert!(peaked, "the mixer never saw the tone");
	assert!(drained, "the device never drained the queue");
}

/// Two sinks on one device, which is the case a call with several participants
/// hits and the reason `Engine` and `Sink` are separate types.
#[tokio::test]
#[ignore]
async fn mixes_two_sinks_and_honors_volume() {
	let engine = open().await;

	let mut loud = engine.sink(input()).expect("a sink");
	let mut quiet = engine.sink(input()).expect("a second sink");
	quiet.set_volume(0.0);

	let tone = tone(0.1);
	let mut loud_peak: f32 = 0.0;
	let mut quiet_peak: f32 = 0.0;

	for _ in 0..10 {
		loud.write(&tone).expect("write");
		quiet.write(&tone).expect("write");
		tokio::time::sleep(Duration::from_millis(100)).await;
		loud_peak = loud_peak.max(loud.peak());
		quiet_peak = quiet_peak.max(quiet.peak());
	}

	assert!(loud_peak > 0.4, "the audible sink never registered: {loud_peak}");
	// Not exactly zero: the ramp to silence spans the first few milliseconds.
	assert!(quiet_peak < 0.1, "the muted sink was still audible: {quiet_peak}");
}

/// Every device the machine reports should be openable by the id it hands back,
/// and switching between them must not disturb a sink that is already playing.
#[tokio::test]
#[ignore]
async fn switches_devices_without_dropping_sinks() {
	let devices = playback::devices().await.expect("device enumeration");
	assert!(!devices.is_empty(), "no output devices");
	assert!(devices.iter().any(|d| d.default), "no default output device");

	let engine = open().await;
	let mut sink = engine.sink(input()).expect("a sink");

	let tone = tone(0.1);
	let mut switched = 0;

	for device in devices {
		if engine.switch(Some(device.id.clone())).await.is_err() {
			// Devices can be listed but unopenable (exclusive use, a virtual
			// node with no backing hardware). Enumeration is not a promise.
			continue;
		}

		switched += 1;
		sink.write(&tone).expect("write");
		tokio::time::sleep(Duration::from_millis(200)).await;
		assert!(
			sink.buffered() < Duration::from_secs(1),
			"{} did not drain after the switch",
			device.name
		);
	}

	assert!(switched > 1, "only {switched} device(s) opened, nothing was switched");
}

/// The driver thread belongs to the sinks as much as the engine, so playback
/// survives dropping the handle it was created from.
#[tokio::test]
#[ignore]
async fn sink_outlives_its_engine() {
	let mut sink = {
		let engine = open().await;
		engine.sink(input()).expect("a sink")
	};

	let tone = tone(0.1);
	let mut peaked = false;
	for _ in 0..5 {
		sink.write(&tone).expect("write");
		tokio::time::sleep(Duration::from_millis(100)).await;
		peaked |= sink.peak() > 0.4;
	}

	assert!(peaked, "playback stopped when the engine was dropped");
}
