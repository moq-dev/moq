//! MPEG-TS muxer.
//!
//! [`Export`] subscribes to a MoQ broadcast and produces a single MPEG-TS byte
//! stream: PAT/PMT program tables followed by one PES packet per media frame,
//! packetized into 188-byte TS packets. Video is carried as Annex-B, audio as
//! ADTS AAC.
//!
//! Only in-band sources are supported: H.264 (`inline: true`), H.265
//! (`in_band: true`), and AAC, all in the Legacy/LOC container (raw codec
//! payloads). Out-of-band avc1/hvc1 (length-prefixed, `description` = avcC/hvcC)
//! and CMAF tracks are rejected with a clear error.

use std::collections::HashMap;
use std::task::Poll;
use std::time::Duration;

use anyhow::Context;
use bytes::Bytes;
use hang::catalog::{AudioCodec, AudioConfig, Catalog, Container, VideoCodec, VideoConfig};
use mpeg2ts::es::StreamId;
use mpeg2ts::es::StreamType;
use mpeg2ts::time::Timestamp as TsTimestamp;
use mpeg2ts::ts::payload::{Bytes as TsBytes, Pat, Pes, Pmt};
use mpeg2ts::ts::{
	AdaptationField, ContinuityCounter, EsInfo, Pid, ProgramAssociation, TransportScramblingControl, TsHeader,
	TsPacket, TsPacketWriter, TsPayload, VersionNumber, WriteTsPacket,
};

use crate::catalog::CatalogFormat;
use crate::catalog::hang::Container as HangContainer;
use crate::container::{CatalogSource, Consumer, Frame};

use super::adts;

/// PID of the single program's PMT.
const PMT_PID: u16 = 0x1000;
/// First elementary-stream PID; each track gets the next one.
const FIRST_ES_PID: u16 = 0x1001;
/// Re-emit PAT/PMT at least this often (wall-clock of the media) for tune-in.
const PSI_INTERVAL: Duration = Duration::from_millis(500);

/// Subscribe to a broadcast and produce an MPEG-TS byte stream.
///
/// Use [`next`](Self::next) to pull byte chunks: the first chunk is PAT+PMT, then
/// each subsequent chunk is the TS packets for one media frame (preceded by a
/// fresh PAT+PMT at video keyframes). Returns `None` when the broadcast ends.
pub struct Export {
	broadcast: moq_net::BroadcastConsumer,
	catalog: Option<CatalogSource>,
	latency: Duration,

	tracks: HashMap<String, Track>,
	/// Continuity counter per PID (PAT, PMT, and each elementary stream).
	counters: HashMap<u16, ContinuityCounter>,

	/// Program tables, built once the track layout is known.
	psi: Option<Psi>,
	/// Media timestamp of the last PAT/PMT emission.
	last_psi: Option<crate::container::Timestamp>,
}

struct Track {
	consumer: Consumer<HangContainer>,
	pending: Option<Frame>,
	finished: bool,
	pid: u16,
	kind: Kind,
}

#[derive(Clone)]
enum Kind {
	/// Video carries its TS stream type (H.264 = 0x1B, H.265 = 0x24).
	Video(StreamType),
	Aac {
		object_type: u8,
		sample_rate: u32,
		channel_count: u32,
	},
}

/// The program tables plus the resolved PID layout.
struct Psi {
	pat: Pat,
	pmt: Pmt,
	pcr_pid: u16,
}

/// Per-frame PES descriptor (everything but the payload bytes).
struct PesUnit {
	pid: u16,
	is_pcr: bool,
	is_video: bool,
	keyframe: bool,
	timestamp: crate::container::Timestamp,
}

impl Export {
	/// Subscribe to `broadcast`, using the default catalog format.
	pub fn new(broadcast: moq_net::BroadcastConsumer) -> Result<Self, crate::Error> {
		Self::with_catalog_format(broadcast, CatalogFormat::default())
	}

