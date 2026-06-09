//! MPEG-TS demuxer.
//!
//! [`Import`] reads a TS byte stream, reassembles PES packets per PID, and
//! routes their payloads to the existing codec importers (H.264/H.265/AAC),
//! which own their broadcast tracks and catalog entries. TS only adds PAT/PMT
//! discovery, PES reassembly, and the 90 kHz -> microsecond PTS conversion.

use std::collections::HashMap;
use std::io::Read;
use std::sync::{Arc, Mutex};

use bytes::{Buf, BytesMut};
use mpeg2ts::es::StreamType;
use mpeg2ts::pes::PesHeader;
use mpeg2ts::ts::payload::Pes;
use mpeg2ts::ts::{Pid, ReadTsPacket, TsPacket, TsPacketReader, TsPayload};
use tokio::io::{AsyncRead, AsyncReadExt};

use super::adts;
use crate::codec::{aac, h264, h265};
use crate::container::Timestamp;

/// Demuxes an MPEG-TS byte stream into a MoQ broadcast.
///
/// Supports H.264 (stream type 0x1B), H.265 (0x24), and ADTS AAC (0x0F). LATM/LOAS
/// AAC (0x11) is not ADTS-framed and is dropped. Other elementary streams are
/// logged and dropped. Each elementary stream is fed to its codec importer, which
/// manages the track, catalog config, and keyframe-based group boundaries.
pub struct Import {
	broadcast: moq_net::BroadcastProducer,
	catalog: crate::catalog::Producer,

	/// Shared, refillable byte source the persistent reader pulls whole packets
	/// from. Kept beside the reader so [`decode`](Self::decode) can append bytes
	/// between reads without recreating the reader (which holds PAT/PMT state).
	feed: Feed,
	reader: TsPacketReader<Feed>,

	/// Per elementary-stream-PID codec routing.
	streams: HashMap<Pid, Stream>,
	/// In-progress PES reassembly, keyed by elementary PID.
	pending: HashMap<Pid, Pending>,
	/// True once a PMT with at least one supported stream has been parsed.
	initialized: bool,
	/// Raw 90 kHz PTS of the first audio frame in the current consecutive run.
	/// TS muxes audio in clumps separated by video, so the span of one run sizes
	/// the audio catalog jitter (see [`AacStream::write`]). Reset on a video frame.
	audio_burst: Option<u64>,
}

impl Import {
	pub fn new(broadcast: moq_net::BroadcastProducer, catalog: crate::catalog::Producer) -> Self {
		let feed = Feed::default();
		Self {
			broadcast,
			catalog,
			reader: TsPacketReader::new(feed.clone()),
			feed,
			streams: HashMap::new(),
			pending: HashMap::new(),
			initialized: false,
			audio_burst: None,
		}
	}

	/// True once the stream layout (PMT) has been discovered.
	pub fn is_initialized(&self) -> bool {
		self.initialized
	}

	/// Decode from an asynchronous reader, driving [`Self::decode`] in a loop.
	pub async fn decode_from<T: AsyncRead + Unpin>(&mut self, reader: &mut T) -> anyhow::Result<()> {
		let mut chunk = BytesMut::with_capacity(64 * 1024);
		loop {
			chunk.clear();
			let n = reader.read_buf(&mut chunk).await?;
			if n == 0 {
				break;
			}
			self.decode(&mut chunk)?;
		}
		Ok(())
	}

	/// Append `buf` to the internal scratch and demux every whole TS packet it
	/// now completes. The buffer is fully consumed; a trailing partial packet
	/// (< 188 bytes) is retained for the next call.
	pub fn decode<T: Buf + AsRef<[u8]>>(&mut self, buf: &mut T) -> anyhow::Result<()> {
		{
			let mut state = self.feed.lock();
			while buf.has_remaining() {
				let chunk = buf.chunk();
				state.data.extend_from_slice(chunk);
				let len = chunk.len();
				buf.advance(len);
			}
			// Only expose whole packets to the reader; it stops cleanly at the boundary.
			state.limit = state.data.len() / TsPacket::SIZE * TsPacket::SIZE;
			state.pos = 0;
		}

		while let Some(packet) = self.reader.read_ts_packet()? {
			self.handle_packet(packet)?;
		}

		{
			let mut state = self.feed.lock();
			let consumed = state.limit;
			state.data.advance(consumed);
			state.pos = 0;
			state.limit = 0;
		}

		Ok(())
	}

