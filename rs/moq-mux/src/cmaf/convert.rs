use anyhow::Context;
use base64::Engine;
use bytes::Bytes;
use hang::catalog::{AudioCodec, AudioConfig, Container, VideoCodec, VideoConfig};
use mp4_atom::{Atom, Encode};

/// Converts a broadcast from any format to CMAF format.
///
/// If tracks are already CMAF, they are passed through unchanged.
/// If tracks are hang/Legacy, each frame is individually wrapped in moof+mdat.
pub struct Convert {
	input: moq_lite::BroadcastConsumer,
	output: moq_lite::BroadcastProducer,
}

impl Convert {
	pub fn new(input: moq_lite::BroadcastConsumer, output: moq_lite::BroadcastProducer) -> Self {
		Self { input, output }
	}

	/// Run the converter.
	///
	/// Reads the hang catalog from the input broadcast. If tracks are already CMAF,
	/// passes them through unchanged (no-op). If tracks are hang/Legacy, converts
	/// each frame to moof+mdat.
	pub async fn run(self) -> anyhow::Result<()> {
		let mut broadcast = self.output;
		let catalog_producer = crate::CatalogProducer::new(&mut broadcast)?;

		let catalog_track = self.input.subscribe_track(&hang::Catalog::default_track())?;
		let mut catalog_consumer = hang::CatalogConsumer::new(catalog_track);
		let catalog = catalog_consumer.next().await?.context("empty catalog")?;

		let mut output_catalog = catalog_producer.clone();
		let mut guard = output_catalog.lock();
		let mut tasks = tokio::task::JoinSet::new();

		for (name, config) in &catalog.video.renditions {
			let input_track = self.input.subscribe_track(&moq_lite::Track {
				name: name.clone(),
				priority: 1,
			})?;

			match &config.container {
				Container::Cmaf { .. } => {
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
				Container::Legacy => {
					let init_data = build_video_init(config)?;
					let timescale = guess_video_timescale(config);

					let mut cmaf_config = config.clone();
					cmaf_config.container = Container::Cmaf {
						init_data: base64::engine::general_purpose::STANDARD.encode(&init_data),
					};
					guard.video.renditions.insert(name.clone(), cmaf_config);

					let output_track = broadcast.create_track(moq_lite::Track {
						name: name.clone(),
						priority: 1,
					})?;

					let track_name = name.clone();
					tasks.spawn(async move {
						if let Err(e) = convert_legacy_to_cmaf(input_track, output_track, timescale, true).await {
							tracing::error!(%e, track = %track_name, "convert_legacy_to_cmaf failed");
						}
					});
				}
			}
		}

		for (name, config) in &catalog.audio.renditions {
			let input_track = self.input.subscribe_track(&moq_lite::Track {
				name: name.clone(),
				priority: 2,
			})?;

			match &config.container {
				Container::Cmaf { .. } => {
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
				Container::Legacy => {
					let init_data = build_audio_init(config)?;

					let mut cmaf_config = config.clone();
					cmaf_config.container = Container::Cmaf {
						init_data: base64::engine::general_purpose::STANDARD.encode(&init_data),
					};
					guard.audio.renditions.insert(name.clone(), cmaf_config);

					let output_track = broadcast.create_track(moq_lite::Track {
						name: name.clone(),
						priority: 2,
					})?;

					let timescale = config.sample_rate as u64;
					let track_name = name.clone();
					tasks.spawn(async move {
						if let Err(e) = convert_legacy_to_cmaf(input_track, output_track, timescale, false).await {
							tracing::error!(%e, track = %track_name, "convert_legacy_to_cmaf failed");
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

async fn passthrough_track(
	mut input: moq_lite::TrackConsumer,
	mut output: moq_lite::TrackProducer,
) -> anyhow::Result<()> {
	while let Some(group) = input.recv_group().await? {
		let mut out_group = output.append_group()?;
		let mut reader = group;
		while let Some(data) = reader.read_frame().await? {
			out_group.write_frame(data)?;
		}
		out_group.finish()?;
	}
	output.finish()?;
	Ok(())
}

async fn convert_legacy_to_cmaf(
	input: moq_lite::TrackConsumer,
	mut output: moq_lite::TrackProducer,
	timescale: u64,
	is_video: bool,
) -> anyhow::Result<()> {
	let mut consumer = crate::consumer::OrderedConsumer::new(input, crate::consumer::Legacy, std::time::Duration::MAX);
	let mut seq: u32 = 1;
	let mut current_group: Option<moq_lite::GroupProducer> = None;

	while let Some(frame) = consumer.read().await? {
		let keyframe = frame.is_keyframe();

		if is_video && keyframe {
			if let Some(mut prev) = current_group.take() {
				prev.finish()?;
			}
			current_group = Some(output.append_group()?);
		} else if current_group.is_none() {
			current_group = Some(output.append_group()?);
		}

		let group = current_group.as_mut().unwrap();

		let payload: Vec<u8> = frame.payload.into_iter().flat_map(|chunk| chunk.into_iter()).collect();
		let dts = frame.timestamp.as_micros() as u64 * timescale / 1_000_000;
		let moof_mdat = build_moof_mdat(seq, 1, dts, &payload, keyframe)?;
		seq += 1;

		group.write_frame(moof_mdat)?;
	}

	if let Some(mut g) = current_group.take() {
		g.finish()?;
	}
	output.finish()?;
	Ok(())
}

pub(crate) fn build_moof_mdat(seq: u32, track_id: u32, dts: u64, data: &[u8], keyframe: bool) -> anyhow::Result<Bytes> {
	let flags = if keyframe { 0x0200_0000 } else { 0x0001_0000 };

	// First pass to get moof size (use Some(0) so trun includes the data_offset field)
	let moof = build_moof(seq, track_id, dts, data.len() as u32, flags, Some(0));
	let mut buf = Vec::new();
	moof.encode(&mut buf)?;
	let moof_size = buf.len();

	// Second pass with data_offset
	let data_offset = (moof_size + 8) as i32; // 8 = mdat header
	let moof = build_moof(seq, track_id, dts, data.len() as u32, flags, Some(data_offset));
	buf.clear();
	moof.encode(&mut buf)?;

	let mdat = mp4_atom::Mdat { data: data.to_vec() };
	mdat.encode(&mut buf)?;

	Ok(Bytes::from(buf))
}

fn build_moof(seq: u32, track_id: u32, dts: u64, size: u32, flags: u32, data_offset: Option<i32>) -> mp4_atom::Moof {
	mp4_atom::Moof {
		mfhd: mp4_atom::Mfhd { sequence_number: seq },
		traf: vec![mp4_atom::Traf {
			tfhd: mp4_atom::Tfhd {
				track_id,
				..Default::default()
			},
			tfdt: Some(mp4_atom::Tfdt {
				base_media_decode_time: dts,
			}),
			trun: vec![mp4_atom::Trun {
				data_offset,
				entries: vec![mp4_atom::TrunEntry {
					size: Some(size),
					flags: Some(flags),
					..Default::default()
				}],
			}],
			..Default::default()
		}],
	}
}

pub(crate) fn build_video_init(config: &VideoConfig) -> anyhow::Result<Vec<u8>> {
	let ftyp = mp4_atom::Ftyp {
		major_brand: b"isom".into(),
		minor_version: 0x200,
		compatible_brands: vec![b"isom".into(), b"iso6".into(), b"mp41".into()],
	};

	let codec = build_video_codec(config)?;
	let timescale = guess_video_timescale(config) as u32;

	let moov = mp4_atom::Moov {
		mvhd: mp4_atom::Mvhd {
			timescale,
			..Default::default()
		},
		trak: vec![mp4_atom::Trak {
			tkhd: mp4_atom::Tkhd {
				track_id: 1,
				width: mp4_atom::FixedPoint::new(config.coded_width.unwrap_or(0) as u16, 0),
				height: mp4_atom::FixedPoint::new(config.coded_height.unwrap_or(0) as u16, 0),
				..Default::default()
			},
			mdia: mp4_atom::Mdia {
				mdhd: mp4_atom::Mdhd {
					timescale,
					..Default::default()
				},
				hdlr: mp4_atom::Hdlr {
					handler: b"vide".into(),
					name: "VideoHandler".into(),
				},
				minf: mp4_atom::Minf {
					vmhd: Some(mp4_atom::Vmhd::default()),
					dinf: mp4_atom::Dinf {
						dref: mp4_atom::Dref { urls: vec![] },
					},
					stbl: mp4_atom::Stbl {
						stsd: mp4_atom::Stsd { codecs: vec![codec] },
						..Default::default()
					},
					..Default::default()
				},
			},
			..Default::default()
		}],
		mvex: Some(mp4_atom::Mvex {
			trex: vec![mp4_atom::Trex {
				track_id: 1,
				default_sample_description_index: 1,
				..Default::default()
			}],
			..Default::default()
		}),
		..Default::default()
	};

	let mut buf = Vec::new();
	ftyp.encode(&mut buf)?;
	moov.encode(&mut buf)?;
	Ok(buf)
}

pub(crate) fn build_audio_init(config: &AudioConfig) -> anyhow::Result<Vec<u8>> {
	let ftyp = mp4_atom::Ftyp {
		major_brand: b"isom".into(),
		minor_version: 0x200,
		compatible_brands: vec![b"isom".into(), b"iso6".into(), b"mp41".into()],
	};

	let codec = build_audio_codec(config)?;
	let timescale = config.sample_rate;

	let moov = mp4_atom::Moov {
		mvhd: mp4_atom::Mvhd {
			timescale,
			..Default::default()
		},
		trak: vec![mp4_atom::Trak {
			tkhd: mp4_atom::Tkhd {
				track_id: 1,
				..Default::default()
			},
			mdia: mp4_atom::Mdia {
				mdhd: mp4_atom::Mdhd {
					timescale,
					..Default::default()
				},
				hdlr: mp4_atom::Hdlr {
					handler: b"soun".into(),
					name: "SoundHandler".into(),
				},
				minf: mp4_atom::Minf {
					smhd: Some(mp4_atom::Smhd::default()),
					dinf: mp4_atom::Dinf {
						dref: mp4_atom::Dref { urls: vec![] },
					},
					stbl: mp4_atom::Stbl {
						stsd: mp4_atom::Stsd { codecs: vec![codec] },
						..Default::default()
					},
					..Default::default()
				},
			},
			..Default::default()
		}],
		mvex: Some(mp4_atom::Mvex {
			trex: vec![mp4_atom::Trex {
				track_id: 1,
				default_sample_description_index: 1,
				..Default::default()
			}],
			..Default::default()
		}),
		..Default::default()
	};

	let mut buf = Vec::new();
	ftyp.encode(&mut buf)?;
	moov.encode(&mut buf)?;
	Ok(buf)
}

fn build_video_codec(config: &VideoConfig) -> anyhow::Result<mp4_atom::Codec> {
	let visual = mp4_atom::Visual {
		width: config.coded_width.unwrap_or(0) as u16,
		height: config.coded_height.unwrap_or(0) as u16,
		..Default::default()
	};

	match &config.codec {
		VideoCodec::H264(_) => {
			let mut data = config
				.description
				.as_ref()
				.context("H264 requires description")?
				.clone();
			let avcc = mp4_atom::Avcc::decode_body(&mut data)?;
			Ok(mp4_atom::Codec::Avc1(mp4_atom::Avc1 {
				visual,
				avcc,
				..Default::default()
			}))
		}
		VideoCodec::H265(h265) => {
			let mut data = config
				.description
				.as_ref()
				.context("H265 requires description")?
				.clone();
			let hvcc = mp4_atom::Hvcc::decode_body(&mut data)?;
			if h265.in_band {
				Ok(mp4_atom::Codec::Hev1(mp4_atom::Hev1 {
					visual,
					hvcc,
					..Default::default()
				}))
			} else {
				Ok(mp4_atom::Codec::Hvc1(mp4_atom::Hvc1 {
					visual,
					hvcc,
					..Default::default()
				}))
			}
		}
		VideoCodec::VP9(vp9) => Ok(mp4_atom::Codec::Vp09(mp4_atom::Vp09 {
			visual,
			vpcc: mp4_atom::VpcC {
				profile: vp9.profile,
				level: vp9.level,
				bit_depth: vp9.bit_depth,
				chroma_subsampling: vp9.chroma_subsampling,
				video_full_range_flag: vp9.full_range,
				color_primaries: vp9.color_primaries,
				transfer_characteristics: vp9.transfer_characteristics,
				matrix_coefficients: vp9.matrix_coefficients,
				codec_initialization_data: vec![],
			},
			..Default::default()
		})),
		VideoCodec::AV1(av1) => Ok(mp4_atom::Codec::Av01(mp4_atom::Av01 {
			visual,
			av1c: mp4_atom::Av1c {
				seq_profile: av1.profile,
				seq_level_idx_0: av1.level,
				seq_tier_0: av1.tier == 'H',
				high_bitdepth: av1.bitdepth >= 10,
				twelve_bit: av1.bitdepth >= 12,
				monochrome: av1.mono_chrome,
				chroma_subsampling_x: av1.chroma_subsampling_x,
				chroma_subsampling_y: av1.chroma_subsampling_y,
				chroma_sample_position: av1.chroma_sample_position,
				..Default::default()
			},
			..Default::default()
		})),
		VideoCodec::VP8 => Ok(mp4_atom::Codec::Vp08(mp4_atom::Vp08 {
			visual,
			..Default::default()
		})),
		_ => anyhow::bail!("unsupported video codec for CMAF conversion"),
	}
}

fn build_audio_codec(config: &AudioConfig) -> anyhow::Result<mp4_atom::Codec> {
	let audio = mp4_atom::Audio {
		data_reference_index: 1,
		channel_count: config.channel_count as u16,
		sample_size: 16,
		sample_rate: mp4_atom::FixedPoint::new(config.sample_rate as u16, 0),
	};

	match &config.codec {
		AudioCodec::AAC(aac) => {
			let freq_index: u8 = match config.sample_rate {
				96000 => 0,
				88200 => 1,
				64000 => 2,
				48000 => 3,
				44100 => 4,
				32000 => 5,
				24000 => 6,
				22050 => 7,
				16000 => 8,
				12000 => 9,
				11025 => 10,
				8000 => 11,
				7350 => 12,
				_ => 0xF,
			};

			Ok(mp4_atom::Codec::Mp4a(mp4_atom::Mp4a {
				audio,
				esds: mp4_atom::Esds {
					es_desc: mp4_atom::esds::EsDescriptor {
						es_id: 1,
						dec_config: mp4_atom::esds::DecoderConfig {
							object_type_indication: 0x40,
							stream_type: 5,
							max_bitrate: config.bitrate.unwrap_or(0) as u32,
							avg_bitrate: config.bitrate.unwrap_or(0) as u32,
							dec_specific: mp4_atom::esds::DecoderSpecific {
								profile: aac.profile,
								freq_index,
								chan_conf: config.channel_count as u8,
							},
							..Default::default()
						},
						sl_config: mp4_atom::esds::SLConfig {},
					},
				},
				btrt: None,
				taic: None,
			}))
		}
		AudioCodec::Opus => Ok(mp4_atom::Codec::Opus(mp4_atom::Opus {
			audio,
			dops: mp4_atom::Dops {
				output_channel_count: config.channel_count as u8,
				pre_skip: 0,
				input_sample_rate: config.sample_rate,
				output_gain: 0,
			},
			btrt: None,
		})),
		_ => anyhow::bail!("unsupported audio codec for CMAF conversion"),
	}
}

fn guess_video_timescale(config: &VideoConfig) -> u64 {
	if let Some(fps) = config.framerate {
		(fps * 1000.0) as u64
	} else {
		90000
	}
}

#[cfg(test)]
pub(crate) mod test {
	use std::time::Duration;

	use base64::Engine;
	use bytes::{Bytes, BytesMut};
	use hang::catalog::{Container, H264, VideoCodec, VideoConfig};
	use hang::container::{Frame, Timestamp};
	use mp4_atom::{Atom, DecodeMaybe};

	use buf_list::BufList;

	fn ts(micros: u64) -> Timestamp {
		Timestamp::from_micros(micros).unwrap()
	}

	pub(crate) fn build_avcc_description() -> Bytes {
		let avcc = mp4_atom::Avcc {
			configuration_version: 1,
			avc_profile_indication: 0x64,
			profile_compatibility: 0x00,
			avc_level_indication: 0x1F,
			length_size: 4,
			sequence_parameter_sets: vec![vec![
				0x67, 0x64, 0x00, 0x1F, 0xAC, 0xD9, 0x40, 0x50, 0x05, 0xBB, 0x01, 0x10, 0x00, 0x00, 0x03, 0x00, 0x10,
				0x00, 0x00, 0x03, 0x03, 0xC0, 0xF1, 0x62, 0xE4, 0x80,
			]],
			picture_parameter_sets: vec![vec![0x68, 0xEB, 0xE3, 0xCB, 0x22, 0xC0]],
			..Default::default()
		};

		let mut buf = BytesMut::new();
		avcc.encode_body(&mut buf).expect("encode avcc");
		buf.freeze()
	}

	pub(crate) fn test_video_config() -> VideoConfig {
		VideoConfig {
			codec: VideoCodec::H264(H264 {
				profile: 0x64,
				constraints: 0x00,
				level: 0x1F,
				inline: false,
			}),
			description: Some(build_avcc_description()),
			coded_width: Some(1920),
			coded_height: Some(1080),
			framerate: Some(30.0),
			container: Container::Legacy,
			bitrate: None,
			display_ratio_width: None,
			display_ratio_height: None,
			optimize_for_latency: None,
			jitter: None,
		}
	}

	pub(crate) fn setup_input(
		video_config: &VideoConfig,
	) -> (
		moq_lite::BroadcastConsumer,
		moq_lite::TrackProducer,
		moq_lite::BroadcastProducer,
		moq_lite::TrackProducer,
	) {
		let mut broadcast = moq_lite::Broadcast::new().produce();

		let mut catalog_track = broadcast.create_track(hang::Catalog::default_track()).unwrap();
		let mut catalog = hang::Catalog::default();
		catalog
			.video
			.renditions
			.insert("video".to_string(), video_config.clone());

		let catalog_json = catalog.to_string().unwrap();
		let mut group = catalog_track.append_group().unwrap();
		group.write_frame(catalog_json).unwrap();
		group.finish().unwrap();

		let video_track = broadcast
			.create_track(moq_lite::Track {
				name: "video".to_string(),
				priority: 1,
			})
			.unwrap();

		let consumer = broadcast.consume();

		(consumer, video_track, broadcast, catalog_track)
	}

	pub(crate) fn write_legacy_frames(track: &mut moq_lite::TrackProducer, frames: &[(Timestamp, Vec<u8>, bool)]) {
		let mut current_group: Option<moq_lite::GroupProducer> = None;
		for (timestamp, payload, is_keyframe) in frames {
			if *is_keyframe {
				if let Some(mut g) = current_group.take() {
					g.finish().unwrap();
				}
				current_group = Some(track.append_group().unwrap());
			} else if current_group.is_none() {
				current_group = Some(track.append_group().unwrap());
			}

			let frame = Frame {
				timestamp: *timestamp,
				payload: BufList::from_iter(vec![Bytes::from(payload.clone())]),
			};
			frame.encode(current_group.as_mut().unwrap()).unwrap();
		}

		if let Some(mut g) = current_group.take() {
			g.finish().unwrap();
		}
		track.finish().unwrap();
	}

	pub(crate) fn write_cmaf_frames(track: &mut moq_lite::TrackProducer, frames: &[(u64, Vec<u8>, bool)]) {
		let mut current_group: Option<moq_lite::GroupProducer> = None;
		let mut seq: u32 = 1;
		for (dts, payload, keyframe) in frames {
			if *keyframe {
				if let Some(mut g) = current_group.take() {
					g.finish().unwrap();
				}
				current_group = Some(track.append_group().unwrap());
			} else if current_group.is_none() {
				current_group = Some(track.append_group().unwrap());
			}

			let moof_mdat = super::build_moof_mdat(seq, 1, *dts, payload, *keyframe).unwrap();
			seq += 1;
			current_group.as_mut().unwrap().write_frame(moof_mdat).unwrap();
		}

		if let Some(mut g) = current_group.take() {
			g.finish().unwrap();
		}
		track.finish().unwrap();
	}

	pub(crate) async fn read_legacy_frames(track: moq_lite::TrackConsumer) -> Vec<(Timestamp, Vec<u8>, bool)> {
		let mut ordered = crate::consumer::OrderedConsumer::new(track, crate::consumer::Legacy, Duration::MAX);

		let mut result = Vec::new();
		while let Some(frame) = tokio::time::timeout(Duration::from_millis(500), ordered.read())
			.await
			.expect("read_legacy_frames timed out")
			.expect("read_legacy_frames error")
		{
			let is_keyframe = frame.is_keyframe();
			let timestamp = frame.timestamp;
			let payload: Vec<u8> = frame.payload.into_iter().flat_map(|c| c.into_iter()).collect();
			result.push((timestamp, payload, is_keyframe));
		}
		result
	}

	pub(crate) async fn read_cmaf_raw_frames(mut track: moq_lite::TrackConsumer) -> Vec<Bytes> {
		let mut result = Vec::new();
		while let Some(group) = tokio::time::timeout(Duration::from_millis(500), track.recv_group())
			.await
			.expect("read_cmaf_raw_frames timed out")
			.expect("read_cmaf_raw_frames error")
		{
			let mut reader = group;
			while let Some(data) = tokio::time::timeout(Duration::from_millis(500), reader.read_frame())
				.await
				.expect("read_cmaf_raw_frames timed out on frame")
				.expect("read_cmaf_raw_frames frame error")
			{
				result.push(data);
			}
		}
		result
	}

	fn parse_cmaf_frame(data: &Bytes, timescale: u64) -> (Timestamp, Vec<u8>, bool) {
		let mut cursor = std::io::Cursor::new(data.as_ref());
		let mut moof_found = None;
		let mut mdat_found = None;

		while let Some(atom) = mp4_atom::Any::decode_maybe(&mut cursor).unwrap() {
			match atom {
				mp4_atom::Any::Moof(m) => moof_found = Some(m),
				mp4_atom::Any::Mdat(m) => mdat_found = Some(m),
				_ => {}
			}
		}

		let moof = moof_found.expect("no moof");
		let mdat = mdat_found.expect("no mdat");
		let traf = &moof.traf[0];
		let tfdt = traf.tfdt.as_ref().expect("no tfdt");
		let timestamp = Timestamp::from_scale(tfdt.base_media_decode_time, timescale).unwrap();
		let flags = traf.trun[0].entries[0].flags.unwrap_or(0);
		let keyframe = (flags >> 24) & 0x3 == 0x2 && (flags >> 16) & 0x1 == 0;

		(timestamp, mdat.data.clone(), keyframe)
	}

	pub(crate) async fn subscribe_video(consumer: &moq_lite::BroadcastConsumer) -> moq_lite::TrackConsumer {
		let track = moq_lite::Track {
			name: "video".to_string(),
			priority: 1,
		};
		loop {
			match consumer.subscribe_track(&track) {
				Ok(t) => return t,
				Err(_) => tokio::task::yield_now().await,
			}
		}
	}

	#[tokio::test]
	async fn legacy_to_cmaf_video() {
		let config = test_video_config();
		let frames = vec![
			(ts(0), vec![0x01, 0x02, 0x03], true),
			(ts(33_000), vec![0x04, 0x05], false),
			(ts(66_000), vec![0x06, 0x07, 0x08], true),
		];

		let (consumer, mut video_track, _broadcast, _catalog_track) = setup_input(&config);
		let output = moq_lite::Broadcast::new().produce();
		let output_consumer = output.consume();

		let converter = super::Convert::new(consumer, output);

		let frames_clone = frames.clone();
		tokio::spawn(async move {
			tokio::task::yield_now().await;
			write_legacy_frames(&mut video_track, &frames_clone);
		});

		let (convert_result, cmaf_frames) = tokio::join!(converter.run(), async {
			let output_video = subscribe_video(&output_consumer).await;
			read_cmaf_raw_frames(output_video).await
		});
		convert_result.unwrap();

		let timescale = config.framerate.map(|f| (f * 1000.0) as u64).unwrap();
		assert_eq!(cmaf_frames.len(), 3, "expected 3 CMAF frames");

		for (i, cmaf_data) in cmaf_frames.iter().enumerate() {
			let (parsed_ts, payload, keyframe) = parse_cmaf_frame(cmaf_data, timescale);
			assert_eq!(parsed_ts, frames[i].0, "timestamp mismatch at frame {i}");
			assert_eq!(payload, frames[i].1, "payload mismatch at frame {i}");
			assert_eq!(keyframe, frames[i].2, "keyframe flag mismatch at frame {i}");
		}
	}

	#[tokio::test]
	async fn cmaf_passthrough() {
		let config = test_video_config();
		let init_data = super::build_video_init(&config).unwrap();
		let timescale = config.framerate.map(|f| (f * 1000.0) as u64).unwrap();

		let cmaf_frames: Vec<(u64, Vec<u8>, bool)> = vec![
			(0, vec![0x01, 0x02], true),
			(33_000u64 * timescale / 1_000_000, vec![0x03, 0x04], false),
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

		let (convert_result, output_frames) = tokio::join!(converter.run(), async {
			let output_video = subscribe_video(&output_consumer).await;
			read_cmaf_raw_frames(output_video).await
		});
		convert_result.unwrap();

		assert_eq!(output_frames.len(), cmaf_frames.len());

		let mut seq = 1u32;
		for (i, (dts, payload, keyframe)) in cmaf_frames.iter().enumerate() {
			let expected = super::build_moof_mdat(seq, 1, *dts, payload, *keyframe).unwrap();
			seq += 1;
			assert_eq!(output_frames[i], expected, "frame {i} should be byte-identical");
		}
	}

	#[tokio::test]
	async fn roundtrip_legacy_cmaf_legacy() {
		let config = test_video_config();
		let frames = vec![
			(ts(0), vec![0xAA, 0xBB], true),
			(ts(33_000), vec![0xCC], false),
			(ts(66_000), vec![0xDD, 0xEE, 0xFF], true),
			(ts(99_000), vec![0x11, 0x22], false),
		];

		let (consumer, mut video_track, _broadcast, _catalog_track) = setup_input(&config);

		// Legacy → CMAF
		let cmaf_output = moq_lite::Broadcast::new().produce();
		let cmaf_consumer = cmaf_output.consume();
		let fmp4_converter = super::Convert::new(consumer, cmaf_output);

		// CMAF → Legacy
		let legacy_output = moq_lite::Broadcast::new().produce();
		let legacy_consumer = legacy_output.consume();
		let hang_converter = crate::hang::Convert::new(cmaf_consumer, legacy_output);

		let frames_clone = frames.clone();
		tokio::spawn(async move {
			tokio::task::yield_now().await;
			write_legacy_frames(&mut video_track, &frames_clone);
		});

		let (r1, r2, result) = tokio::join!(fmp4_converter.run(), hang_converter.run(), async {
			let legacy_video = subscribe_video(&legacy_consumer).await;
			read_legacy_frames(legacy_video).await
		});
		r1.unwrap();
		r2.unwrap();

		assert_eq!(result.len(), frames.len(), "frame count mismatch after roundtrip");

		for (i, (expected_ts, expected_payload, _)) in frames.iter().enumerate() {
			assert_eq!(result[i].0, *expected_ts, "timestamp mismatch at frame {i}");
			assert_eq!(result[i].1, *expected_payload, "payload mismatch at frame {i}");
		}
	}
}
