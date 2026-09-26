//! Container imports that must decode end to end: a stream `moq-mux` imports
//! plays through [`decode::Consumer`] like the same stream from any container.

use std::time::Duration;

use moq_audio::{Format, Layout, decode};

/// MPEG-TS names surround Opus only by channel count, so the importer has to
/// synthesize the OpusHead mapping table or the decoder refuses the track.
#[tokio::test]
async fn ts_surround_opus_decodes() {
	// A 440 Hz center channel in 5.1; see `import_opus_surround_catalog` in moq-mux.
	let data = include_bytes!("../../moq-mux/src/container/ts/test_data/opus_5_1.ts");

	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let broadcast_consumer = broadcast.consume();
	let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
	let mut import = moq_mux::container::ts::Import::new(broadcast, catalog.reserve());
	import.decode(&data[..]).unwrap();
	import.finish().unwrap();

	let snapshot = catalog.snapshot();
	let (name, config) = snapshot.audio.renditions.iter().next().expect("an Opus track");

	let mut options = decode::Options::default();
	options.output.format = Format::F32;
	// The whole file is imported before anything decodes it.
	options.max_age = Duration::from_secs(30);
	let mut consumer = decode::Consumer::new(&broadcast_consumer, config, name, options)
		.await
		.unwrap();
	assert_eq!(consumer.layout(), Layout::FivePointOne);

	// Canonical 5.1 order: FL, FR, FC, LFE, SL, SR.
	let mut energy = [0.0f64; 6];
	let mut frames = 0;
	while let Some(frame) = tokio::time::timeout(Duration::from_secs(5), consumer.read())
		.await
		.expect("decoded frame timed out")
		.unwrap()
	{
		let (samples, rest) = frame.data.as_chunks::<4>();
		assert!(rest.is_empty());
		for chunk in samples.as_chunks::<6>().0 {
			for (channel, sample) in energy.iter_mut().zip(chunk) {
				*channel += f32::from_le_bytes(*sample).powi(2) as f64;
			}
			frames += 1;
		}
	}

	assert!(frames > 12_000, "expected most of 0.5 s at 48 kHz, got {frames} frames");
	let center = energy[2];
	assert!(
		center / frames as f64 > 0.001,
		"center should carry the tone: {energy:?}"
	);
	for (channel, &other) in energy.iter().enumerate().filter(|&(channel, _)| channel != 2) {
		assert!(
			other < center / 100.0,
			"channel {channel} should be near silent: {energy:?}"
		);
	}
}