	/// Subscribe to `broadcast`, selecting an explicit catalog format.
	pub fn with_catalog_format(
		broadcast: moq_net::BroadcastConsumer,
		catalog_format: CatalogFormat,
	) -> Result<Self, crate::Error> {
		let catalog = CatalogSource::new(&broadcast, catalog_format)?;
		Ok(Self {
			broadcast,
			catalog: Some(catalog),
			latency: Duration::ZERO,
			tracks: HashMap::new(),
			counters: HashMap::new(),
			psi: None,
			last_psi: None,
		})
	}

	/// Set the maximum buffering latency for each per-track source.
	pub fn with_latency(mut self, latency: Duration) -> Self {
		self.latency = latency;
		self
	}

	/// Get the next byte chunk.
	pub async fn next(&mut self) -> anyhow::Result<Option<Bytes>> {
		kio::wait(|waiter| self.poll_next(waiter)).await
	}

	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<anyhow::Result<Option<Bytes>>> {
		// 1. Drain catalog updates, discovering the track layout.
		while let Some(catalog) = self.catalog.as_mut() {
			match catalog.poll_next(waiter)? {
				Poll::Ready(Some(snapshot)) => self.update_catalog(snapshot)?,
				Poll::Ready(None) => {
					self.catalog = None;
					break;
				}
				Poll::Pending => break,
			}
		}

		// 2. Emit the program tables once the layout is resolved.
		if self.psi.is_none() {
			if self.tracks.is_empty() {
				// No tracks yet. If the catalog is also done, the broadcast is empty.
				if self.catalog.is_none() {
					return Poll::Ready(Ok(None));
				}
				return Poll::Pending;
			}
			self.build_psi()?;
			let header = self.write_psi()?;
			return Poll::Ready(Ok(Some(header)));
		}

		// 3. Pull a frame into every idle track.
		for track in self.tracks.values_mut() {
			if track.pending.is_some() || track.finished {
				continue;
			}
			match track.consumer.poll_read(waiter) {
				Poll::Ready(Ok(Some(frame))) => track.pending = Some(frame),
				Poll::Ready(Ok(None)) => track.finished = true,
				Poll::Ready(Err(e)) => return Poll::Ready(Err(e.into())),
				Poll::Pending => {}
			}
		}

		// 4. Emit the smallest-timestamp pending frame as a PES packet.
		if let Some(name) = self.pick_next_track() {
			let frame = self.tracks.get_mut(&name).unwrap().pending.take().unwrap();
			let chunk = self.write_frame(&name, frame)?;
			return Poll::Ready(Ok(Some(chunk)));
		}

		// 5. End of stream once every track has drained and the catalog is closed.
		if self.catalog.is_none() && !self.tracks.is_empty() && self.tracks.values().all(|t| t.finished) {
			return Poll::Ready(Ok(None));
		}
		if self.catalog.is_none() && self.tracks.is_empty() {
			return Poll::Ready(Ok(None));
		}

		Poll::Pending
	}

	fn update_catalog(&mut self, catalog: Catalog) -> anyhow::Result<()> {
		let mut active: HashMap<String, ()> = HashMap::new();
		for name in catalog.video.renditions.keys() {
			active.insert(name.clone(), ());
		}
		for name in catalog.audio.renditions.keys() {
			active.insert(name.clone(), ());
		}

		// The program tables are written once; reject layout changes afterwards.
		if self.psi.is_some() {
			for name in active.keys() {
				anyhow::ensure!(
					self.tracks.contains_key(name),
					"TS track layout changed after PAT/PMT was emitted: '{name}' added"
				);
			}
			for name in self.tracks.keys() {
				anyhow::ensure!(
					active.contains_key(name),
					"TS track layout changed after PAT/PMT was emitted: '{name}' removed"
				);
			}
			return Ok(());
		}

		let mut next_pid = self
			.tracks
			.values()
			.map(|t| t.pid)
			.max()
			.map(|p| p + 1)
			.unwrap_or(FIRST_ES_PID);

		for (name, config) in catalog.video.renditions.iter() {
			if self.tracks.contains_key(name) {
				continue;
			}
			let kind = video_kind(config, name)?;
			let consumer = self.subscribe(name, &config.container)?;
			self.tracks.insert(
				name.clone(),
				Track {
					consumer,
					pending: None,
					finished: false,
					pid: next_pid,
					kind,
				},
			);
			next_pid += 1;
		}

		for (name, config) in catalog.audio.renditions.iter() {
			if self.tracks.contains_key(name) {
				continue;
			}
			let kind = audio_kind(config, name)?;
			let consumer = self.subscribe(name, &config.container)?;
			self.tracks.insert(
				name.clone(),
				Track {
					consumer,
					pending: None,
					finished: false,
					pid: next_pid,
					kind,
				},
			);
			next_pid += 1;
		}

		self.tracks.retain(|name, _| active.contains_key(name));
		Ok(())
	}

