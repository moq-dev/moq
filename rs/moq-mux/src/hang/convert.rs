use anyhow::Context;
#[cfg(feature = "mp4")]
use base64::Engine;
use hang::catalog::Container;
use hang::container::{Frame, OrderedProducer, Timestamp};

/// Converts a broadcast from any format to hang/Legacy format.
///
/// If tracks are already Legacy, they are passed through unchanged.
/// If tracks are CMAF, parses moof+mdat and converts to hang frames.
pub struct Convert {
	input: moq_lite::BroadcastConsumer,
	output: moq_lite::BroadcastProducer,
}

// Make a new audio group every 100ms.
const MAX_AUDIO_GROUP_DURATION: Timestamp = Timestamp::from_millis_unchecked(100);

impl Convert {
	pub fn new(input: moq_lite::BroadcastConsumer, output: moq_lite::BroadcastProducer) -> Self {
		Self { input, output }
	}

	/// Run the converter.
	///
	/// Reads the hang catalog from the input broadcast. If tracks are already Legacy,
	/// passes them through unchanged (no-op). If tracks are CMAF, parses moof+mdat
	/// and converts to hang frames.
	pub async fn run(self) -> anyhow::Result<()> {
		let mut broadcast = self.output;
		let catalog_producer = crate::CatalogProducer::new(&mut broadcast)?;

		// Subscribe to the input catalog
		let catalog_track = self.input.subscribe_track(&hang::Catalog::default_track())?;
		let mut catalog_consumer = hang::CatalogConsumer::new(catalog_track);
		let catalog = catalog_consumer.next().await?.context("empty catalog")?;

		let mut output_catalog = catalog_producer.clone();
		let mut guard = output_catalog.lock();
		let mut tasks: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();

		// Convert video tracks
		for (name, config) in &catalog.video.renditions {
			let input_track = self.input.subscribe_track(&moq_lite::Track {
				name: name.clone(),
				priority: 1,
			})?;

			match &config.container {
				Container::Legacy => {
					// Already Legacy — pass through
					guard.video.renditions.insert(name.clone(), config.clone());
					let output_track = broadcast.create_track(moq_lite::Track {
						name: name.clone(),
						priority: 1,
					})?;
					let track_name = name.clone();
					tasks.spawn(async move {
						if let Err(e) = passthrough_track(input_track, output_track).await {
							tracing::error!(%e, track = %track_name, "passthrough_track failed");
						}
					});
				}
				#[cfg(feature = "mp4")]
				Container::Cmaf { init_data } => {
					let init_bytes = base64::engine::general_purpose::STANDARD
						.decode(init_data)
						.context("invalid base64 init_data")?;

					let timescale = crate::cmaf::parse_timescale(&init_bytes)?;

					let mut legacy_config = config.clone();
					legacy_config.container = Container::Legacy;
					guard.video.renditions.insert(name.clone(), legacy_config);

					let output_track = broadcast.create_track(moq_lite::Track {
						name: name.clone(),
						priority: 1,
					})?;

					let track_name = name.clone();
					tasks.spawn(async move {
						if let Err(e) = convert_cmaf_to_legacy(input_track, output_track, timescale, true).await {
							tracing::error!(%e, track = %track_name, "convert_cmaf_to_legacy failed");
						}
					});
				}
				#[cfg(not(feature = "mp4"))]
				_ => anyhow::bail!("CMAF container requires the 'mp4' feature"),
			}
		}

		// Convert audio tracks
		for (name, config) in &catalog.audio.renditions {
			let input_track = self.input.subscribe_track(&moq_lite::Track {
				name: name.clone(),
				priority: 2,
			})?;

			match &config.container {
				Container::Legacy => {
					guard.audio.renditions.insert(name.clone(), config.clone());
					let output_track = broadcast.create_track(moq_lite::Track {
						name: name.clone(),
						priority: 2,
					})?;
					let track_name = name.clone();
					tasks.spawn(async move {
						if let Err(e) = passthrough_track(input_track, output_track).await {
							tracing::error!(%e, track = %track_name, "passthrough_track failed");
						}
					});
				}
				#[cfg(feature = "mp4")]
				Container::Cmaf { init_data } => {
					let init_bytes = base64::engine::general_purpose::STANDARD
						.decode(init_data)
						.context("invalid base64 init_data")?;

					let timescale = crate::cmaf::parse_timescale(&init_bytes)?;

					let mut legacy_config = config.clone();
					legacy_config.container = Container::Legacy;
					guard.audio.renditions.insert(name.clone(), legacy_config);

					let output_track = broadcast.create_track(moq_lite::Track {
						name: name.clone(),
						priority: 2,
					})?;

					let track_name = name.clone();
					tasks.spawn(async move {
						if let Err(e) = convert_cmaf_to_legacy(input_track, output_track, timescale, false).await {
							tracing::error!(%e, track = %track_name, "convert_cmaf_to_legacy failed");
						}
					});
				}
				#[cfg(not(feature = "mp4"))]
				_ => anyhow::bail!("CMAF container requires the 'mp4' feature"),
			}
		}

		drop(guard);

		// Keep broadcast and catalog alive until all track tasks complete.
		while tasks.join_next().await.is_some() {}

		Ok(())
	}
}