	fn handle_packet(&mut self, packet: TsPacket) -> anyhow::Result<()> {
		let pid = packet.header.pid;
		match packet.payload {
			Some(TsPayload::Pmt(pmt)) => {
				for es in &pmt.es_info {
					self.ensure_stream(es.elementary_pid, es.stream_type)?;
				}
			}
			Some(TsPayload::PesStart(pes)) => self.handle_pes_start(pid, pes)?,
			Some(TsPayload::PesContinuation(bytes)) => self.handle_pes_continuation(pid, &bytes)?,
			// PAT routing is handled inside the reader; everything else is ignored.
			_ => {}
		}
		Ok(())
	}

	fn ensure_stream(&mut self, pid: Pid, stream_type: StreamType) -> anyhow::Result<()> {
		if self.streams.contains_key(&pid) {
			return Ok(());
		}

		let stream = match stream_type {
			StreamType::H264 => {
				let import =
					h264::Import::new(self.broadcast.clone(), self.catalog.clone()).with_mode(h264::Mode::Avc3)?;
				Stream::H264 {
					import: Box::new(import),
					unwrap: PtsUnwrap::default(),
				}
			}
			StreamType::H265 => Stream::H265 {
				import: Box::new(h265::Import::new(self.broadcast.clone(), self.catalog.clone())),
				unwrap: PtsUnwrap::default(),
			},
			// Only ADTS-framed AAC (0x0F). 0x11 is LATM/LOAS, which uses a different
			// framing and syncword, so it falls through to the ignored arm below.
			StreamType::AdtsAac => Stream::Aac(Box::new(AacStream {
				import: None,
				broadcast: self.broadcast.clone(),
				catalog: self.catalog.clone(),
				unwrap: PtsUnwrap::default(),
				jitter: None,
			})),
			other => {
				tracing::warn!(?other, pid = pid.as_u16(), "unsupported TS stream type, dropping");
				Stream::Ignored
			}
		};

		if !matches!(stream, Stream::Ignored) {
			self.initialized = true;
		}
		self.streams.insert(pid, stream);
		Ok(())
	}

	fn handle_pes_start(&mut self, pid: Pid, pes: Pes) -> anyhow::Result<()> {
		// A new PES start means the previous one for this PID is complete.
		if self.pending.contains_key(&pid) {
			self.flush(pid)?;
		}

		let Some(stream) = self.streams.get(&pid) else {
			// PES before its PMT entry; ignore until the layout is known.
			return Ok(());
		};

		// A video PES arriving marks the end of any preceding audio run; audio is
		// muxed into the gaps between video frames. Resetting here (on delivery)
		// rather than only on the video flush avoids over-counting the startup run,
		// since unbounded video PES don't flush until the next one starts.
		if matches!(stream, Stream::H264 { .. } | Stream::H265 { .. }) {
			self.audio_burst = None;
		}

		let data_len = pes_data_len(&pes.header, pes.pes_packet_len);
		let mut pending = Pending {
			pts: pes.header.pts.map(|t| t.as_u64()),
			data: Vec::with_capacity(pes.data.len()),
			data_len,
		};
		pending.data.extend_from_slice(&pes.data);
		let complete = matches!(data_len, Some(len) if pending.data.len() >= len);
		self.pending.insert(pid, pending);

		if complete {
			self.flush(pid)?;
		}
		Ok(())
	}

	fn handle_pes_continuation(&mut self, pid: Pid, data: &[u8]) -> anyhow::Result<()> {
		let Some(pending) = self.pending.get_mut(&pid) else {
			return Ok(());
		};
		pending.data.extend_from_slice(data);
		if matches!(pending.data_len, Some(len) if pending.data.len() >= len) {
			self.flush(pid)?;
		}
		Ok(())
	}

	fn flush(&mut self, pid: Pid) -> anyhow::Result<()> {
		let Some(pending) = self.pending.remove(&pid) else {
			return Ok(());
		};

		// Track the start of the current consecutive audio run (audio PTS since the
		// last video frame), so the audio stream can size its jitter to the burst.
		let is_video = matches!(self.streams.get(&pid), Some(Stream::H264 { .. } | Stream::H265 { .. }));
		let run_start = if is_video {
			self.audio_burst = None;
			None
		} else if let Some(audio) = pending.pts {
			Some(*self.audio_burst.get_or_insert(audio))
		} else {
			None
		};

		let Some(stream) = self.streams.get_mut(&pid) else {
			return Ok(());
		};
		stream.write(pending, run_start)
	}

	/// Close the current group on every track and reopen at `sequence`.
	pub fn seek(&mut self, sequence: u64) -> anyhow::Result<()> {
		for stream in self.streams.values_mut() {
			stream.seek(sequence)?;
		}
		Ok(())
	}