	fn subscribe(&self, name: &str, container: &Container) -> anyhow::Result<Consumer<HangContainer>> {
		let media: HangContainer = container.try_into()?;
		let track = self.broadcast.subscribe_track(&moq_net::Track::new(name.to_string()))?;
		Ok(Consumer::new(track, media).with_latency(self.latency))
	}

	/// Build the PAT/PMT once every track's PID and codec is known.
	fn build_psi(&mut self) -> anyhow::Result<()> {
		// Order tracks by PID for a stable layout; first video track carries the PCR.
		let mut tracks: Vec<&Track> = self.tracks.values().collect();
		tracks.sort_by_key(|t| t.pid);

		let pcr_pid = tracks
			.iter()
			.find(|t| matches!(t.kind, Kind::Video(_)))
			.or_else(|| tracks.first())
			.map(|t| t.pid)
			.context("no tracks to build PMT")?;

		let es_info = tracks
			.iter()
			.map(|t| {
				Ok(EsInfo {
					stream_type: match t.kind {
						Kind::Video(stream_type) => stream_type,
						Kind::Aac { .. } => StreamType::AdtsAac,
					},
					elementary_pid: Pid::new(t.pid)?,
					descriptors: Vec::new(),
				})
			})
			.collect::<anyhow::Result<Vec<_>>>()?;

		let pat = Pat {
			transport_stream_id: 1,
			version_number: VersionNumber::default(),
			table: vec![ProgramAssociation {
				program_num: 1,
				program_map_pid: Pid::new(PMT_PID)?,
			}],
		};
		let pmt = Pmt {
			program_num: 1,
			pcr_pid: Some(Pid::new(pcr_pid)?),
			version_number: VersionNumber::default(),
			program_info: Vec::new(),
			es_info,
		};

		self.psi = Some(Psi { pat, pmt, pcr_pid });
		Ok(())
	}

	/// Serialize a fresh PAT + PMT into a chunk.
	fn write_psi(&mut self) -> anyhow::Result<Bytes> {
		let psi = self.psi.as_ref().context("PSI not built")?;
		let pat = TsPayload::Pat(psi.pat.clone());
		let pmt = TsPayload::Pmt(psi.pmt.clone());

		let mut out = Vec::with_capacity(2 * TsPacket::SIZE);
		self.write_packet(&mut out, Pid::PAT, None, pat)?;
		self.write_packet(&mut out, PMT_PID, None, pmt)?;
		Ok(Bytes::from(out))
	}

	fn pick_next_track(&self) -> Option<String> {
		self.tracks
			.iter()
			.filter_map(|(n, t)| t.pending.as_ref().map(|f| (n.clone(), f.timestamp)))
			.min_by_key(|(_, ts)| *ts)
			.map(|(n, _)| n)
	}

