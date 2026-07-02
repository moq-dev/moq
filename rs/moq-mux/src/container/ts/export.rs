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
use super::catalog;

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
pub struct Export<E: catalog::Catalog = ()> {
	broadcast: moq_net::BroadcastConsumer,
	catalog: Option<crate::catalog::Consumer<E>>,
	latency: Duration,

	tracks: HashMap<String, Track>,
	/// Continuity counter per PID (PAT, PMT, and each elementary stream).
	counters: HashMap<u16, ContinuityCounter>,
	/// PMT program-level descriptors captured on import, re-emitted in the PMT.
	program_descriptors: Vec<catalog::Descriptor>,

	/// Program tables, built once the track layout is known.
	psi: Option<Psi>,
	/// Media timestamp of the last PAT/PMT emission.
	last_psi: Option<Timestamp>,
	/// Tune-in point: the first video keyframe's timestamp, captured when the program
	/// tables are built. Non-video frames before it are dropped so the keyframe leads
	/// the stream.
	///
	/// MPEG-TS carries the H.264/H.265 parameter sets in-band on the keyframe (unlike
	/// RTMP/CMAF, which carry the codec config out-of-band in the header). On a
	/// mid-stream join the audio source can start over a second before the oldest
	/// cached video keyframe; emitting that lead audio first would bury the parameter
	/// sets behind an audio-only preamble, and a live decoder probing the stream gives
	/// up before it ever configures video. `None` until the tables are built, and for
	/// programs with no video track (nothing to align to).
	video_start: Option<Timestamp>,
}

struct Track {
	source: ExportSource,
	pending: Option<Frame>,
	finished: bool,
	pid: u16,
	kind: Kind,
	/// PMT ES-level descriptors to re-announce, captured verbatim on import (language,
	/// registration, ...). Empty for non-TS sources; AC-3/E-AC-3 then synthesize one.
	descriptors: Vec<catalog::Descriptor>,
	/// Last decode timestamp (continuous 90 kHz ticks) authored for this track, keeping the
	/// decode clock monotonic across reordered (B-frame) video. Only video uses it.
	last_dts: Option<u64>,
	/// Decode-clock reserve (90 kHz ticks): how far ahead of its PTS each frame decodes. Taken
	/// from the catalog `jitter` (the reorder depth) so it is large enough for `DTS <= PTS`,
	/// or [`DEFAULT_DTS_RESERVE`] when the catalog declares none. Only video uses it.
	dts_reserve: u64,
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
	/// Opus (private stream_type 0x06). Each frame is one Opus packet, prefixed with
	/// the Opus-in-TS access-unit control header and announced with the 'Opus'
	/// registration plus DVB extension descriptor.
	Opus { channel_count: u32 },
	/// MP2, carried verbatim. The sample rate picks the stream type on the way
	/// out (0x03 vs 0x04).
	Mp2 { sample_rate: u32 },
	/// AC-3 (ATSC stream_type 0x81), carried verbatim.
	Ac3,
	/// E-AC-3 (ATSC stream_type 0x87), carried verbatim.
	Eac3,
	/// An undecoded elementary stream carried verbatim (SCTE-35, private PES,
	/// teletext, ...). Re-announced in the PMT with its recorded `stream_type` and
	/// repacketized per its `framing`. `stream_id` is the original PES stream_id to
	/// re-emit (PES framing only; `None` falls back to `private_stream_1`).
	Verbatim {
		stream_type: u8,
		framing: catalog::Framing,
		stream_id: Option<u8>,
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
	timestamp: Timestamp,
	/// Authored decode timestamp for a reordered (B-frame) video frame, in continuous
	/// (unwrapped) 90 kHz ticks (wrapped to the wire field in `write_pes`). `Some` only when
	/// it differs from the PTS; the PES then carries both PTS and DTS.
	dts: Option<u64>,
	/// Explicit PES stream_id (verbatim PES); `None` derives it from `is_video`.
	stream_id: Option<u8>,
}

impl Export {
	/// Subscribe to `broadcast`, using the default catalog format.
	pub async fn new(broadcast: moq_net::BroadcastConsumer) -> Result<Self, crate::Error> {
		Self::with_catalog_format(broadcast, CatalogFormat::default()).await
	}

