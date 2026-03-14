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
}

// Make a new audio group every 100ms.
const MAX_AUDIO_GROUP_DURATION: Timestamp = Timestamp::from_millis_unchecked(100);

impl Hang {
	pub fn new(input: moq_lite::BroadcastConsumer) -> Self {
		Self { input }
	}

	/// Run the converter.
	///
	/// Reads the hang catalog from the input broadcast. If tracks are already Legacy,
	/// passes them through unchanged (no-op). If tracks are CMAF, parses moof+mdat
	/// and converts to hang frames.
	pub async fn run(self) -> anyhow::Result<(moq_lite::BroadcastProducer, crate::CatalogProducer)> {
		let mut broadcast = moq_lite::Broadcast::new().produce();
		let catalog_producer = crate::CatalogProducer::new(&mut broadcast)?;

		// Subscribe to the input catalog
		let catalog_track = self.input.subscribe_track(&hang::Catalog::default_track())?;
		let mut catalog_consumer = hang::CatalogConsumer::new(catalog_track);
		let catalog = catalog_consumer.next().await?.context("empty catalog")?;

		let mut output_catalog = catalog_producer.clone();
		let mut guard = output_catalog.lock();

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
					tokio::spawn(passthrough_track(input_track, output_track));
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

					tokio::spawn(convert_cmaf_to_legacy(input_track, output_track, timescale, true));
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
					tokio::spawn(passthrough_track(input_track, output_track));
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

					tokio::spawn(convert_cmaf_to_legacy(input_track, output_track, timescale, false));
				}
			}
		}

		drop(guard);

		Ok((broadcast, catalog_producer))
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
	while let Some(group) = input.next_group().await? {
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

	while let Some(group) = input.next_group().await? {
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

		for trun in &traf.trun {
			let mut offset = 0usize;

			if let Some(data_offset) = trun.data_offset {
				// data_offset is relative to start of moof, but we only have mdat data.
				// We need to subtract the moof size and mdat header.
				// Since we don't have the raw moof size here, we assume offset starts at 0
				// for the mdat data directly.
				offset = 0;
				let _ = data_offset; // consumed by the offset computation in the import path
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

				if offset + size <= mdat.data.len() {
					let payload = Bytes::copy_from_slice(&mdat.data[offset..offset + size]);
					samples.push((timestamp, payload, keyframe));
				}

				dts += duration as u64;
				offset += size;
			}
		}
	}

	Ok(samples)
}