	/// Packetize one media frame into a chunk, re-emitting PAT/PMT before video
	/// keyframes (and periodically) so receivers can tune in mid-stream.
	fn write_frame(&mut self, name: &str, frame: Frame) -> anyhow::Result<Bytes> {
		let track = self.tracks.get(name).context("missing track")?;
		let pid = track.pid;
		let kind = track.kind.clone();
		let is_pcr = self.psi.as_ref().is_some_and(|p| p.pcr_pid == pid);
		let is_video = matches!(kind, Kind::Video(_));

		let mut out = Vec::with_capacity(TsPacket::SIZE);

		// Refresh PSI at keyframes or after the interval lapses.
		let psi_due = match self.last_psi {
			None => true,
			Some(last) => frame.timestamp >= last && (frame.timestamp - last) >= psi_interval(),
		};
		if (is_video && frame.keyframe) || psi_due {
			let psi = self.psi.as_ref().context("PSI not built")?;
			let pat = TsPayload::Pat(psi.pat.clone());
			let pmt = TsPayload::Pmt(psi.pmt.clone());
			self.write_packet(&mut out, Pid::PAT, None, pat)?;
			self.write_packet(&mut out, PMT_PID, None, pmt)?;
			self.last_psi = Some(frame.timestamp);
		}

		// Build the elementary-stream payload for this frame.
		let payload = match &kind {
			Kind::Video(_) => frame.payload.to_vec(),
			Kind::Aac {
				object_type,
				sample_rate,
				channel_count,
			} => {
				let header = adts::write_header(*object_type, *sample_rate, *channel_count, frame.payload.len())?;
				let mut framed = Vec::with_capacity(7 + frame.payload.len());
				framed.extend_from_slice(&header);
				framed.extend_from_slice(&frame.payload);
				framed
			}
		};

		let unit = PesUnit {
			pid,
			is_pcr,
			is_video,
			keyframe: frame.keyframe,
			timestamp: frame.timestamp,
		};
		self.write_pes(&mut out, &unit, &payload)?;
		Ok(Bytes::from(out))
	}

	/// Packetize a PES payload into 188-byte TS packets.
	fn write_pes(&mut self, out: &mut Vec<u8>, unit: &PesUnit, payload: &[u8]) -> anyhow::Result<()> {
		let pts = to_ts_timestamp(unit.timestamp)?;
		let stream_id = if unit.is_video {
			StreamId::new(StreamId::VIDEO_MIN)
		} else {
			StreamId::new(StreamId::AUDIO_MIN)
		};
		let header = mpeg2ts::pes::PesHeader {
			stream_id,
			priority: false,
			data_alignment_indicator: true,
			copyright: false,
			original_or_copy: false,
			pts: Some(pts),
			dts: None,
			escr: None,
		};

		// `pes_packet_len` counts the optional header plus the payload (not the
		// 6-byte fixed prefix). Unbounded for video (0); bounded for audio when
		// it fits a u16.
		let pes_packet_len = if unit.is_video {
			0
		} else {
			u16::try_from(PES_OPTIONAL_LEN + payload.len()).unwrap_or(0)
		};

		let mut offset = 0;
		let mut first = true;
		loop {
			let adaptation = if first && (unit.is_pcr || unit.keyframe) {
				Some(AdaptationField {
					discontinuity_indicator: false,
					random_access_indicator: unit.keyframe,
					es_priority_indicator: false,
					pcr: if unit.is_pcr { Some(pts.into()) } else { None },
					opcr: None,
					splice_countdown: None,
					transport_private_data: Vec::new(),
					extension: None,
				})
			} else {
				None
			};

			let header_len = if first { PES_HEADER_LEN } else { 0 };
			let af_len = adaptation.as_ref().map(adaptation_size).unwrap_or(0);
			let avail = TsBytes::MAX_SIZE - header_len - af_len;
			let take = avail.min(payload.len() - offset);
			let chunk = &payload[offset..offset + take];

			let ts_payload = if first {
				TsPayload::PesStart(Pes {
					header: header.clone(),
					pes_packet_len,
					data: TsBytes::new(chunk).map_err(anyhow::Error::msg)?,
				})
			} else {
				TsPayload::PesContinuation(TsBytes::new(chunk).map_err(anyhow::Error::msg)?)
			};

			self.write_packet(out, unit.pid, adaptation, ts_payload)?;

			offset += take;
			first = false;
			if offset >= payload.len() {
				break;
			}
		}
		Ok(())
	}