/// Pass a track through unchanged.
async fn passthrough_track(
	mut input: moq_lite::TrackConsumer,
	mut output: moq_lite::TrackProducer,
) -> anyhow::Result<()> {
	while let Some(group) = input.recv_group().await? {
		let mut out_group = output.append_group()?;
		let mut frame_reader = group;
		while let Some(frame_data) = frame_reader.read_frame().await? {
			out_group.write_frame(frame_data)?;
		}
		out_group.finish()?;
	}
	output.finish()?;
	Ok(())
}

/// Convert CMAF moof+mdat frames to hang Legacy frames.
#[cfg(feature = "mp4")]
async fn convert_cmaf_to_legacy(
	mut input: moq_lite::TrackConsumer,
	output: moq_lite::TrackProducer,
	timescale: u64,
	is_video: bool,
) -> anyhow::Result<()> {
	let mut ordered = OrderedProducer::new(output);

	if !is_video {
		ordered = ordered.with_max_group_duration(MAX_AUDIO_GROUP_DURATION);
	}

	while let Some(group) = input.recv_group().await? {
		let mut frame_reader = group;
		let mut is_first_in_group = true;

		while let Some(frame_data) = frame_reader.read_frame().await? {
			let frames = crate::cmaf::decode(frame_data, timescale)?;

			for (i, frame) in frames.into_iter().enumerate() {
				if is_video && is_first_in_group && i == 0 && frame.keyframe {
					ordered.keyframe()?;
				}

				ordered.write(Frame {
					timestamp: frame.timestamp,
					payload: frame.payload.into(),
				})?;
			}

			is_first_in_group = false;
		}
	}

	ordered.finish()?;
	Ok(())
}

#[cfg(test)]
mod test {
	use hang::container::Timestamp;

	use crate::cmaf::test::*;

	fn ts(micros: u64) -> Timestamp {
		Timestamp::from_micros(micros).unwrap()
	}

	#[cfg(feature = "mp4")]
	#[tokio::test]
	async fn cmaf_to_legacy_video() {
		use base64::Engine;
		use hang::catalog::Container;

		let config = test_video_config();
		let init_data = crate::cmaf::build_video_init(&config).unwrap();
		let timescale = config.framerate.map(|f| (f * 1000.0) as u64).unwrap();

		let cmaf_frames: Vec<(u64, Vec<u8>, bool)> = vec![
			(0, vec![0x01, 0x02, 0x03], true),
			(33_000u64 * timescale / 1_000_000, vec![0x04, 0x05], false),
			(66_000u64 * timescale / 1_000_000, vec![0x06, 0x07, 0x08], true),
		];

		let mut cmaf_config = config.clone();
		cmaf_config.container = Container::Cmaf {
			init_data: base64::engine::general_purpose::STANDARD.encode(&init_data),
		};

		let (consumer, mut video_track, _broadcast, _catalog_track) = setup_input(&cmaf_config);
		let output = moq_lite::Broadcast::new().produce();
		let output_consumer = output.consume();
		let converter = super::Convert::new(consumer, output);

		let cmaf_frames_clone = cmaf_frames.clone();
		tokio::spawn(async move {
			tokio::task::yield_now().await;
			write_cmaf_frames(&mut video_track, &cmaf_frames_clone);
		});

		let (convert_result, legacy_frames) = tokio::join!(converter.run(), async {
			let output_video = subscribe_video(&output_consumer).await;
			read_legacy_frames(output_video).await
		});
		convert_result.unwrap();

		assert_eq!(legacy_frames.len(), 3, "expected 3 Legacy frames");
		assert_eq!(legacy_frames[0].0, ts(0));
		assert_eq!(legacy_frames[0].1, vec![0x01, 0x02, 0x03]);
		assert_eq!(legacy_frames[1].0, ts(33_000));
		assert_eq!(legacy_frames[1].1, vec![0x04, 0x05]);
		assert_eq!(legacy_frames[2].0, ts(66_000));
		assert_eq!(legacy_frames[2].1, vec![0x06, 0x07, 0x08]);
	}

	#[tokio::test]
	async fn legacy_passthrough() {
		let config = test_video_config();
		let frames = vec![(ts(0), vec![0xAA, 0xBB], true), (ts(33_000), vec![0xCC, 0xDD], false)];

		let (consumer, mut video_track, _broadcast, _catalog_track) = setup_input(&config);
		let output = moq_lite::Broadcast::new().produce();
		let output_consumer = output.consume();

		let converter = super::Convert::new(consumer, output);

		let frames_clone = frames.clone();
		tokio::spawn(async move {
			tokio::task::yield_now().await;
			write_legacy_frames(&mut video_track, &frames_clone);
		});

		let (convert_result, result) = tokio::join!(converter.run(), async {
			let output_video = subscribe_video(&output_consumer).await;
			read_legacy_frames(output_video).await
		});
		convert_result.expect("converter.run() failed");

		assert_eq!(result.len(), frames.len());

		for (i, (expected_ts, expected_payload, _)) in frames.iter().enumerate() {
			assert_eq!(result[i].0, *expected_ts, "timestamp mismatch at frame {i}");
			assert_eq!(result[i].1, *expected_payload, "payload mismatch at frame {i}");
		}
	}
}
