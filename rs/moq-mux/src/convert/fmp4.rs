use anyhow::Context;
use base64::Engine;
use bytes::Bytes;
use hang::catalog::{AudioCodec, AudioConfig, Container, VideoCodec, VideoConfig};
use mp4_atom::{Atom, Encode};

/// Converts a broadcast from any format to CMAF format.
///
/// If tracks are already CMAF, they are passed through unchanged.
/// If tracks are hang/Legacy, each frame is individually wrapped in moof+mdat.
pub struct Fmp4 {
	input: moq_lite::BroadcastConsumer,
	output: moq_lite::BroadcastProducer,
}

impl Fmp4 {
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
