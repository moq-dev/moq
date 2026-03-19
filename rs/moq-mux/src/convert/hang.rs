use anyhow::Context;
use base64::Engine;
use bytes::Bytes;
use hang::catalog::Container;
use hang::container::{Frame, OrderedProducer, Timestamp};
use mp4_atom::DecodeMaybe;

/// Converts a broadcast from any format to hang/Legacy format.
///
/// If tracks are already Legacy, they are passed through unchanged.
/// If tracks are CMAF, parses moof+mdat and converts to hang frames.
pub struct Hang {
	input: moq_lite::BroadcastConsumer,
	output: moq_lite::BroadcastProducer,
}

// Make a new audio group every 100ms.
const MAX_AUDIO_GROUP_DURATION: Timestamp = Timestamp::from_millis_unchecked(100);

impl Hang {
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
				Container::Cmaf { init_data } => {
					let init_bytes = base64::engine::general_purpose::STANDARD
						.decode(init_data)
						.context("invalid base64 init_data")?;

					let timescale = parse_timescale(&init_bytes)?;

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
				Container::Cmaf { init_data } => {
					let init_bytes = base64::engine::general_purpose::STANDARD
						.decode(init_data)
						.context("invalid base64 init_data")?;

					let timescale = parse_timescale(&init_bytes)?;

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
			}
		}

		drop(guard);

		// Keep broadcast and catalog alive until all track tasks complete.
		while tasks.join_next().await.is_some() {}

		Ok(())
	}
}

/// Parse the timescale from an init segment (ftyp+moov).
fn parse_timescale(init_data: &[u8]) -> anyhow::Result<u64> {
	let mut cursor = std::io::Cursor::new(init_data);
	while let Some(atom) = mp4_atom::Any::decode_maybe(&mut cursor)? {
		if let mp4_atom::Any::Moov(moov) = atom {
			let trak = moov.trak.first().context("no tracks in moov")?;
			return Ok(trak.mdia.mdhd.timescale as u64);
		}
	}
	anyhow::bail!("no moov found in init data")
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
			// Parse the moof+mdat fragment
			let samples = extract_samples(&frame_data, timescale)?;

			for (i, (timestamp, payload, keyframe)) in samples.into_iter().enumerate() {
				if is_video && is_first_in_group && i == 0 && keyframe {
					ordered.keyframe()?;
				}

				let frame = Frame {
					timestamp,
					payload: payload.into(),
				};
				ordered.write(frame)?;
			}

			is_first_in_group = false;
		}
	}

	ordered.finish()?;
	Ok(())
}

/// Extract individual samples from a moof+mdat fragment.
fn extract_samples(data: &Bytes, timescale: u64) -> anyhow::Result<Vec<(Timestamp, Bytes, bool)>> {
	let mut cursor = std::io::Cursor::new(data.as_ref());
	let mut moof: Option<mp4_atom::Moof> = None;

	while let Some(atom) = mp4_atom::Any::decode_maybe(&mut cursor)? {
		match atom {
			mp4_atom::Any::Moof(m) => {
				moof = Some(m);
			}
			mp4_atom::Any::Mdat(mdat) => {
				let moof = moof.take().context("mdat without moof")?;
				return extract_from_moof_mdat(&moof, &mdat, timescale);
			}
			_ => {}
		}
	}

	anyhow::bail!("no mdat found in fragment")
}

fn extract_from_moof_mdat(
	moof: &mp4_atom::Moof,
	mdat: &mp4_atom::Mdat,
	timescale: u64,
) -> anyhow::Result<Vec<(Timestamp, Bytes, bool)>> {
	let mut samples = Vec::new();

	for traf in &moof.traf {
		let tfdt = traf.tfdt.as_ref().context("missing tfdt")?;
		let mut dts = tfdt.base_media_decode_time;
		let mut offset = 0usize;

		for trun in &traf.trun {
			if trun.data_offset.is_some() {
				// data_offset is relative to start of moof. Since we converted the
				// fragment ourselves (build_moof_mdat sets data_offset = moof_size + 8),
				// we subtract those to get an offset into mdat.data.
				// For fragments we produce, data_offset points past the mdat header,
				// so the offset into mdat.data is 0 for the first sample.
				// For external fragments we don't have moof_size, so we reset to 0.
				offset = 0;
			}

			for entry in &trun.entries {
				let flags = entry.flags.unwrap_or(traf.tfhd.default_sample_flags.unwrap_or(0));
				let duration = entry.duration.unwrap_or(traf.tfhd.default_sample_duration.unwrap_or(0));
				let size = entry.size.unwrap_or(traf.tfhd.default_sample_size.unwrap_or(0)) as usize;

				let pts = (dts as i64 + entry.cts.unwrap_or_default() as i64) as u64;
				let timestamp = Timestamp::from_scale(pts, timescale)?;

				let keyframe = {
					let depends_on_no_other = (flags >> 24) & 0x3 == 0x2;
					let non_sync = (flags >> 16) & 0x1 == 0x1;
					depends_on_no_other && !non_sync
				};

				anyhow::ensure!(
					offset + size <= mdat.data.len(),
					"sample extends past mdat: offset={offset} size={size} mdat_len={}",
					mdat.data.len()
				);

				let payload = Bytes::copy_from_slice(&mdat.data[offset..offset + size]);
				samples.push((timestamp, payload, keyframe));

				dts += duration as u64;
				offset += size;
			}
		}
	}

	Ok(samples)
}