	/// Flush any buffered PES and finish every track.
	pub fn finish(&mut self) -> anyhow::Result<()> {
		let pids: Vec<Pid> = self.pending.keys().copied().collect();
		for pid in pids {
			self.flush(pid)?;
		}
		for stream in self.streams.values_mut() {
			stream.finish()?;
		}
		Ok(())
	}
}

/// A reassembled PES packet awaiting routing to its codec importer.
struct Pending {
	/// Raw 90 kHz PTS, before wrap-unwrapping.
	pts: Option<u64>,
	data: Vec<u8>,
	/// Expected payload length for bounded PES, else `None` (unbounded video).
	data_len: Option<usize>,
}

/// One elementary stream's codec importer plus PTS-unwrap state.
enum Stream {
	H264 {
		import: Box<h264::Import>,
		unwrap: PtsUnwrap,
	},
	H265 {
		import: Box<h265::Import>,
		unwrap: PtsUnwrap,
	},
	Aac(Box<AacStream>),
	Ignored,
}

impl Stream {
	fn write(&mut self, pending: Pending, burst: Option<u64>) -> anyhow::Result<()> {
		match self {
			Stream::H264 { import, unwrap } => {
				let pts = unwrap_pts(unwrap, pending.pts)?;
				import.decode_frame(&mut pending.data.as_slice(), pts)
			}
			Stream::H265 { import, unwrap } => {
				let pts = unwrap_pts(unwrap, pending.pts)?;
				import.decode_frame(&mut pending.data.as_slice(), pts)
			}
			Stream::Aac(stream) => stream.write(pending, burst),
			Stream::Ignored => Ok(()),
		}
	}

	fn seek(&mut self, sequence: u64) -> anyhow::Result<()> {
		match self {
			Stream::H264 { import, .. } => import.seek(sequence),
			Stream::H265 { import, .. } => import.seek(sequence),
			Stream::Aac(stream) => stream.seek(sequence),
			Stream::Ignored => Ok(()),
		}
	}

	fn finish(&mut self) -> anyhow::Result<()> {
		match self {
			Stream::H264 { import, .. } => import.finish(),
			Stream::H265 { import, .. } => import.finish(),
			Stream::Aac(stream) => stream.finish(),
			Stream::Ignored => Ok(()),
		}
	}
}

/// AAC needs the first ADTS header before it can build a [`aac::Import`]
/// (the sample rate and channel layout aren't in the PMT), so creation is
/// deferred until the first frame arrives.
struct AacStream {
	import: Option<aac::Import>,
	broadcast: moq_net::BroadcastProducer,
	catalog: crate::catalog::Producer,
	unwrap: PtsUnwrap,
	/// Largest audio burst span seen, published as the catalog jitter.
	jitter: Option<Timestamp>,
}

impl AacStream {
	fn write(&mut self, pending: Pending, run_start: Option<u64>) -> anyhow::Result<()> {
		let base = unwrap_pts(&mut self.unwrap, pending.pts)?;

		// A single PES can carry several ADTS frames; split and feed each raw frame.
		let data = &pending.data;
		let mut offset = 0;
		let mut index = 0u64;
		let mut sample_rate = None;
		while offset + 7 <= data.len() {
			let header = adts::Header::parse(&data[offset..])?;
			let end = offset + header.frame_len;
			anyhow::ensure!(end <= data.len(), "ADTS frame exceeds PES payload");
			sample_rate = Some(header.sample_rate);

			let import = match &mut self.import {
				Some(import) => import,
				None => {
					let config = aac::Config {
						profile: header.object_type,
						sample_rate: header.sample_rate,
						channel_count: header.channel_count,
					};
					// Synthesize the AudioSpecificConfig from the first ADTS header so
					// downstream consumers that need out-of-band config (fMP4/MKV export,
					// WebCodecs) can configure the decoder. TS itself carries it inline.
					let description = config.encode();
					let import = aac::Import::new(self.broadcast.clone(), self.catalog.clone(), config)?;
					let name = import.track().name.clone();
					if let Some(rendition) = self.catalog.lock().audio.renditions.get_mut(&name) {
						rendition.description = Some(description);
					}
					self.import.insert(import)
				}
			};

			// Each frame after the first in this PES advances by 1024 samples.
			let pts = match base {
				Some(base) if index > 0 => {
					let advance = Timestamp::from_scale(index * 1024, header.sample_rate as u64)?;
					Some(base + advance)
				}
				other => other,
			};

			let mut raw = &data[offset + header.header_len..end];
			import.decode(&mut raw, pts)?;

			offset = end;
			index += 1;
		}

		self.update_jitter(run_start, pending.pts, index, sample_rate)
	}

