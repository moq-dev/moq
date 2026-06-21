use clap::Subcommand;
use hang::moq_net;
use moq_mux::container::{flv, fmp4, hls, ts};

#[derive(Subcommand, Clone)]
pub enum PublishFormat {
	Avc3,
	Fmp4,
	/// MPEG-TS (transport stream) read from stdin.
	Ts,
	/// FLV (Flash Video / RTMP) read from stdin.
	Flv,
	// NOTE: No aac support because it needs framing.
	Hls {
		/// URL or file path of an HLS playlist to ingest.
		#[arg(long)]
		playlist: String,
	},
}

enum PublishDecoder {
	Avc3(Box<moq_mux::codec::h264::Import>),
	Fmp4(Box<fmp4::Import>),
	// TS carries undecoded elementary streams (SCTE-35, teletext, DVB AC-3, ...)
	// verbatim, so it uses the `mpegts` catalog extension rather than the media-only `()`.
	Ts(Box<ts::Import<ts::catalog::Ext>>),
	Flv(Box<flv::Import>),
	Hls(Box<hls::Import>),
}

impl PublishDecoder {
	/// Decode a chunk of bytes from stdin (Avc3, Fmp4, Ts, or Flv).
	fn decode_buf(&mut self, buffer: &mut bytes::BytesMut) -> anyhow::Result<()> {
		match self {
			Self::Avc3(d) => d.decode_stream(buffer, None),
			Self::Fmp4(d) => d.decode(buffer),
			Self::Ts(d) => d.decode(buffer),
			Self::Flv(d) => d.decode(buffer),
			Self::Hls(_) => unreachable!(),
		}
	}
}

pub struct Publish {
	source: PublishDecoder,
	broadcast: moq_net::BroadcastProducer,
}

impl Publish {
	pub fn new(format: &PublishFormat) -> anyhow::Result<Self> {
		let mut broadcast = moq_net::Broadcast::new().produce();

		// TS carries undecoded elementary streams (SCTE-35, teletext, DVB AC-3, ...)
		// verbatim, so it uses the `mpegts` catalog extension rather than the media-only
		// `()`. The catalog producer owns the broadcast's catalog tracks, so each broadcast
		// gets exactly one; TS builds its `Ext` catalog here instead of the shared `()` below.
		if let PublishFormat::Ts = format {
			let catalog = moq_mux::catalog::Producer::with_catalog(
				&mut broadcast,
				moq_mux::catalog::hang::Catalog::<ts::catalog::Ext>::default(),
			)?;
			let ts = ts::Import::new(broadcast.clone(), catalog);
			return Ok(Self {
				source: PublishDecoder::Ts(Box::new(ts)),
				broadcast,
			});
		}

		let catalog = moq_mux::catalog::Producer::new(&mut broadcast)?;
		let source = match format {
			PublishFormat::Avc3 => {
				let avc3 = moq_mux::codec::h264::Import::new(broadcast.clone(), catalog.clone())
					.with_mode(moq_mux::codec::h264::Mode::Avc3)?;
				PublishDecoder::Avc3(Box::new(avc3))
			}
			PublishFormat::Fmp4 => {
				let fmp4 = fmp4::Import::new(broadcast.clone(), catalog.clone());
				PublishDecoder::Fmp4(Box::new(fmp4))
			}
			PublishFormat::Ts => unreachable!("TS is handled above with the mpegts catalog extension"),
			PublishFormat::Flv => {
				let flv = flv::Import::new(broadcast.clone(), catalog.clone());
				PublishDecoder::Flv(Box::new(flv))
			}
			PublishFormat::Hls { playlist } => {
				let hls = hls::Import::new(broadcast.clone(), catalog.clone(), hls::Config::new(playlist.clone()))?;
				PublishDecoder::Hls(Box::new(hls))
			}
		};

		Ok(Self { source, broadcast })
	}

	pub fn consume(&self) -> moq_net::BroadcastConsumer {
		self.broadcast.consume()
	}