	/// Subscribe to `broadcast`, selecting an explicit catalog format. Media only;
	/// any catalog extension (e.g. the `mpegts` verbatim streams) is ignored.
	pub async fn with_catalog_format(
		broadcast: moq_net::BroadcastConsumer,
		catalog_format: CatalogFormat,
	) -> Result<Self, crate::Error> {
		Self::build(broadcast, catalog_format).await
	}
}

impl Export<catalog::Ext> {
	/// Subscribe to `broadcast`, exporting its `mpegts` verbatim streams (SCTE-35,
	/// private data, ...) back to MPEG-TS alongside the media. The `Self` type pins
	/// the extension, so callers write `Export::with_ts(..)` with no turbofish (the
	/// plain constructors are media-only).
	pub async fn with_ts(
		broadcast: moq_net::BroadcastConsumer,
		catalog_format: CatalogFormat,
	) -> Result<Self, crate::Error> {
		Self::build(broadcast, catalog_format).await
	}
}

impl<E: catalog::Catalog> Export<E> {
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
			program_descriptors: Vec::new(),
			psi: None,
			last_psi: None,
			video_start: None,
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
		let video_start = self.video_start;
		for track in self.tracks.values_mut() {
			if track.pending.is_some() || track.finished {
				continue;
			}
			let is_video = matches!(track.kind, Kind::Video(_));
			loop {
				match track.source.poll_read(waiter) {
					Poll::Ready(Ok(Some(frame))) => {
						if waiting_for_header && !track.source.header_ready() {
							continue;
						}
						// Tune-in alignment: drop non-video frames before the first video
						// keyframe (see `video_start`) so the in-band SPS/PPS leads the stream.
						if let Some(start) = video_start
							&& !is_video && frame.timestamp < start
						{
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
			if !self.header_ready() || !self.video_ready() {
				// Hold all output (tables and audio alike) until codec configs resolve
				// and, when the program has a video rendition, its first keyframe is
				// buffered: the stream must begin on that keyframe so the in-band
				// parameter sets lead it. An audio-only program has nothing to wait for.
				// If every track finished without producing a config, it can't be muxed.
				if self.catalog.is_none() && self.tracks.values().all(|t| t.finished) {
					return Poll::Ready(Ok(None));
				}
				return Poll::Pending;
			}
			self.build_psi()?;
			// Anchor tune-in to the first video keyframe and drop any non-video frame
			// already buffered ahead of it (see `video_start`).
			self.video_start = self.first_video_pts();
			if let Some(start) = self.video_start {
				for track in self.tracks.values_mut() {
					if !matches!(track.kind, Kind::Video(_))
						&& track.pending.as_ref().is_some_and(|f| f.timestamp < start)
					{
						track.pending = None;
					}
				}
			}
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
		// The MPEG-TS section lives in the extension. The trait only exposes
		// `mpegts_mut`, and this snapshot is owned, so clone it out (`()` yields the
		// empty default: no verbatim streams, no preserved PIDs/descriptors).
		let mpegts = catalog.mpegts_mut().cloned().unwrap_or_default();
		self.program_descriptors = mpegts.program_descriptors.clone();

		// The desired track set: media renditions plus the verbatim streams.
		let mut active: HashMap<String, ()> = HashMap::new();
		for name in catalog.video.renditions.keys() {
			active.insert(name.clone(), ());
		}
		for name in catalog.audio.renditions.keys() {
			active.insert(name.clone(), ());
		}
		for (name, track) in mpegts.tracks.iter() {
			if track.verbatim.is_some() {
				active.insert(name.clone(), ());
			}
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

		// Assign a PID to every desired track: prefer the original recorded in the
		// `mpegts` section, then fill the rest from FIRST_ES_PID. The importer fills
		// PIDs, descriptors, and stream_ids across several catalog publishes, so this
		// runs every snapshot until the PMT is built and the tracks below are
		// *refreshed*, not latched from the first (partial) snapshot.
		let mut used: Vec<u16> = vec![0x0000, PMT_PID, 0x1FFF];
		let mut pids: HashMap<String, u16> = HashMap::new();
		for name in active.keys() {
			if let Some(pid) = mpegts.tracks.get(name).map(|t| t.pid)
				&& !used.contains(&pid)
			{
				used.push(pid);
				pids.insert(name.clone(), pid);
			}
		}
		for name in active.keys() {
			if !pids.contains_key(name) {
				let mut pid = FIRST_ES_PID;
				while used.contains(&pid) {
					pid += 1;
				}
				used.push(pid);
				pids.insert(name.clone(), pid);
			}
		}

		// Reuse each track's existing source (and any pending frame) by name; refresh
		// its PID, kind, and descriptors from this snapshot. Drop tracks no longer present.
		let mut old = std::mem::take(&mut self.tracks);
		for (name, config) in catalog.video.renditions.iter() {
			let kind = video_kind(config, name)?;
			let descriptors = track_descriptors(&mpegts, name);
			let pid = pids[name];
			// The catalog `jitter` carries the reorder depth (max PTS - DTS), so use it as the
			// decode-clock reserve; it may arrive in a later snapshot, so refresh it each time.
			let reserve = dts_reserve(config);
			match old.remove(name) {
				Some(mut track) => {
					track.pid = pid;
					track.kind = kind;
					track.descriptors = descriptors;
					track.dts_reserve = reserve;
					self.tracks.insert(name.clone(), track);
				}
				None => {
					let source = ExportSource::for_video(&self.broadcast, name, config, self.latency)?;
					self.insert_track(name, source, pid, kind, descriptors, reserve);
				}
			}
		}
		for (name, config) in catalog.audio.renditions.iter() {
			let kind = audio_kind(config, name)?;
			let descriptors = track_descriptors(&mpegts, name);
			let pid = pids[name];
			match old.remove(name) {
				Some(mut track) => {
					track.pid = pid;
					track.kind = kind;
					track.descriptors = descriptors;
					self.tracks.insert(name.clone(), track);
				}
				None => {
					let source = ExportSource::for_audio(&self.broadcast, name, config, self.latency)?;
					self.insert_track(name, source, pid, kind, descriptors, DEFAULT_DTS_RESERVE);
				}
			}
		}
		for (name, track) in mpegts.tracks.iter() {
			let Some(verbatim) = &track.verbatim else {
				continue;
			};
			let kind = Kind::Verbatim {
				stream_type: verbatim.stream_type,
				framing: verbatim.framing,
				stream_id: verbatim.stream_id,
			};
			let descriptors = track.descriptors.clone();
			let pid = pids[name];
			match old.remove(name) {
				Some(mut existing) => {
					existing.pid = pid;
					existing.kind = kind;
					existing.descriptors = descriptors;
					self.tracks.insert(name.clone(), existing);
				}
				None => {
					let source = ExportSource::for_stream(&self.broadcast, name, self.latency)?;
					self.insert_track(name, source, pid, kind, descriptors, DEFAULT_DTS_RESERVE);
				}
			}
		}
		Ok(())
	}

	/// Insert a freshly created export track.
	fn insert_track(
		&mut self,
		name: &str,
		source: ExportSource,
		pid: u16,
		kind: Kind,
		descriptors: Vec<catalog::Descriptor>,
		dts_reserve: u64,
	) {
		self.tracks.insert(
			name.to_string(),
			Track {
				source,
				pending: None,
				finished: false,
				pid,
				kind,
				descriptors,
				last_dts: None,
				dts_reserve,
			},
		);
	}

	/// Header is ready when every track's [`ExportSource`] has resolved its
	/// codec config (from the catalog `description`, or built by the transform).
	fn header_ready(&self) -> bool {
		self.tracks.values().all(|t| t.source.header_ready())
	}

	/// Every video track has buffered its first frame (the keyframe) or finished.
	/// The tables wait for this so the tune-in point ([`Self::video_start`]) can be
	/// read from the keyframe before any audio is emitted ahead of it. A program
	/// with no video track is trivially ready.
	fn video_ready(&self) -> bool {
		self.tracks
			.values()
			.filter(|t| matches!(t.kind, Kind::Video(_)))
			.all(|t| t.pending.is_some() || t.finished)
	}

	/// The smallest timestamp among the video tracks' buffered frames: the first
	/// video keyframe, since pre-keyframe video frames are dropped before the tables
	/// are built. `None` when no video track has a buffered frame (audio-only program).
	fn first_video_pts(&self) -> Option<Timestamp> {
		self.tracks
			.values()
			.filter(|t| matches!(t.kind, Kind::Video(_)))
			.filter_map(|t| t.pending.as_ref().map(|f| f.timestamp))
			.min()
	}

	/// Build the PAT/PMT once every track's PID and codec is known.
	fn build_psi(&mut self) -> anyhow::Result<()> {
		// Order tracks by PID for a stable layout; first video track carries the PCR.
		let mut tracks: Vec<&Track> = self.tracks.values().collect();
		tracks.sort_by_key(|t| t.pid);

		// Section-framed verbatim streams (SCTE-35, ...) are stamped on the video clock
		// and carry no PTS for the PCR, so they need a video track; audio alone would
		// leave them pinned to zero.
		let needs_clock = tracks.iter().any(|t| {
			matches!(
				&t.kind,
				Kind::Verbatim {
					framing: catalog::Framing::Section,
					..
				}
			)
		});
		let video = tracks.iter().find(|t| matches!(t.kind, Kind::Video(_)));
		anyhow::ensure!(
			!needs_clock || video.is_some(),
			"TS export of section-framed verbatim streams (e.g. SCTE-35) requires a video track for the program clock"
		);
		let pcr_pid = video
			.or_else(|| {
				tracks.iter().find(|t| {
					matches!(
						t.kind,
						Kind::Aac { .. } | Kind::Opus { .. } | Kind::Mp2 { .. } | Kind::Ac3 | Kind::Eac3
					)
				})
			})
			.map(|t| t.pid)
			.context("TS export requires a video or audio track for the PCR")?;

		let es_info = tracks
			.iter()
			.map(|t| {
				let stream_type = match &t.kind {
					Kind::Video(stream_type) => *stream_type,
					Kind::Aac { .. } => StreamType::AdtsAac,
					// Opus rides private-data PES; the registration + extension descriptors
					// below tell the demuxer it's Opus.
					Kind::Opus { .. } => StreamType::from_u8(0x06).map_err(anyhow::Error::msg)?,
					// Half-rate MPEG-2 BC audio (< 32 kHz) re-announces as 0x04; the full
					// rates are MPEG-1 (0x03). The catalog sample rate came from the frame
					// header, so the mapping is faithful.
					Kind::Mp2 { sample_rate } if *sample_rate < 32000 => StreamType::Mpeg2HalvedSampleRateAudio,
					Kind::Mp2 { .. } => StreamType::Mpeg1Audio,
					Kind::Ac3 => StreamType::DolbyDigitalUpToSixChannelAudio,
					Kind::Eac3 => StreamType::DolbyDigitalPlusUpTo16ChannelAudioForAtsc,
					Kind::Verbatim { stream_type, .. } => {
						StreamType::from_u8(*stream_type).map_err(anyhow::Error::msg)?
					}
				};
				// Prefer the descriptors captured verbatim on import; otherwise synthesize
				// the ATSC Dolby registration so a fresh (non-TS) AC-3/E-AC-3 track is
				// still announced the way the import path expects.
				let descriptors = if !t.descriptors.is_empty() {
					to_pmt_descriptors(&t.descriptors)
				} else {
					match &t.kind {
						Kind::Ac3 => vec![Descriptor {
							tag: 0x05,
							data: b"AC-3".to_vec(),
						}],
						Kind::Eac3 => vec![Descriptor {
							tag: 0x05,
							data: b"EAC3".to_vec(),
						}],
						Kind::Opus { channel_count } => opus_descriptors(*channel_count),
						_ => Vec::new(),
					}
				};
				Ok(EsInfo {
					stream_type,
					elementary_pid: Pid::new(t.pid)?,
					descriptors,
				})
			})
			.collect::<anyhow::Result<Vec<_>>>()?;

		// Re-emit the captured program-level descriptors. With none (a non-TS source),
		// derive the SCTE-35 'CUEI' registration when a 0x86 verbatim stream is present.
		let program_info = if !self.program_descriptors.is_empty() {
			to_pmt_descriptors(&self.program_descriptors)
		} else if tracks.iter().any(|t| {
			// Only derive CUEI for section-framed 0x86 (SCTE-35); a PES-framed 0x86
			// (e.g. DTS audio) must not advertise SCTE-35 section signaling.
			matches!(
				&t.kind,
				Kind::Verbatim {
					stream_type: 0x86,
					framing: catalog::Framing::Section,
					..
				}
			)
		}) {
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
		// resolved avcC/hvcC to rewrite length-prefixed NALs as Annex-B. Section-framed
		// verbatim streams carry no PES payload; the section is written separately below.
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
			// Each moq Opus frame is one packet; prefix the Opus-in-TS control header.
			Kind::Opus { .. } => Some(opus_es_payload(&frame.payload)),
			// Legacy audio frames were ingested whole (framing header included), so
			// they pass through untouched. PES-framed verbatim payloads likewise.
			Kind::Mp2 { .. } | Kind::Ac3 | Kind::Eac3 => Some(frame.payload.to_vec()),
			Kind::Verbatim {
				framing: catalog::Framing::Pes,
				..
			} => Some(frame.payload.to_vec()),
			Kind::Verbatim {
				framing: catalog::Framing::Section,
				..
			} => None,
		};

		// Author a monotonic decode timeline for reordered video (B-frames). Other kinds
		// never reorder, so DTS == PTS and the PES stays PTS-only.
		let dts = if is_video {
			let pts = to_ticks(frame.timestamp);
			let track = self.tracks.get_mut(name).context("missing track")?;
			author_dts(pts, track.dts_reserve, &mut track.last_dts)
		} else {
			None
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
			// Section-framed verbatim (SCTE-35, ...) rides in private sections, not PES;
			// carry the bytes verbatim.
			None => self.write_section(&mut out, pid, &frame.payload)?,
			Some(es_payload) => {
				// Verbatim PES re-emits its original stream_id (falling back to
				// private_stream_1 for an undecoded stream with none recorded); media
				// derives it from is_video.
				let stream_id = match &kind {
					Kind::Verbatim { stream_id, .. } => Some(stream_id.unwrap_or(StreamId::PRIVATE_STREAM_1)),
					// Opus is private-data PES, carried under private_stream_1 like ffmpeg.
					Kind::Opus { .. } => Some(StreamId::PRIVATE_STREAM_1),
					_ => None,
				};
				let unit = PesUnit {
					pid,
					is_pcr,
					is_video,
					keyframe: frame.keyframe,
					timestamp: frame.timestamp,
					dts,
					stream_id,
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
		// A reordered video frame carries DTS alongside PTS; else PTS-only. The decode clock
		// is continuous ticks, so wrap into the 33-bit wire field here, like the PTS.
		let dts = unit
			.dts
			.map(|t| TsTimestamp::new(t & TS_TIMESTAMP_MASK).map_err(anyhow::Error::msg))
			.transpose()?;
		let stream_id = match unit.stream_id {
			Some(id) => StreamId::new(id),
			None if unit.is_video => StreamId::new(StreamId::VIDEO_MIN),
			None => StreamId::new(StreamId::AUDIO_MIN),
		};
		let header = mpeg2ts::pes::PesHeader {
			stream_id,
			priority: false,
			data_alignment_indicator: true,
			copyright: false,
			original_or_copy: false,
			pts: Some(pts),
			dts,
			escr: None,
		};

		// The optional PES header grows by 5 bytes when it also carries a DTS.
		let optional_len = PES_OPTIONAL_LEN + if dts.is_some() { PES_DTS_LEN } else { 0 };

		// `pes_packet_len` counts the optional header plus the payload (not the
		// 6-byte fixed prefix). Unbounded for video (0); bounded for audio when
		// it fits a u16.
		let pes_packet_len = if unit.is_video {
			0
		} else {
			u16::try_from(optional_len + payload.len()).unwrap_or(0)
		};

		// PCR follows the decode clock, so a B-frame stream advertises DTS (not PTS) here.
		let pcr = dts.unwrap_or(pts);

		let mut offset = 0;
		let mut first = true;
		loop {
			let adaptation = if first && (unit.is_pcr || unit.keyframe) {
				Some(AdaptationField {
					discontinuity_indicator: false,
					random_access_indicator: unit.keyframe,
					es_priority_indicator: false,
					pcr: if unit.is_pcr { Some(pcr.into()) } else { None },
					opcr: None,
					splice_countdown: None,
					transport_private_data: Vec::new(),
					extension: None,
				})
			} else {
				None
			};

			let header_len = if first { 6 + optional_len } else { 0 };
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

	/// Packetize a private section (SCTE-35 or other) verbatim. The first packet
	/// carries the pointer_field plus the section start as a `Section` payload (sets
	/// the unit-start bit so the receiver finds the pointer_field); continuations are
	/// `Raw`. The section bytes are opaque, so this round-trips byte-for-byte.
	fn write_section(&mut self, out: &mut Vec<u8>, pid: u16, section: &[u8]) -> anyhow::Result<()> {
		// The verbatim track is public; a non-importer producer could publish a frame
		// that isn't a complete section. Drop it (with a warning) rather than emit a
		// malformed section a downstream demuxer would choke on. One bad section must
		// not abort a live export, so this skips instead of erroring.
		if !is_complete_section(section) {
			tracing::warn!(pid, len = section.len(), "dropping malformed private section on export");
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
/// Extra bytes when the optional region also carries a DTS (5 DTS bytes).
const PES_DTS_LEN: usize = 5;
/// Fallback decode-clock reserve in 90 kHz ticks when the catalog declares no `jitter`. At
/// 16 ticks (~0.18 ms) it is just a strict-monotonic nudge: it keeps DTS strictly increasing
/// across reordered (B-frame) decode order (the `ffplay -fflags +igndts` fix) but does not
/// keep `DTS <= PTS`. When the catalog carries `jitter` (the reorder depth, populated on
/// import), the track uses that instead, which is large enough to keep `DTS <= PTS`. See
/// [`author_dts`] and [`Track::dts_reserve`].
const DEFAULT_DTS_RESERVE: u64 = 16;

fn psi_interval() -> Timestamp {
	Timestamp::try_from(PSI_INTERVAL).unwrap_or(Timestamp::ZERO)
}

/// External byte size of an adaptation field (manual mirror of the crate's
/// private `external_size`); only PCR is ever set.
fn adaptation_size(af: &AdaptationField) -> usize {
	2 + if af.pcr.is_some() { 6 } else { 0 }
}

/// The 33-bit wire timestamp field (90 kHz). DTS and PTS both wrap into it.
const TS_TIMESTAMP_MASK: u64 = (1 << 33) - 1;

/// Continuous (unwrapped) 90 kHz tick count for a media timestamp. The decode clock runs in
/// this domain so it never wraps mid-stream (the source timestamps are already unwrapped);
/// [`to_ts_timestamp`] masks to the 33-bit wire field only at emission.
fn to_ticks(timestamp: Timestamp) -> u64 {
	(timestamp.as_micros() * 90_000 / 1_000_000) as u64
}

fn to_ts_timestamp(timestamp: Timestamp) -> anyhow::Result<TsTimestamp> {
	// Continuous 90 kHz ticks, wrapped into the 33-bit field.
	TsTimestamp::new(to_ticks(timestamp) & TS_TIMESTAMP_MASK).map_err(anyhow::Error::msg)
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
		AudioCodec::Opus => Ok(Kind::Opus {
			channel_count: config.channel_count,
		}),
		AudioCodec::Ac3 => Ok(Kind::Ac3),
		AudioCodec::Ec3 => Ok(Kind::Eac3),
		other => anyhow::bail!("TS export does not support audio codec {other:?} (track '{name}')"),
	}
}

/// The two PMT descriptors for an Opus elementary stream: the `Opus` registration
/// descriptor (which sets the codec) and the DVB extension descriptor 0x80 carrying
/// the channel configuration. ffmpeg's demuxer requires both to recognize the stream.
fn opus_descriptors(channel_count: u32) -> Vec<Descriptor> {
	vec![
		Descriptor {
			tag: 0x05,
			data: b"Opus".to_vec(),
		},
		Descriptor {
			tag: 0x7f,
			// extension_descriptor_tag 0x80, then channel_config_code (1=mono, 2=stereo,
			// = channel count for the Vorbis mapping), clamped to the 1..=8 the demuxer reads.
			data: vec![0x80, channel_count.clamp(1, 8) as u8],
		},
	]
}

/// Wrap a raw Opus packet in the Opus-in-TS access-unit control header, producing one
/// PES access unit. Emits the 11-bit `0x3FF` sync (no trim, no control extension), then
/// the `0xFF`-run `au_size`, then the packet.
fn opus_es_payload(packet: &[u8]) -> Vec<u8> {
	let mut out = Vec::with_capacity(packet.len() + 4);
	// Sync 0x3FF over 11 bits: all of byte 0 (0x7F) plus the top 3 bits of byte 1. The
	// low 5 bits of byte 1 are the start-trim/end-trim/control-extension flags, all clear.
	out.push(0x7f);
	out.push(0xe0);
	// au_size: a run of 0xFF bytes summing toward the size, then a final byte < 0xFF. A
	// size that is an exact multiple of 255 still emits a terminating 0x00 byte.
	let mut n = packet.len();
	loop {
		out.push(n.min(255) as u8);
		if n < 255 {
			break;
		}
		n -= 255;
	}
	out.extend_from_slice(packet);
	out
}

/// The PMT descriptors recorded for `name` in the `mpegts` section, if any.
fn track_descriptors(mpegts: &catalog::Mpegts, name: &str) -> Vec<catalog::Descriptor> {
	mpegts
		.tracks
		.get(name)
		.map(|t| t.descriptors.clone())
		.unwrap_or_default()
}

/// Convert catalog descriptors (base64 bytes) to mpeg2ts PMT descriptors.
fn to_pmt_descriptors(descriptors: &[catalog::Descriptor]) -> Vec<Descriptor> {
	descriptors
		.iter()
		.map(|d| Descriptor {
			tag: d.tag,
			data: d.data.to_vec(),
		})
		.collect()
}

/// One section-framed verbatim frame must be exactly one section: at least the
/// 3-byte header and a total length matching the declared section_length.
/// Structural only (no table semantics); the bytes are still carried verbatim.
fn is_complete_section(section: &[u8]) -> bool {
	section.len() >= 3 && section.len() == 3 + ((((section[1] & 0x0f) as usize) << 8) | section[2] as usize)
}

fn ensure_raw(container: &Container, kind: &str, name: &str) -> anyhow::Result<()> {
	match container {
		// TS carries raw codec payloads, like the Legacy varint and LOC formats.
		Container::Legacy | Container::Loc => Ok(()),
		Container::Cmaf { .. } => anyhow::bail!("TS export does not support CMAF {kind} track '{name}'"),
	}
}

/// Author a monotonic decode timestamp (DTS) for a reordered (B-frame) video frame.
///
/// [`Frame`] carries only a presentation timestamp (PTS) and frames reach the muxer in
/// decode order (MoQ groups and frames are delivered in decode order), so a B-frame stream
/// arrives with valid but non-monotonic PTS and no decode time. MPEG-TS players need a
/// monotonic DTS to schedule decoding; without it they choke on the out-of-order PTS (the
/// `ffplay -fflags +igndts` workaround).
///
/// Since decode order is already the delivery order, the only job is to keep DTS strictly
/// increasing. The clock runs [`DTS_RESERVE`] ticks behind the PTS and never goes backwards:
/// a reordered frame whose PTS dips below the clock is nudged one tick past the last DTS. With
/// the small reserve this keeps DTS monotonic but lets it sit above a B-frame's own PTS; a
/// frame-scale reserve (or the faithful wire DTS) would be needed for `DTS <= PTS`.
///
/// `reserve` is how far behind the PTS to run the clock (the catalog reorder depth, or the
/// fallback). `pts` and `last` are continuous (unwrapped) 90 kHz ticks, so the clock never
/// wraps mid-stream; the 33-bit wire wrap happens once at emission in [`write_pes`]. `last` is
/// the previous DTS, updated in place. Returns `None` when the DTS equals the PTS (PES stays
/// PTS-only).
fn author_dts(pts: u64, reserve: u64, last: &mut Option<u64>) -> Option<u64> {
	let mut dts = pts.saturating_sub(reserve);
	if let Some(prev) = *last
		&& dts <= prev
	{
		dts = prev + 1;
	}
	*last = Some(dts);
	(dts != pts).then_some(dts)
}

/// The decode-clock reserve for a video rendition: its catalog `jitter` (the reorder depth)
/// in 90 kHz ticks, or [`DEFAULT_DTS_RESERVE`] when none is declared.
fn dts_reserve(config: &VideoConfig) -> u64 {
	config
		.jitter
		.map(|t| (t.as_micros() * 90_000 / 1_000_000) as u64)
		.filter(|&ticks| ticks > 0)
		.unwrap_or(DEFAULT_DTS_RESERVE)
}

#[cfg(test)]
mod tests {
	use super::{DEFAULT_DTS_RESERVE, author_dts, is_complete_section};

	/// Push a decode-order PTS stream (90 kHz) through the decode clock with a given reserve and
	/// return the effective DTS per frame (the authored DTS, or the PTS when none is authored).
	fn run_clock(pts: &[u64], reserve: u64) -> Vec<u64> {
		let mut last = None;
		pts.iter()
			.map(|&p| author_dts(p, reserve, &mut last).unwrap_or(p))
			.collect()
	}

	/// Decode-order PTS for a constant-frame-rate display timeline with `b` B-frames between
	/// each pair of reference frames (the common broadcast structure: references pulled ahead
	/// of the B-frames they predict). `base` keeps the timeline off zero, like a real feed's
	/// initial PTS offset.
	fn decode_order(refs: usize, b: usize, dur: u64, base: u64) -> Vec<u64> {
		let pts = |display: usize| base + display as u64 * dur;
		let span = b + 1;
		let mut out = vec![pts(0)]; // first reference (keyframe) at display 0
		for g in 1..refs {
			let reference = g * span;
			out.push(pts(reference)); // reference, decoded before its B-frames
			for j in 1..=b {
				out.push(pts(reference - span + j)); // the B-frames between the two references
			}
		}
		out
	}

	#[test]
	fn dts_is_monotonic_across_reorder() {
		// 25 fps, 10 s offset. Even with the tiny fallback reserve the decode timeline is
		// strictly increasing (the `+igndts` fix); it just may sit above PTS for B-frames.
		for b in [1, 3, 5] {
			let pts = decode_order(40, b, 3_600, 10_000_000);
			let dts = run_clock(&pts, DEFAULT_DTS_RESERVE);

			// The fixture genuinely reorders (PTS dips in decode order).
			assert!(pts.windows(2).any(|w| w[1] < w[0]), "b={b}: stream must reorder PTS");
			for (i, win) in dts.windows(2).enumerate() {
				assert!(win[1] > win[0], "b={b}: DTS not strictly increasing at {i}: {win:?}");
			}
		}
	}

	#[test]
	fn sufficient_reserve_keeps_dts_under_pts() {
		// With a reserve covering the reorder span (the catalog `jitter` carries it), the decode
		// timeline is both strictly increasing and never after the PTS.
		let dur = 3_600;
		for b in [1, 3, 5] {
			let reserve = (b as u64 + 1) * dur; // one frame past the b-frame run
			let pts = decode_order(40, b, dur, 10_000_000);
			let dts = run_clock(&pts, reserve);

			for (i, win) in dts.windows(2).enumerate() {
				assert!(win[1] > win[0], "b={b}: DTS not strictly increasing at {i}: {win:?}");
			}
			for (i, (&d, &p)) in dts.iter().zip(pts.iter()).enumerate() {
				assert!(d <= p, "b={b}: DTS {d} after PTS {p} at {i}");
			}
		}
	}

	#[test]
	fn dts_clock_survives_33bit_wrap() {
		// The decode clock runs in continuous ticks, so it stays strictly increasing even as
		// the source timeline crosses the 33-bit wire boundary (~26.5 h). The wrap is applied
		// only at emission, so here the authored DTS keeps climbing past 1 << 33.
		let wrap = 1u64 << 33;
		let pts = decode_order(40, 3, 3_600, wrap - 20 * 3_600);
		let dts = run_clock(&pts, DEFAULT_DTS_RESERVE);

		assert!(pts.iter().any(|&p| p >= wrap), "test must cross the wrap boundary");
		for (i, win) in dts.windows(2).enumerate() {
			assert!(
				win[1] > win[0],
				"DTS not strictly increasing across wrap at {i}: {win:?}"
			);
		}
	}

	#[test]
	fn dts_without_reorder_trails_pts_by_the_reserve() {
		// A monotonic (no-B) stream stays strictly increasing and one reserve under its PTS.
		let pts: Vec<u64> = (0..40).map(|i| 10_000_000 + i * 3_600).collect();
		let dts = run_clock(&pts, DEFAULT_DTS_RESERVE);

		for (i, win) in dts.windows(2).enumerate() {
			assert!(win[1] > win[0], "DTS not strictly increasing at {i}: {win:?}");
		}
		for (i, (&d, &p)) in dts.iter().zip(pts.iter()).enumerate() {
			assert_eq!(d, p - DEFAULT_DTS_RESERVE, "DTS should trail PTS by the reserve at {i}");
		}
	}

	#[test]
	fn section_validation() {
		// section_length 27 (0x1b) -> 30 bytes total.
		let mut ok = vec![0xfc, 0x30, 0x1b];
		ok.resize(30, 0x00);
		assert!(is_complete_section(&ok));
		// minimal: section_length 0 -> exactly the 3-byte header.
		assert!(is_complete_section(&[0xfc, 0x00, 0x00]));
		// any table_id is accepted (verbatim carriage isn't SCTE-specific).
		assert!(is_complete_section(&[0x00, 0x00, 0x00]));

		// shorter than the 3-byte header.
		assert!(!is_complete_section(&[0xfc, 0x00]));
		// declared section_length (27) does not match the actual length (3).
		assert!(!is_complete_section(&[0xfc, 0x30, 0x1b]));
	}
}