	/// Size the catalog jitter to the TS audio burst. MPEG-TS delivers audio in
	/// clumps (several ADTS frames per PES, and runs of audio PES between video)
	/// rather than one frame at a time, so without a matching jitter hint the
	/// player under-buffers audio and stutters between bursts. The burst is the
	/// PTS span from the start of the current audio run to this PES's last frame.
	fn update_jitter(
		&mut self,
		run_start: Option<u64>,
		pes_pts: Option<u64>,
		frames: u64,
		sample_rate: Option<u32>,
	) -> anyhow::Result<()> {
		let (Some(start), Some(pts), Some(rate)) = (run_start, pes_pts, sample_rate) else {
			return Ok(());
		};
		if frames == 0 {
			return Ok(());
		}

		let frame = 1024 * 90_000 / rate as u64;
		// Span from the run start through this PES's frames, plus one frame slack.
		let span = pts.saturating_sub(start) + frames * frame;
		// Ignore implausible spans (e.g. across a 33-bit PTS wrap).
		if span > 90_000 * 4 {
			return Ok(());
		}

		let jitter = Timestamp::from_scale(span, 90_000)?;
		if jitter <= self.jitter.unwrap_or(Timestamp::ZERO) {
			return Ok(());
		}
		self.jitter = Some(jitter);

		if let Some(import) = &self.import {
			let name = import.track().name.clone();
			if let Some(rendition) = self.catalog.lock().audio.renditions.get_mut(&name) {
				rendition.jitter = Some(jitter.convert()?);
			}
		}
		Ok(())
	}

	fn seek(&mut self, sequence: u64) -> anyhow::Result<()> {
		if let Some(import) = &mut self.import {
			import.seek(sequence)?;
		}
		Ok(())
	}

	fn finish(&mut self) -> anyhow::Result<()> {
		if let Some(import) = &mut self.import {
			import.finish()?;
		}
		Ok(())
	}
}

/// Replicates [`PesHeader::optional_header_len`] (which is crate-private) to
/// compute the declared PES payload length for bounded packets.
fn pes_data_len(header: &PesHeader, pes_packet_len: u16) -> Option<usize> {
	if pes_packet_len == 0 {
		// Unbounded (the usual case for video); flush on the next PES start.
		return None;
	}
	let optional = 3 + header.pts.map_or(0, |_| 5) + header.dts.map_or(0, |_| 5) + header.escr.map_or(0, |_| 6);
	pes_packet_len.checked_sub(optional).map(|n| n as usize)
}

/// Convert a raw 90 kHz PTS to a microsecond [`Timestamp`], unwrapping the
/// 33-bit field. Returns `None` when the PES carried no PTS (the codec layer
/// then falls back to a wall-clock timestamp).
fn unwrap_pts(unwrap: &mut PtsUnwrap, pts: Option<u64>) -> anyhow::Result<Option<Timestamp>> {
	let Some(raw) = pts else {
		return Ok(None);
	};
	let extended = unwrap.unwrap(raw);
	Ok(Some(Timestamp::from_scale(extended, 90_000)?))
}

/// Tracks the wrap-around of the 33-bit, 90 kHz PTS field so timestamps stay
/// monotonic across the ~26.5 hour wrap period.
#[derive(Default)]
struct PtsUnwrap {
	last: Option<u64>,
	offset: u64,
}

impl PtsUnwrap {
	fn unwrap(&mut self, raw: u64) -> u64 {
		const WRAP: u64 = 1 << 33;
		const HALF: i64 = (WRAP / 2) as i64;
		if let Some(last) = self.last {
			let diff = raw as i64 - last as i64;
			if diff < -HALF {
				self.offset += WRAP;
			} else if diff > HALF && self.offset >= WRAP {
				self.offset -= WRAP;
			}
		}
		self.last = Some(raw);
		self.offset + raw
	}
}

/// A cloneable, refillable [`Read`] source backed by a shared buffer.
///
/// The TS reader holds one clone and pulls whole packets; [`Import`] holds
/// another and appends bytes. It exposes only `[pos, limit)` so the reader
/// reaches a clean end-of-stream at each packet boundary, leaving any partial
/// trailing packet buffered for the next decode call.
#[derive(Clone, Default)]
struct Feed(Arc<Mutex<FeedState>>);

#[derive(Default)]
struct FeedState {
	data: BytesMut,
	pos: usize,
	limit: usize,
}

impl Feed {
	fn lock(&self) -> std::sync::MutexGuard<'_, FeedState> {
		self.0.lock().unwrap()
	}
}

impl Read for Feed {
	fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
		let mut state = self.lock();
		let n = out.len().min(state.limit - state.pos);
		if n == 0 {
			return Ok(0);
		}
		out[..n].copy_from_slice(&state.data[state.pos..state.pos + n]);
		state.pos += n;
		Ok(n)
	}
}