	pub async fn run(self) -> anyhow::Result<()> {
		match self.source {
			PublishDecoder::Hls(mut decoder) => {
				decoder.init().await?;
				decoder.run().await
			}
			mut decoder => {
				let mut stdin = tokio::io::stdin();
				let mut buffer = bytes::BytesMut::new();

				loop {
					let n = tokio::io::AsyncReadExt::read_buf(&mut stdin, &mut buffer).await?;
					if n == 0 {
						return Ok(());
					}
					decoder.decode_buf(&mut buffer)?;
				}
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use bytes::BytesMut;
	use moq_mux::catalog::CatalogFormat;
	use moq_mux::catalog::hang::{Catalog, Container};
	use moq_mux::container::ts::{Export, Import, catalog as tscat};
	use moq_mux::container::{Consumer, Frame, Producer, Timestamp};

	use super::*;

	/// Real H.264 + AAC TS, reused to give the manufactured input a video clock
	/// (section-framed verbatim export requires one) and decodable media tracks.
	const BBB: &[u8] = include_bytes!("../../moq-mux/src/container/ts/test_data/bbb.ts");

	/// A libklvanc public-sample SCTE-35 splice_info_section (table_id 0xFC), carried
	/// on a section-framed PID. Same bytes the moq-mux export round-trip test uses.
	const CUE: &[u8] = &[
		0xfc, 0x30, 0x1b, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0xf0, 0x0a, 0x05, 0x00, 0x00, 0x2b, 0xb4,
		0x7f, 0xdf, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0xad, 0x25, 0xe8, 0x39,
	];

	/// Payload of an undecoded PES-framed stream (e.g. teletext/DVB AC-3 private data),
	/// carried verbatim on its own PID with the original PES stream_id.
	const PES_PAYLOAD: &[u8] = &[0xDE, 0xAD, 0xBE, 0xEF, 0x01, 0x02];

	const SECTION_PID: u16 = 0x102;
	const VERBATIM_PES_PID: u16 = 0x104;
	const VERBATIM_PES_STREAM_ID: u8 = 0xC0;

	/// Drain an exporter, concatenating every chunk until output stops. The producers
	/// stay alive (retained tracks), so the stream never hard-ends; pull until a
	/// `next()` blocks, surfaced here as a timeout once the buffered frames are gone.
	async fn drain(mut exporter: Export<tscat::Ext>) -> Vec<u8> {
		let mut out = Vec::new();
		while let Ok(res) = tokio::time::timeout(Duration::from_millis(500), exporter.next()).await {
			match res.expect("exporter error") {
				Some(chunk) => out.extend_from_slice(&chunk),
				None => break,
			}
		}
		out
	}

	/// Manufacture a TS feed carrying real video/audio plus one section-framed
	/// verbatim stream (SCTE-35) and one PES-framed verbatim stream, by importing
	/// `bbb.ts` into a broadcast that also holds the two ancillary tracks and
	/// re-exporting with the `mpegts` catalog extension.
	async fn manufacture_input() -> Vec<u8> {
		let mut broadcast = moq_net::Broadcast::new().produce();
		let consumer = broadcast.consume();
		let mut catalog =
			moq_mux::catalog::Producer::with_catalog(&mut broadcast, Catalog::<tscat::Ext>::default()).unwrap();

		// Section-framed verbatim stream (SCTE-35, stream_type 0x86).
		let section = broadcast.unique_track(".scte35").unwrap();
		let mut section_track = tscat::Track::new(SECTION_PID);
		section_track.verbatim = Some(tscat::Verbatim::new(0x86, tscat::Framing::Section));
		catalog.lock().mpegts.tracks.insert(section.name.clone(), section_track);
		let mut section_producer = Producer::new(section, Container::Legacy);
		section_producer
			.write(Frame {
				timestamp: Timestamp::from_millis(40).unwrap(),
				payload: bytes::Bytes::from_static(CUE),
				keyframe: true,
			})
			.unwrap();
		section_producer.finish_group().unwrap();
		section_producer.finish().unwrap();

		// PES-framed verbatim stream (undecoded private data, stream_type 0x06), with
		// an explicit PES stream_id to round-trip.
		let pes = broadcast.unique_track(".data").unwrap();
		let mut verbatim = tscat::Verbatim::new(0x06, tscat::Framing::Pes);
		verbatim.stream_id = Some(VERBATIM_PES_STREAM_ID);
		let mut pes_track = tscat::Track::new(VERBATIM_PES_PID);
		pes_track.verbatim = Some(verbatim);
		catalog.lock().mpegts.tracks.insert(pes.name.clone(), pes_track);
		let mut pes_producer = Producer::new(pes, Container::Legacy);
		pes_producer
			.write(Frame {
				timestamp: Timestamp::from_millis(40).unwrap(),
				payload: bytes::Bytes::from_static(PES_PAYLOAD),
				keyframe: true,
			})
			.unwrap();
		pes_producer.finish_group().unwrap();
		pes_producer.finish().unwrap();

		// Add the real video/audio (moves `broadcast` into the importer).
		let mut import = Import::new(broadcast, catalog.clone());
		import.decode(&mut BytesMut::from(BBB)).unwrap();
		import.finish().unwrap();

		// `catalog`, the producers, and `import` stay alive: the exporter subscribes to
		// the retained tracks.
		drain(
			Export::with_ts(consumer, CatalogFormat::Hang)
				.unwrap()
				.with_latency(Duration::ZERO),
		)
		.await
	}

	/// Full CLI round-trip: a TS feed with undecoded streams goes through `Publish`
	/// (which selects the `mpegts` catalog) and the subscribe-side `Export::with_ts`,
	/// and the SCTE-35 section and the verbatim PES survive with their PIDs, framing,
	/// PES stream_id, and byte-exact payloads.
	#[tokio::test(start_paused = true)]
	async fn ts_verbatim_streams_round_trip_through_cli() {
		// Paused time auto-advances when the exporter parks, so the `drain` timeouts
		// fire instantly instead of waiting on the wall clock.
		let input = manufacture_input().await;

		// Publish side: `Publish::new(Ts)` builds a `ts::Import<Ext>`, so the verbatim
		// streams land in the broadcast instead of being dropped by the media-only path.
		let mut publish = Publish::new(&PublishFormat::Ts).unwrap();
		let consumer = publish.consume();
		let mut buffer = BytesMut::from(&input[..]);
		publish.source.decode_buf(&mut buffer).unwrap();
		let PublishDecoder::Ts(import) = &mut publish.source else {
			panic!("expected a TS decoder");
		};
		import.finish().unwrap();

		// Subscribe side: the same `with_ts` call `run_ts` makes, re-emitting the
		// ancillary streams verbatim.
		let output = drain(
			Export::with_ts(consumer, CatalogFormat::Hang)
				.unwrap()
				.with_latency(Duration::ZERO),
		)
		.await;

		// Re-import the round-tripped TS and inspect the recovered `mpegts` section.
		let mut broadcast = moq_net::Broadcast::new().produce();
		let consumer = broadcast.consume();
		let catalog =
			moq_mux::catalog::Producer::with_catalog(&mut broadcast, Catalog::<tscat::Ext>::default()).unwrap();
		let mut import = Import::new(broadcast, catalog.clone());
		import.decode(&mut BytesMut::from(&output[..])).unwrap();
		import.finish().unwrap();
		let snapshot = catalog.snapshot();

		let (section_name, section) = snapshot
			.mpegts
			.tracks
			.iter()
			.find(|(_, t)| t.verbatim.as_ref().is_some_and(|v| v.stream_type == 0x86))
			.expect("SCTE-35 section survived the round-trip");
		assert_eq!(section.pid, SECTION_PID, "section PID preserved");
		assert_eq!(
			section.verbatim.as_ref().unwrap().framing,
			tscat::Framing::Section,
			"section framing preserved"
		);
		let section_name = section_name.clone();

		let (pes_name, pes) = snapshot
			.mpegts
			.tracks
			.iter()
			.find(|(_, t)| t.verbatim.as_ref().is_some_and(|v| v.stream_type == 0x06))
			.expect("verbatim PES survived the round-trip");
		assert_eq!(pes.pid, VERBATIM_PES_PID, "verbatim PES PID preserved");
		let pes_verbatim = pes.verbatim.as_ref().unwrap();
		assert_eq!(pes_verbatim.framing, tscat::Framing::Pes, "PES framing preserved");
		assert_eq!(
			pes_verbatim.stream_id,
			Some(VERBATIM_PES_STREAM_ID),
			"PES stream_id preserved"
		);
		let pes_name = pes_name.clone();

		assert_eq!(
			read_frame(&consumer, &section_name).await,
			CUE,
			"SCTE-35 section round-trips byte-for-byte"
		);
		assert_eq!(
			read_frame(&consumer, &pes_name).await,
			PES_PAYLOAD,
			"verbatim PES payload round-trips byte-for-byte"
		);
	}

	/// Read the first frame of a verbatim track back as raw bytes.
	async fn read_frame(consumer: &moq_net::BroadcastConsumer, name: &str) -> Vec<u8> {
		let track = consumer
			.subscribe_track(&moq_net::Track::new(name.to_string()))
			.unwrap();
		let mut reader = Consumer::new(track, Container::Legacy).with_latency(Duration::ZERO);
		let frame = tokio::time::timeout(Duration::from_secs(1), reader.read())
			.await
			.expect("verbatim read timed out")
			.unwrap()
			.expect("a published verbatim frame");
		frame.payload.to_vec()
	}
}