	/// Serialize one TS packet (with its continuity counter) into `out`.
	fn write_packet(
		&mut self,
		out: &mut Vec<u8>,
		pid: u16,
		adaptation_field: Option<AdaptationField>,
		payload: TsPayload,
	) -> anyhow::Result<()> {
		let counter = self.counters.entry(pid).or_default();
		let continuity_counter = *counter;
		counter.increment();

		let packet = TsPacket {
			header: TsHeader {
				transport_error_indicator: false,
				transport_priority: false,
				pid: Pid::new(pid)?,
				transport_scrambling_control: TransportScramblingControl::NotScrambled,
				continuity_counter,
			},
			adaptation_field,
			payload: Some(payload),
		};

		let mut writer = TsPacketWriter::new(out);
		writer.write_ts_packet(&packet).map_err(anyhow::Error::msg)?;
		Ok(())
	}
}

/// Optional PES header region carrying PTS only: 2 flag bytes + 1 length byte + 5 PTS bytes.
const PES_OPTIONAL_LEN: usize = 3 + 5;
/// Full on-wire PES header for the first packet: 6-byte fixed prefix + optional region.
const PES_HEADER_LEN: usize = 6 + PES_OPTIONAL_LEN;

fn psi_interval() -> crate::container::Timestamp {
	crate::container::Timestamp::try_from(PSI_INTERVAL).unwrap_or(crate::container::Timestamp::ZERO)
}

/// External byte size of an adaptation field (manual mirror of the crate's
/// private `external_size`); only PCR is ever set.
fn adaptation_size(af: &AdaptationField) -> usize {
	2 + if af.pcr.is_some() { 6 } else { 0 }
}

fn to_ts_timestamp(timestamp: crate::container::Timestamp) -> anyhow::Result<TsTimestamp> {
	// micros -> 90 kHz, wrapped into the 33-bit field.
	let micros = timestamp.as_micros();
	let ticks = (micros * 90_000 / 1_000_000) as u64 & ((1 << 33) - 1);
	TsTimestamp::new(ticks).map_err(anyhow::Error::msg)
}

fn video_kind(config: &VideoConfig, name: &str) -> anyhow::Result<Kind> {
	ensure_raw(&config.container, "video", name)?;
	match &config.codec {
		VideoCodec::H264(h264) => {
			anyhow::ensure!(
				h264.inline,
				"TS export needs in-band H.264 (avc3); track '{name}' is out-of-band (avc1)"
			);
			Ok(Kind::Video(StreamType::H264))
		}
		VideoCodec::H265(h265) => {
			anyhow::ensure!(
				h265.in_band,
				"TS export needs in-band H.265 (hev1); track '{name}' is out-of-band (hvc1)"
			);
			Ok(Kind::Video(StreamType::H265))
		}
		other => anyhow::bail!("TS export does not support video codec {other:?} (track '{name}')"),
	}
}

fn audio_kind(config: &AudioConfig, name: &str) -> anyhow::Result<Kind> {
	ensure_raw(&config.container, "audio", name)?;
	match &config.codec {
		AudioCodec::AAC(aac) => Ok(Kind::Aac {
			object_type: aac.profile,
			sample_rate: config.sample_rate,
			channel_count: config.channel_count,
		}),
		other => anyhow::bail!("TS export does not support audio codec {other:?} (track '{name}')"),
	}
}

fn ensure_raw(container: &Container, kind: &str, name: &str) -> anyhow::Result<()> {
	match container {
		// TS carries raw codec payloads, like the Legacy varint and LOC formats.
		Container::Legacy | Container::Loc => Ok(()),
		Container::Cmaf { .. } => anyhow::bail!("TS export does not support CMAF {kind} track '{name}'"),
	}
}
