//! MPEG-TS muxer.
//!
//! [`Export`] subscribes to a MoQ broadcast and produces MPEG-TS, yielding one
//! [`Frame`] per media frame: PAT/PMT program tables followed by one PES packet,
//! packetized into 188-byte TS packets. Each frame keeps its media timestamp so
//! the caller can pace delivery on the media clock. Video is carried as Annex-B,
//! audio as ADTS AAC.
//!
//! Video flows through [`ExportSource`], which normalizes every H.264/H.265
//! source to length-prefixed NALU plus a resolved avcC/hvcC (parsing in-band
//! avc3/hev1 parameter sets out of the bitstream, or taking the catalog
//! `description` for out-of-band avc1/hvc1). The muxer then does one
//! length-prefixed -> Annex-B conversion, re-injecting the parameter sets as
//! inline NALs on every keyframe. CMAF tracks are rejected with a clear error.

use std::collections::HashMap;
use std::task::Poll;
use std::time::Duration;

use anyhow::Context;
use bytes::Bytes;
use hang::catalog::{AudioCodec, AudioConfig, Container, VideoCodec, VideoConfig};
use mpeg2ts::es::StreamId;
use mpeg2ts::es::StreamType;
use mpeg2ts::time::Timestamp as TsTimestamp;
use mpeg2ts::ts::payload::{Bytes as TsBytes, Pat, Pes, Pmt, Section};
use mpeg2ts::ts::{
	AdaptationField, ContinuityCounter, Descriptor, EsInfo, Pid, ProgramAssociation, TransportScramblingControl,
	TsHeader, TsPacket, TsPacketWriter, TsPayload, VersionNumber, WriteTsPacket,
};

use moq_net::Timestamp;

use crate::catalog::hang::Catalog;
use crate::catalog::{CatalogFormat, Stream};
use crate::codec::annexb;
use crate::container::{ExportSource, Frame};

use super::adts;
use super::scte35;

/// PID of the single program's PMT.
const PMT_PID: u16 = 0x1000;
/// First elementary-stream PID; each track gets the next one.
const FIRST_ES_PID: u16 = 0x1001;
/// Re-emit PAT/PMT at least this often (wall-clock of the media) for tune-in.
const PSI_INTERVAL: Duration = Duration::from_millis(500);

/// Subscribe to a broadcast and produce an MPEG-TS byte stream.
///
/// Use [`next`](Self::next) to pull one [`Frame`] per media frame: its `payload`
/// is the TS packets, stamped with the source `timestamp` and `keyframe` flag.
/// The leading PAT/PMT rides on the first frame (so it inherits a real
/// timestamp), and is re-emitted at video keyframes and periodically for
/// mid-stream tune-in. Returns `None` when the broadcast ends.
pub struct Export<E: scte35::Catalog = ()> {
	broadcast: moq_net::BroadcastConsumer,
	catalog: Option<crate::catalog::Consumer<E>>,
	latency: Duration,

	tracks: HashMap<String, Track>,
	/// Continuity counter per PID (PAT, PMT, and each elementary stream).
	counters: HashMap<u16, ContinuityCounter>,

	/// Program tables, built once the track layout is known.
	psi: Option<Psi>,
	/// Media timestamp of the last PAT/PMT emission.
	last_psi: Option<Timestamp>,
}

struct Track {
	source: ExportSource,
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
	/// MP2, carried verbatim. The sample rate picks the stream type on the way
	/// out (0x03 vs 0x04).
	Mp2 { sample_rate: u32 },
	/// AC-3 (ATSC stream_type 0x81), carried verbatim.
	Ac3,
	/// E-AC-3 (ATSC stream_type 0x87), carried verbatim.
	Eac3,
	/// SCTE-35: private sections (stream_type 0x86), carried verbatim.
	Scte35,
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
	timestamp: Timestamp,
}

impl Export {
	/// Subscribe to `broadcast`, using the default catalog format.
	pub async fn new(broadcast: moq_net::BroadcastConsumer) -> Result<Self, crate::Error> {
		Self::with_catalog_format(broadcast, CatalogFormat::default()).await
	}

	/// Subscribe to `broadcast`, selecting an explicit catalog format. Media only;
	/// any catalog extension (e.g. `.scte35` cues) is ignored.
	pub async fn with_catalog_format(
		broadcast: moq_net::BroadcastConsumer,
		catalog_format: CatalogFormat,
	) -> Result<Self, crate::Error> {
		Self::build(broadcast, catalog_format).await
	}
}

impl Export<scte35::Ext> {
	/// Subscribe to `broadcast`, exporting its `.scte35` cue tracks back to MPEG-TS
	/// alongside the media. The `Self` type pins the extension, so callers write
	/// `Export::with_scte35(..)` with no turbofish (the plain constructors are media-only).
	pub async fn with_scte35(
		broadcast: moq_net::BroadcastConsumer,
		catalog_format: CatalogFormat,
	) -> Result<Self, crate::Error> {
		Self::build(broadcast, catalog_format).await
	}
}

impl<E: scte35::Catalog> Export<E> {
	/// Shared constructor. The public entry points each live on a concrete
	/// `Export<E>` impl that pins `E`, so the extension is chosen by which one you call.
	async fn build(broadcast: moq_net::BroadcastConsumer, catalog_format: CatalogFormat) -> Result<Self, crate::Error> {
		let catalog = crate::catalog::Consumer::<E>::new(&broadcast, catalog_format).await?;
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

	/// Get the next muxed frame.
	///
	/// Each [`Frame`] carries the TS packets for one media frame in `payload`,
	/// stamped with that frame's media `timestamp` and `keyframe` flag so a
	/// transport can pace delivery on the media clock. The leading PAT/PMT rides
	/// on the first frame (inheriting its timestamp), and is re-emitted at video
	/// keyframes and periodically for mid-stream tune-in. Returns `None` when the
	/// broadcast ends. `duration` is always `None`: the muxer has no use for it.
	pub async fn next(&mut self) -> anyhow::Result<Option<Frame>> {
		kio::wait(|waiter| self.poll_next(waiter)).await
	}

	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<anyhow::Result<Option<Frame>>> {
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

		// 2. Pull a frame into every idle track. ExportSource has already
		// transformed Annex-B avc3/hev1 into length-prefixed form and resolved
		// the avcC/hvcC. Before the program tables are written, drop slices that
		// arrive before their codec config resolves: a receiver joining mid-GOP
		// can't use them, and parking them would stop us polling for the keyframe
		// that carries the parameter sets.
		let waiting_for_header = self.psi.is_none();
		for track in self.tracks.values_mut() {
			if track.pending.is_some() || track.finished {
				continue;
			}
			loop {
				match track.source.poll_read(waiter) {
					Poll::Ready(Ok(Some(frame))) => {
						if waiting_for_header && !track.source.header_ready() {
							continue;
						}
						track.pending = Some(frame);
						break;
					}
					Poll::Ready(Ok(None)) => {
						track.finished = true;
						break;
					}
					Poll::Ready(Err(e)) => return Poll::Ready(Err(e.into())),
					Poll::Pending => break,
				}
			}
		}

		// 3. Build the program tables once the layout is resolved and every
		// track's codec config is ready. The tables aren't emitted here: PSI has
		// no media time of its own, so `write_frame` prepends them to the first
		// frame instead, letting the leading PAT/PMT inherit a real timestamp.
		if self.psi.is_none() {
			if self.tracks.is_empty() {
				// No tracks yet. If the catalog is also done, the broadcast is empty.
				if self.catalog.is_none() {
					return Poll::Ready(Ok(None));
				}
				return Poll::Pending;
			}
			if !self.header_ready() {
				// Still waiting on codec configs. If every track finished without
				// producing one, the broadcast can't be muxed.
				if self.catalog.is_none() && self.tracks.values().all(|t| t.finished) {
					return Poll::Ready(Ok(None));
				}
				return Poll::Pending;
			}
			self.build_psi()?;
		}

		// 4. Emit the smallest-timestamp pending frame as a PES packet (the first
		// one carries the buffered PAT/PMT).
		if let Some(name) = self.pick_next_track() {
			let frame = self.tracks.get_mut(&name).unwrap().pending.take().unwrap();
			let out = self.write_frame(&name, frame)?;
			return Poll::Ready(Ok(Some(out)));
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

	fn update_catalog(&mut self, mut catalog: Catalog<E>) -> anyhow::Result<()> {
		// The cue tracks live in the extension. The trait only exposes `scte35_mut`,
		// and this snapshot is owned, so clone the section out (`()` yields the
		// empty default: zero cue tracks).
		let scte35 = catalog.scte35_mut().cloned().unwrap_or_default();

		let mut active: HashMap<String, ()> = HashMap::new();
		for name in catalog.video.renditions.keys() {
			active.insert(name.clone(), ());
		}
		for name in catalog.audio.renditions.keys() {
			active.insert(name.clone(), ());
		}
		for name in scte35.renditions.keys() {
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
			let source = ExportSource::for_video(&self.broadcast, name, config, self.latency)?;
			self.tracks.insert(
				name.clone(),
				Track {
					source,
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
			let source = ExportSource::for_audio(&self.broadcast, name, config, self.latency)?;
			self.tracks.insert(
				name.clone(),
				Track {
					source,
					pending: None,
					finished: false,
					pid: next_pid,
					kind,
				},
			);
			next_pid += 1;
		}

		for (name, config) in scte35.renditions.iter() {
			if self.tracks.contains_key(name) {
				continue;
			}
			let kind = scte35_kind(config, name)?;
			let source = ExportSource::for_scte35(&self.broadcast, name, config, self.latency)?;
			self.tracks.insert(
				name.clone(),
				Track {
					source,
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

	/// Header is ready when every track's [`ExportSource`] has resolved its
	/// codec config (from the catalog `description`, or built by the transform).
	fn header_ready(&self) -> bool {
		self.tracks.values().all(|t| t.source.header_ready())
	}

	/// Build the PAT/PMT once every track's PID and codec is known.
	fn build_psi(&mut self) -> anyhow::Result<()> {
		// Order tracks by PID for a stable layout; first video track carries the PCR.
		let mut tracks: Vec<&Track> = self.tracks.values().collect();
		tracks.sort_by_key(|t| t.pid);

		// SCTE-35 cues are stamped on the video clock (and SCTE carries no PTS for the PCR),
		// so a cue program needs a video track; audio alone would leave the cues pinned to zero.
		let has_scte = tracks.iter().any(|t| matches!(t.kind, Kind::Scte35));
		let video = tracks.iter().find(|t| matches!(t.kind, Kind::Video(_)));
		anyhow::ensure!(
			!has_scte || video.is_some(),
			"TS export of SCTE-35 requires a video track for the program clock"
		);
		let pcr_pid = video
			.or_else(|| {
				tracks
					.iter()
					.find(|t| matches!(t.kind, Kind::Aac { .. } | Kind::Mp2 { .. } | Kind::Ac3 | Kind::Eac3))
			})
			.map(|t| t.pid)
			.context("TS export requires a video or audio track for the PCR")?;

		let es_info = tracks
			.iter()
			.map(|t| {
				Ok(EsInfo {
					stream_type: match t.kind {
						Kind::Video(stream_type) => stream_type,
						Kind::Aac { .. } => StreamType::AdtsAac,
						// Half-rate MPEG-2 BC audio (< 32 kHz) re-announces as 0x04;
						// the full rates are MPEG-1 (0x03). The catalog sample rate
						// came from the frame header, so the mapping is faithful.
						Kind::Mp2 { sample_rate } if sample_rate < 32000 => StreamType::Mpeg2HalvedSampleRateAudio,
						Kind::Mp2 { .. } => StreamType::Mpeg1Audio,
						Kind::Ac3 => StreamType::DolbyDigitalUpToSixChannelAudio,
						Kind::Eac3 => StreamType::DolbyDigitalPlusUpTo16ChannelAudioForAtsc,
						Kind::Scte35 => StreamType::Dts8ChannelLosslessAudio,
					},
					elementary_pid: Pid::new(t.pid)?,
					// ATSC pairs the Dolby stream types with a registration descriptor.
					descriptors: match t.kind {
						Kind::Ac3 => vec![Descriptor {
							tag: 0x05,
							data: b"AC-3".to_vec(),
						}],
						Kind::Eac3 => vec![Descriptor {
							tag: 0x05,
							data: b"EAC3".to_vec(),
						}],
						_ => Vec::new(),
					},
				})
			})
			.collect::<anyhow::Result<Vec<_>>>()?;

		// SCTE-35 is announced by a program-level 'CUEI' registration descriptor;
		// the import keys detection off it (stream_type 0x86 alone is ambiguous).
		let program_info = if tracks.iter().any(|t| matches!(t.kind, Kind::Scte35)) {
			vec![Descriptor {
				tag: 0x05,
				data: b"CUEI".to_vec(),
			}]
		} else {
			Vec::new()
		};

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
			program_info,
			es_info,
		};

		self.psi = Some(Psi { pat, pmt, pcr_pid });
		Ok(())
	}

	/// Name of the track whose pending frame has the smallest timestamp.
	fn pick_next_track(&self) -> Option<String> {
		self.tracks
			.iter()
			.filter_map(|(n, t)| t.pending.as_ref().map(|f| (n.clone(), f.timestamp)))
			.min_by_key(|(_, ts)| *ts)
			.map(|(n, _)| n)
	}

	/// Packetize one media frame into an output [`Frame`], re-emitting PAT/PMT
	/// before video keyframes (and periodically) so receivers can tune in
	/// mid-stream. The returned frame keeps the source `timestamp` and `keyframe`
	/// flag so the caller can pace it.
	fn write_frame(&mut self, name: &str, frame: Frame) -> anyhow::Result<Frame> {
		let track = self.tracks.get(name).context("missing track")?;
		let pid = track.pid;
		let kind = track.kind.clone();
		let is_pcr = self.psi.as_ref().is_some_and(|p| p.pcr_pid == pid);
		let is_video = matches!(kind, Kind::Video(_));
		let timestamp = frame.timestamp;
		let keyframe = frame.keyframe;

		// Build the elementary-stream payload for this frame. Video needs the
		// resolved avcC/hvcC to rewrite length-prefixed NALs as Annex-B. SCTE-35
		// carries no PES payload; the section is written separately below.
		let es_payload = match &kind {
			Kind::Video(stream_type) => Some(video_es_payload(*stream_type, track.source.description(), &frame)?),
			Kind::Aac {
				object_type,
				sample_rate,
				channel_count,
			} => {
				let header = adts::write_header(*object_type, *sample_rate, *channel_count, frame.payload.len())?;
				let mut framed = Vec::with_capacity(7 + frame.payload.len());
				framed.extend_from_slice(&header);
				framed.extend_from_slice(&frame.payload);
				Some(framed)
			}
			// Legacy audio frames were ingested whole (framing header included), so
			// they pass through untouched.
			Kind::Mp2 { .. } | Kind::Ac3 | Kind::Eac3 => Some(frame.payload.to_vec()),
			Kind::Scte35 => None,
		};

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

		match es_payload {
			// SCTE-35 rides in private sections, not PES; carry the bytes verbatim.
			None => self.write_section(&mut out, pid, &frame.payload)?,
			Some(es_payload) => {
				let unit = PesUnit {
					pid,
					is_pcr,
					is_video,
					keyframe: frame.keyframe,
					timestamp: frame.timestamp,
				};
				self.write_pes(&mut out, &unit, &es_payload)?;
			}
		}
		Ok(Frame {
			timestamp,
			duration: None,
			payload: Bytes::from(out),
			keyframe,
		})
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

	/// Packetize a private section (SCTE-35) verbatim. The first packet carries the
	/// pointer_field plus the section start as a `Section` payload (sets the unit-
	/// start bit so the receiver finds the pointer_field); continuations are `Raw`.
	/// The section bytes are opaque, so this round-trips byte-for-byte.
	fn write_section(&mut self, out: &mut Vec<u8>, pid: u16, section: &[u8]) -> anyhow::Result<()> {
		// The .scte35 track is public; a non-importer producer could publish a frame
		// that isn't a complete splice_info_section. Drop it (with a warning) rather
		// than emit a malformed section a downstream demuxer would choke on. One bad
		// cue must not abort a live export, so this skips instead of erroring.
		if !is_complete_scte35_section(section) {
			tracing::warn!(pid, len = section.len(), "dropping malformed SCTE-35 section on export");
			return Ok(());
		}

		let mut offset = 0;
		let mut first = true;
		loop {
			let payload = if first {
				// pointer_field (1 byte, written by `Section`) eats one payload byte.
				let take = (TsBytes::MAX_SIZE - 1).min(section.len());
				let chunk = &section[..take];
				offset = take;
				TsPayload::Section(Section {
					pointer_field: 0,
					data: TsBytes::new(chunk).map_err(anyhow::Error::msg)?,
				})
			} else {
				let take = TsBytes::MAX_SIZE.min(section.len() - offset);
				let chunk = &section[offset..offset + take];
				offset += take;
				TsPayload::Raw(TsBytes::new(chunk).map_err(anyhow::Error::msg)?)
			};

			self.write_packet(out, pid, None, payload)?;
			first = false;
			if offset >= section.len() {
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

fn psi_interval() -> Timestamp {
	Timestamp::try_from(PSI_INTERVAL).unwrap_or(Timestamp::ZERO)
}

/// External byte size of an adaptation field (manual mirror of the crate's
/// private `external_size`); only PCR is ever set.
fn adaptation_size(af: &AdaptationField) -> usize {
	2 + if af.pcr.is_some() { 6 } else { 0 }
}

fn to_ts_timestamp(timestamp: Timestamp) -> anyhow::Result<TsTimestamp> {
	// micros -> 90 kHz, wrapped into the 33-bit field.
	let micros = timestamp.as_micros();
	let ticks = (micros * 90_000 / 1_000_000) as u64 & ((1 << 33) - 1);
	TsTimestamp::new(ticks).map_err(anyhow::Error::msg)
}

fn video_kind(config: &VideoConfig, name: &str) -> anyhow::Result<Kind> {
	ensure_raw(&config.container, "video", name)?;
	// Both in-band (avc3/hev1) and out-of-band (avc1/hvc1) are accepted:
	// ExportSource normalizes both to length-prefixed NALU + avcC/hvcC, and the
	// muxer rewrites them to Annex-B.
	match &config.codec {
		VideoCodec::H264(_) => Ok(Kind::Video(StreamType::H264)),
		VideoCodec::H265(_) => Ok(Kind::Video(StreamType::H265)),
		other => anyhow::bail!("TS export does not support video codec {other:?} (track '{name}')"),
	}
}

/// Build the Annex-B elementary-stream payload for one video frame: rewrite the
/// length-prefixed NALs to start-code-delimited NALs, prepending the parameter
/// sets (SPS/PPS, plus VPS for H.265) from the avcC/hvcC on keyframes so a
/// receiver can tune in mid-stream.
fn video_es_payload(stream_type: StreamType, description: Option<&Bytes>, frame: &Frame) -> anyhow::Result<Vec<u8>> {
	let description = description.context("video codec config (avcC/hvcC) not resolved")?;
	let (length_size, params) = match stream_type {
		StreamType::H264 => crate::codec::h264::avcc_params(description)?,
		StreamType::H265 => crate::codec::h265::hvcc_params(description)?,
		other => anyhow::bail!("unsupported TS video stream type {other:?}"),
	};

	let mut out = Vec::with_capacity(frame.payload.len() + 64);
	if frame.keyframe {
		for nal in &params {
			out.extend_from_slice(&annexb::START_CODE);
			out.extend_from_slice(nal);
		}
	}
	annexb::length_prefixed_to_annexb(&frame.payload, length_size, &mut out)?;
	Ok(out)
}

fn audio_kind(config: &AudioConfig, name: &str) -> anyhow::Result<Kind> {
	ensure_raw(&config.container, "audio", name)?;
	match &config.codec {
		AudioCodec::AAC(aac) => Ok(Kind::Aac {
			object_type: aac.profile,
			sample_rate: config.sample_rate,
			channel_count: config.channel_count,
		}),
		AudioCodec::Mp2 => Ok(Kind::Mp2 {
			sample_rate: config.sample_rate,
		}),
		AudioCodec::Ac3 => Ok(Kind::Ac3),
		AudioCodec::Ec3 => Ok(Kind::Eac3),
		other => anyhow::bail!("TS export does not support audio codec {other:?} (track '{name}')"),
	}
}

fn scte35_kind(config: &scte35::Config, name: &str) -> anyhow::Result<Kind> {
	ensure_raw(&config.container, "scte35", name)?;
	Ok(Kind::Scte35)
}

/// One SCTE-35 frame must be exactly one splice_info_section: table_id 0xFC and a
/// total length matching the declared section_length. Structural only (no splice
/// semantics); the bytes are still carried verbatim.
fn is_complete_scte35_section(section: &[u8]) -> bool {
	section.len() >= 3
		&& section[0] == 0xfc
		&& section.len() == 3 + ((((section[1] & 0x0f) as usize) << 8) | section[2] as usize)
}

fn ensure_raw(container: &Container, kind: &str, name: &str) -> anyhow::Result<()> {
	match container {
		// TS carries raw codec payloads, like the Legacy varint and LOC formats.
		Container::Legacy | Container::Loc => Ok(()),
		Container::Cmaf { .. } => anyhow::bail!("TS export does not support CMAF {kind} track '{name}'"),
	}
}

#[cfg(test)]
mod tests {
	use super::is_complete_scte35_section;

	#[test]
	fn scte35_section_validation() {
		// table_id 0xFC, section_length 27 (0x1b) -> 30 bytes total.
		let mut ok = vec![0xfc, 0x30, 0x1b];
		ok.resize(30, 0x00);
		assert!(is_complete_scte35_section(&ok));
		// minimal: section_length 0 -> exactly the 3-byte header.
		assert!(is_complete_scte35_section(&[0xfc, 0x00, 0x00]));

		// shorter than the 3-byte header.
		assert!(!is_complete_scte35_section(&[0xfc, 0x00]));
		// wrong table_id (not a splice_info_section).
		assert!(!is_complete_scte35_section(&[0x00, 0x00, 0x00]));
		// declared section_length (27) does not match the actual length (3).
		assert!(!is_complete_scte35_section(&[0xfc, 0x30, 0x1b]));
	}
}
