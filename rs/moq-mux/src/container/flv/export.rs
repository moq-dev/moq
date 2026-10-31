//! FLV muxer.
//!
//! [`Export`] reads a catalog stream and the broadcast it describes, and produces
//! a single FLV byte stream: the file header, the video/audio sequence headers,
//! then one tag per media frame interleaved by timestamp. Legacy H.264 + AAC + MP3
//! are muxed as the classic CodecID tags; HEVC, AV1, VP9, Opus, AC-3, and E-AC-3
//! are muxed as the enhanced-RTMP (E-RTMP) FourCC payloads. Frames flow through
//! [`ExportSource`], which normalizes H.264/H.265 to length-prefixed NALU plus a
//! resolved avcC/hvcC (parsing inline avc3/hev1 parameter sets when needed) and
//! hands the other codecs through unchanged.
//!
//! Narrow renditions with [`Stream::select`](crate::catalog::Stream::select)
//! before construction. By default FLV carries a single video and a single audio
//! stream, so only the best video rendition (see [`Video::ranked`](hang::catalog::Video::ranked))
//! and the best audio rendition (see [`Audio::ranked`](hang::catalog::Audio::ranked))
//! the stream yields are muxed and the rest are ignored. Each pick follows the
//! catalog until the stream header goes out, then stays, following that rendition to another
//! track when the catalog moves it (under a new sequence-header tag when its codec config
//! record changes; a move back in time fails the export). With
//! [`with_multitrack`](Export::with_multitrack) every rendition the stream yields
//! is muxed instead, each as an enhanced-RTMP multitrack track addressed by its own
//! track id (use this only for a player that advertised the `Multitrack` capability).

use std::task::Poll;

use anyhow::Context;
use bytes::{BufMut, Bytes, BytesMut};
use hang::catalog::{AV1, AudioCodec, Catalog, Container, Reference, VideoCodec};

use super::{
	AAC_AUDIO_TAG_HEADER, AAC_RAW, AAC_SEQUENCE_HEADER, AUDIO_FORMAT_EX, AUDIO_PACKET_CODED_FRAMES,
	AUDIO_PACKET_MULTITRACK, AUDIO_PACKET_SEQUENCE_START, AVC_NALU, AVC_SEQUENCE_HEADER, FRAME_TYPE_INTER,
	FRAME_TYPE_KEY, MP3_AUDIO_TAG_HEADER, MULTITRACK_ONE_TRACK, TAG_AUDIO, TAG_HEADER_LEN, TAG_VIDEO, VIDEO_CODEC_AVC,
	VIDEO_EX_HEADER, VIDEO_PACKET_CODED_FRAMES, VIDEO_PACKET_MULTITRACK, VIDEO_PACKET_SEQUENCE_START,
};
use crate::catalog::Stream;
use crate::container::consumer::Event;
use crate::container::declared::DeclaredClock;
use crate::container::{ExportSource, Frame};

/// Before the header, unbind every track the catalog no longer lists (removed, or disabled) or
/// moved to another track, so the header declares only what the snapshot offers. A single-track
/// stream also follows the best rendition, so a snapshot that lacks it doesn't fix the pick.
/// Unbinding unsubscribes the track; nothing was emitted from it yet.
fn unbind<C: Reference>(
	tracks: &mut Vec<FlvTrack>,
	renditions: &[(&String, &C)],
	multitrack: bool,
	source: &crate::Source,
) {
	let bound = |track: &FlvTrack, (name, config): &(&String, &C)| {
		**name == track.name && track.source.reads(source, name, *config)
	};
	if multitrack {
		tracks.retain(|track| renditions.iter().any(|rendition| bound(track, rendition)));
	} else if !tracks
		.first()
		.zip(renditions.first())
		.is_some_and(|(track, rendition)| bound(track, rendition))
	{
		tracks.clear();
	}
	// Ids follow bind order, so close the gaps an unbind left.
	for (id, track) in tracks.iter_mut().enumerate() {
		track.track_id = id as u8;
	}
}

/// Which FLV payload shape a bound track is muxed as: a legacy CodecID
/// (`Avc`/`Aac`) or an enhanced-RTMP FourCC codec.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Flavor {
	Avc,
	Hevc,
	Av1,
	Vp9,
	Aac,
	Mp3,
	Opus,
	Ac3,
	Eac3,
}

impl Flavor {
	/// The enhanced-RTMP FourCC for this codec, or `None` for the legacy
	/// (CodecID-signaled) AVC, AAC, and MP3 shapes.
	fn fourcc(self) -> Option<[u8; 4]> {
		match self {
			Flavor::Hevc => Some(*b"hvc1"),
			Flavor::Av1 => Some(*b"av01"),
			Flavor::Vp9 => Some(*b"vp09"),
			Flavor::Opus => Some(*b"Opus"),
			Flavor::Ac3 => Some(*b"ac-3"),
			Flavor::Eac3 => Some(*b"ec-3"),
			Flavor::Avc | Flavor::Aac | Flavor::Mp3 => None,
		}
	}

	/// The enhanced-RTMP FourCC to use in multitrack framing, where every codec
	/// (including the ones with a legacy CodecID) is identified by FourCC.
	fn multitrack_fourcc(self) -> [u8; 4] {
		match self {
			Flavor::Avc => *b"avc1",
			Flavor::Aac => *b"mp4a",
			Flavor::Mp3 => *b".mp3",
			// The rest share their single-track FourCC.
			_ => self.fourcc().expect("enhanced flavor has a FourCC"),
		}
	}

	fn is_video(self) -> bool {
		matches!(self, Flavor::Avc | Flavor::Hevc | Flavor::Av1 | Flavor::Vp9)
	}

	fn has_composition_time(self) -> bool {
		matches!(self, Flavor::Avc | Flavor::Hevc)
	}

	/// True if the codec carries an out-of-band config record (emitted as a
	/// sequence-header tag). VP9 / MP3 / AC-3 / E-AC-3 carry their config in band.
	fn has_sequence_header(self) -> bool {
		matches!(
			self,
			Flavor::Avc | Flavor::Hevc | Flavor::Av1 | Flavor::Aac | Flavor::Opus
		)
	}
}

/// Produce an FLV byte stream from a broadcast, following a catalog stream.
///
/// Use [`next`](Self::next) to pull byte chunks. The first chunk is the FLV file
/// header followed by the AVC/AAC sequence headers; each subsequent chunk is the
/// tag for one media frame. Returns `None` when the broadcast ends. Narrow the
/// renditions first with [`Stream::select`](crate::catalog::Stream::select).
///
/// ## Avc3 sources
///
/// Annex-B H.264 (`H264 { inline: true }`, empty `description`) is accepted: an
/// [`Avc1`](crate::codec::h264::Avc1) transform caches the inline SPS/PPS, builds
/// the avcC for the sequence-header tag, and length-prefixes each sample. The
/// header is deferred until that codec config is available (typically the first
/// keyframe). Only Legacy and LOC container tracks (raw codec payloads) are
/// supported; CMAF tracks are rejected.
pub struct Export<S: Stream> {
	source: crate::Source,
	catalog: Option<S>,
	delay: std::time::Duration,
	/// Frames as tags, encoded only once released.
	jitter: crate::jitter::Buffer<(u8, usize), Tag>,
	/// Emit every rendition as an enhanced-RTMP multitrack track, rather than only
	/// the best video and audio rendition.
	multitrack: bool,

	video: Vec<FlvTrack>,
	audio: Vec<FlvTrack>,

	/// True once the file header and sequence headers have been emitted.
	header_emitted: bool,
	/// Milliseconds added to every tag's timestamps, so the decode times a declared reorder
	/// depth puts before a group's earliest presentation still fit FLV's unsigned clock.
	shift: u32,
}

/// A frame waiting to go out as a tag.
struct Tag {
	frame: Frame,
	/// The tag timestamp, the authored DTS for a codec with composition time.
	timestamp: u32,
	/// The presentation time on the tag clock.
	presentation: u32,
	/// The codec config record the frame was read under, which a sequence-header tag must
	/// have announced before it.
	record: Option<Bytes>,
}

/// A subscribed rendition feeding the muxer.
struct FlvTrack {
	name: String,
	/// The enhanced-RTMP track id, assigned by bind order within its kind. Only
	/// used when muxing multitrack; a single-track FLV stream carries no id.
	track_id: u8,
	source: ExportSource,
	pending: Option<crate::jitter::Arrival<Frame>>,
	/// When the source was last empty, bounding the next frame's arrival.
	empty: moq_clock::Instant,
	finished: bool,
	/// The FLV payload shape (legacy CodecID vs enhanced FourCC) to mux this
	/// track as, fixed from its catalog codec when it's bound.
	flavor: Flavor,
	/// A codec config record synthesized at bind time for the sequence-header
	/// tag, used when the catalog (and thus [`ExportSource::description`]) carries
	/// none. Only AV1 needs this today (its av1C is optional in the catalog).
	fallback_description: Option<Bytes>,
	/// The video rendition's catalog entry as last listed.
	config: Option<hang::catalog::VideoConfig>,
	/// The current group's clock, when its rendition declared `reorder` at the group's start,
	/// holding each frame with the config record it was read under.
	declared: Option<DeclaredClock<(crate::jitter::Arrival<Frame>, Option<Bytes>)>>,
	/// How far to run the authored decode clock behind the PTS, in FLV milliseconds.
	dts_reserve: u32,
	/// Last authored video DTS, in FLV milliseconds.
	last_dts: Option<u32>,
	/// The config record the last sequence-header tag announced.
	announced: Option<Bytes>,
}

impl FlvTrack {
	/// Read `source`, the track the catalog moved this rendition to.
	fn rebind(&mut self, source: ExportSource) {
		self.source = source.continuing(&self.source);
		self.finished = false;
		self.empty = moq_clock::now();
	}

	/// The tag timestamp of a frame presented at `pts` on the tag clock, on the inferred clock.
	fn tag_timestamp(&self, pts: u32) -> anyhow::Result<u32> {
		if self.flavor.has_composition_time() {
			author_dts(pts, self.dts_reserve, self.last_dts)
		} else {
			Ok(pts)
		}
	}

	/// Start a group: queue the last one's frames, settled by `arrived`, the keyframe's arrival,
	/// then clock this one at the depth its rendition declares now, or on the inferred clock.
	fn start_group(
		&mut self,
		key: (u8, usize),
		arrived: moq_clock::Instant,
		shift: u32,
		jitter: &mut crate::jitter::Buffer<(u8, usize), Tag>,
	) -> anyhow::Result<()> {
		self.release(key, Some(arrived), shift, true, jitter)?;
		let depth = match &self.config {
			Some(config) if self.flavor.has_composition_time() => crate::container::declared::depth(config)?,
			_ => None,
		};
		self.declared = depth.map(DeclaredClock::new);
		Ok(())
	}

	/// Queue each frame the declared clock releases, every one when `flush`. A frame held for a
	/// later one is judged as arriving with it, at `settled`.
	fn release(
		&mut self,
		key: (u8, usize),
		settled: Option<moq_clock::Instant>,
		shift: u32,
		flush: bool,
		jitter: &mut crate::jitter::Buffer<(u8, usize), Tag>,
	) -> anyhow::Result<()> {
		let Some(mut clock) = self.declared.take() else {
			return Ok(());
		};
		let released = (|| {
			while let Some(((mut arrival, record), dts)) = clock.pop(flush)? {
				if let Some(settled) = settled {
					arrival.arrived = arrival.arrived.max(settled);
				}
				let timestamp = u32::try_from(dts + i128::from(shift))
					.context("FLV decode time before the stream's origin or past 32 bits")?;
				self.queue(key, arrival, record, timestamp, shift, jitter)?;
			}
			anyhow::Ok(())
		})();
		self.declared = Some(clock);
		released
	}

	/// Queue `arrival` as a tag decoding at `timestamp`, under the config `record` it was read with.
	fn queue(
		&mut self,
		key: (u8, usize),
		arrival: crate::jitter::Arrival<Frame>,
		record: Option<Bytes>,
		timestamp: u32,
		shift: u32,
		jitter: &mut crate::jitter::Buffer<(u8, usize), Tag>,
	) -> anyhow::Result<()> {
		if self.flavor.has_composition_time() {
			self.last_dts = Some(timestamp);
		}
		let presentation = frame_timestamp_ms(&arrival.item, shift)?;
		let crate::jitter::Arrival {
			arrived,
			read,
			restart,
			skip,
			sync,
			leading,
			bytes,
			item,
			..
		} = arrival;
		let pushed = jitter.push(
			key,
			crate::jitter::Arrival {
				arrived,
				read,
				decode: moq_net::Timestamp::from_millis(u64::from(timestamp))?.into(),
				restart,
				skip,
				sync,
				leading,
				bytes,
				item: Tag {
					frame: item,
					timestamp,
					presentation,
					record,
				},
			},
		)?;
		if pushed == crate::jitter::Push::Late {
			tracing::warn!(
				kind = key.0,
				index = key.1,
				dropped = jitter.dropped(),
				"frame missed its deadline; dropped"
			);
		}
		Ok(())
	}

	/// The out-of-band config record for this track's sequence-header tag, or
	/// `None` for codecs that carry their config in band.
	fn config_record(&self) -> anyhow::Result<Option<Bytes>> {
		if !self.flavor.has_sequence_header() {
			return Ok(None);
		}
		let record = self
			.source
			.description()
			.or(self.fallback_description.as_ref())
			.with_context(|| format!("FLV track '{}' missing its codec config record", self.name))?;
		Ok(Some(record.clone()))
	}
}

impl<S: Stream> Export<S> {
	/// Subscribe to `source` and produce FLV byte chunks, driving track
	/// subscription from `catalog`.
	///
	/// `catalog` is any [`Stream`] of catalog snapshots, typically a
	/// [`catalog::Consumer`](crate::catalog::Consumer) directly, or narrowed to
	/// one rendition set via [`Stream::select`](crate::catalog::Stream::select).
	pub fn new(source: crate::Source, catalog: S) -> Self {
		Self {
			source,
			catalog: Some(catalog),
			delay: std::time::Duration::ZERO,
			jitter: crate::jitter::Buffer::new(std::time::Duration::ZERO),
			multitrack: false,
			video: Vec::new(),
			audio: Vec::new(),
			header_emitted: false,
			shift: 0,
		}
	}

	/// Release frames a fixed delay after their decode time, dropping late frames.
	///
	/// A source whose clock runs further off ours than the output's may follow fails the export
	/// once it has used half the delay. The same delay bounds each source's staleness. Zero (the default) releases each frame as
	/// soon as it is read and drops none as late, but a stalled group is still skipped once a
	/// newer one is ready.
	pub fn with_delay(mut self, delay: std::time::Duration) -> Self {
		self.delay = delay;
		self.jitter = crate::jitter::Buffer::new(delay);
		self
	}

	/// Mux every rendition as an enhanced-RTMP multitrack track (one FLV stream
	/// carrying several video and/or audio tracks), rather than only the best
	/// video and audio rendition.
	///
	/// Only enable this for a player that advertised the enhanced-RTMP
	/// `Multitrack` capability in its `connect` `capsEx`; a legacy player can't
	/// parse the multitrack framing. Defaults to off.
	pub fn with_multitrack(mut self, multitrack: bool) -> Self {
		self.multitrack = multitrack;
		self
	}

	/// Get the next byte chunk.
	pub async fn next(&mut self) -> anyhow::Result<Option<Bytes>> {
		kio::wait(|waiter| self.poll_next(waiter)).await
	}

	/// Poll for the next byte chunk.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<anyhow::Result<Option<Bytes>>> {
		// 1. Drain catalog updates to discover the track layout.
		while let Some(catalog) = self.catalog.as_mut() {
			match catalog.poll_next(waiter)? {
				Poll::Ready(Some(snapshot)) => self.update_catalog(snapshot.media())?,
				Poll::Ready(None) => {
					self.catalog = None;
					break;
				}
				Poll::Pending => break,
			}
		}

		self.fill(waiter)?;

		// 3. Emit the header once every track's codec config has resolved.
		if !self.header_emitted {
			if self.header_ready() {
				let header = self.build_header()?;
				self.header_emitted = true;
				return Poll::Ready(Ok(Some(header)));
			}
			// The catalog closed and nothing more can resolve a codec config (no
			// tracks arrived, or every bound track ended first): there's no header.
			if self.catalog.is_none() && (!self.has_tracks() || self.tracks().all(|t| t.finished)) {
				return Poll::Ready(Ok(None));
			}
			return Poll::Pending;
		}

		if let Poll::Ready(ready) = self.jitter.poll_next(waiter) {
			let (kind, index) = ready.track;
			return Poll::Ready(Ok(Some(self.encode_frame(kind == 0, index, ready.item)?)));
		}
		if !self.jitter.is_empty() {
			return Poll::Pending;
		}

		// 5. End-of-stream once every subscribed track is drained.
		if self.has_tracks() && self.tracks().all(|t| t.finished && t.pending.is_none()) {
			if self.catalog.is_none() {
				return Poll::Ready(Ok(None));
			}
		} else if self.catalog.is_none() && !self.has_tracks() {
			return Poll::Ready(Ok(None));
		}

		Poll::Pending
	}

	/// Iterate the subscribed tracks (video first, then audio).
	fn tracks(&self) -> impl Iterator<Item = &FlvTrack> {
		self.video.iter().chain(self.audio.iter())
	}

	fn has_tracks(&self) -> bool {
		!self.video.is_empty() || !self.audio.is_empty()
	}

	#[cfg(test)]
	pub(super) fn next_deadline(&self) -> Option<moq_clock::Instant> {
		self.jitter.next_deadline()
	}

	/// How many frames the export holds between its sources and the output.
	#[cfg(test)]
	pub(super) fn buffered(&self) -> usize {
		self.jitter.len()
	}

	#[cfg(test)]
	pub(super) fn with_replay(mut self) -> Self {
		self.jitter = self.jitter.replay();
		self
	}

	/// Read each track within its jitter window, preserving ingress timing before buffering.
	fn fill(&mut self, waiter: &kio::Waiter) -> anyhow::Result<()> {
		for kind in [0, 1] {
			let count = if kind == 0 { self.video.len() } else { self.audio.len() };
			for index in 0..count {
				loop {
					let track = if kind == 0 {
						&mut self.video[index]
					} else {
						&mut self.audio[index]
					};
					if self.header_emitted && self.jitter.full(&(kind, index), waiter) {
						let _ = track.source.poll_received(waiter);
						break;
					}
					if !self.header_emitted && track.pending.is_some() {
						break;
					}
					let arrival = if let Some(pending) = track.pending.take() {
						pending
					} else {
						if track.finished {
							break;
						}
						let frame = match track.source.poll_event(waiter)? {
							Poll::Ready(Some(Event::Frame { frame, .. })) => frame,
							// A group that ended or was cut short holds frames no later one can precede.
							Poll::Ready(Some(Event::GroupEnd | Event::Cut { .. })) => {
								track.release((kind, index), None, self.shift, true, &mut self.jitter)?;
								continue;
							}
							Poll::Ready(Some(Event::FrameEnd { .. })) => continue,
							Poll::Ready(None) => {
								track.finished = true;
								break;
							}
							Poll::Pending => {
								track.empty = moq_clock::now();
								break;
							}
						};
						// Until its codec config resolves, a frame has no sequence header to follow.
						if !track.source.header_ready() {
							continue;
						}
						crate::jitter::Arrival {
							arrived: track.empty,
							read: moq_clock::now(),
							decode: frame.timestamp.into(),
							restart: track.source.markers(),
							skip: track.source.skips(),
							sync: kind == 1 || frame.keyframe,
							leading: false,
							bytes: frame.payload.len(),
							item: frame,
						}
					};
					if !self.header_emitted {
						track.pending = Some(arrival);
						break;
					}
					let key = (kind, index);
					let record = track.config_record()?;
					if kind == 0 && arrival.item.keyframe {
						track.start_group(key, arrival.arrived, self.shift, &mut self.jitter)?;
					}
					match &mut track.declared {
						Some(clock) => {
							// Clocked in FLV's milliseconds, so the leading frames step a tag tick apart.
							let pts = arrival.item.timestamp.as_millis() as u64;
							let arrived = arrival.arrived;
							clock.push((arrival, record), pts)?;
							track.release(key, Some(arrived), self.shift, false, &mut self.jitter)?;
						}
						None => {
							let timestamp = track.tag_timestamp(frame_timestamp_ms(&arrival.item, self.shift)?)?;
							track.queue(key, arrival, record, timestamp, self.shift, &mut self.jitter)?;
						}
					}
				}
				let track = if kind == 0 {
					&mut self.video[index]
				} else {
					&mut self.audio[index]
				};
				// Nothing settles the frames held when a track ends, so they keep their own arrival.
				if track.finished && self.header_emitted {
					track.release((kind, index), None, self.shift, true, &mut self.jitter)?;
				}
			}
		}
		Ok(())
	}

	fn update_catalog(&mut self, mut catalog: Catalog) -> anyhow::Result<()> {
		self.source.retain_selectable(&mut catalog);

		// A single-track FLV stream binds one rendition of each kind; multitrack
		// binds them all.
		//
		// Only bind before the header is emitted: the header announces the stream's
		// tracks, so a rendition first seen afterward is left unmuxed. Until then a
		// single-track stream rebinds to a better-ranked rendition. A bound rendition the
		// catalog moves to another track follows it, under a new sequence-header tag when
		// its config record changes.
		for track in &mut self.video {
			if let Some(config) = catalog.video.renditions.get(&track.name) {
				// A depth declared after the header deepens the shift, so its group's first DTS stays
				// at or past zero. The shift never shrinks, so tag times keep increasing.
				if self.header_emitted && track.flavor.has_composition_time() {
					self.shift = self.shift.max(config.reorder.map_or(0, u32::from));
				}
				track.config = Some(config.clone());
			}
		}
		if !self.header_emitted {
			self.bind_video(&catalog)?;
			self.bind_audio(&catalog)?;
		} else {
			self.rebind(&catalog)?;
			if self.multitrack
				&& (catalog.video.renditions.len() > self.video.len()
					|| catalog.audio.renditions.len() > self.audio.len())
			{
				tracing::warn!("ignoring FLV rendition that appeared after the stream header");
			}
		}

		// A bound track leaving the catalog is read to its own end rather than refused. A
		// publisher retires a rendition as its track finishes or drops, and the catalog
		// update races that end on another stream, so only the track's end can say which it
		// was: a finish ends it cleanly, and a drop is the error the export reports.
		// Only binding adds tracks, so the clock waits on the bound set as of this update.
		self.jitter.expect(
			(0..self.video.len())
				.map(|i| (0, i))
				.chain((0..self.audio.len()).map(|i| (1, i))),
		);
		Ok(())
	}

	/// Move each bound track the catalog moved to another track or broadcast onto it. The tag
	/// shape is fixed by the header, so the new track must carry the same codec.
	fn rebind(&mut self, catalog: &Catalog) -> anyhow::Result<()> {
		for track in &mut self.video {
			let Some(config) = catalog.video.renditions.get(&track.name) else {
				continue;
			};
			if track.source.reads(&self.source, &track.name, config) {
				continue;
			}
			anyhow::ensure!(
				video_flavor(config)? == track.flavor,
				"FLV track '{}' moved to another codec after the stream header",
				track.name
			);
			ensure_legacy(&config.container, "video", &track.name)?;
			if let Some(source) = ExportSource::for_video(&self.source, &track.name, config, self.delay)? {
				track.fallback_description = fallback_description(config);
				track.dts_reserve = dts_reserve(config);
				track.rebind(source);
			}
		}
		for track in &mut self.audio {
			let Some(config) = catalog.audio.renditions.get(&track.name) else {
				continue;
			};
			if track.source.reads(&self.source, &track.name, config) {
				continue;
			}
			anyhow::ensure!(
				audio_flavor(config)? == track.flavor,
				"FLV track '{}' moved to another codec after the stream header",
				track.name
			);
			ensure_legacy(&config.container, "audio", &track.name)?;
			if let Some(source) = ExportSource::for_audio(&self.source, &track.name, config, self.delay)? {
				track.rebind(source);
			}
		}
		Ok(())
	}

	fn bind_video(&mut self, catalog: &Catalog) -> anyhow::Result<()> {
		let renditions: Vec<_> = if self.multitrack {
			// Name order (the catalog is a BTreeMap), so each keeps a stable track id.
			catalog.video.renditions.iter().collect()
		} else {
			// The best rendition FLV can carry, whatever the names. The rest follow in
			// rank order so a catalog with nothing FLV carries fails on its best one.
			let mut ranked: Vec<_> = catalog.video.ranked().collect();
			ranked.sort_by_key(|(_, config)| {
				video_flavor(config).is_err() || !matches!(config.container, Container::Legacy | Container::Loc)
			});
			ranked
		};

		unbind(&mut self.video, &renditions, self.multitrack, &self.source);

		for (name, config) in renditions {
			if !self.multitrack && !self.video.is_empty() {
				break;
			}
			if self.video.iter().any(|t| &t.name == name) {
				continue;
			}
			let flavor = video_flavor(config)?;
			ensure_legacy(&config.container, "video", name)?;
			let fallback_description = fallback_description(config);
			let Some(source) = ExportSource::for_video(&self.source, name, config, self.delay)? else {
				continue;
			};
			let track_id = u8::try_from(self.video.len()).context("too many FLV video tracks")?;
			self.video.push(FlvTrack {
				name: name.clone(),
				track_id,
				source,
				pending: None,
				empty: moq_clock::now(),
				finished: false,
				flavor,
				fallback_description,
				config: Some(config.clone()),
				declared: None,
				dts_reserve: dts_reserve(config),
				last_dts: None,
				announced: None,
			});
		}
		Ok(())
	}

	fn bind_audio(&mut self, catalog: &Catalog) -> anyhow::Result<()> {
		let renditions: Vec<_> = if self.multitrack {
			// Name order (the catalog is a BTreeMap), so each keeps a stable track id.
			catalog.audio.renditions.iter().collect()
		} else {
			// The best rendition FLV can carry, whatever the names. The rest follow in
			// rank order so a catalog with nothing FLV carries fails on its best one.
			let mut ranked: Vec<_> = catalog.audio.ranked().collect();
			ranked.sort_by_key(|(_, config)| {
				audio_flavor(config).is_err() || !matches!(config.container, Container::Legacy | Container::Loc)
			});
			ranked
		};

		unbind(&mut self.audio, &renditions, self.multitrack, &self.source);

		for (name, config) in renditions {
			if !self.multitrack && !self.audio.is_empty() {
				tracing::warn!("FLV export only supports one audio track; ignoring the rest (enable multitrack)");
				break;
			}
			if self.audio.iter().any(|t| &t.name == name) {
				continue;
			}
			let flavor = audio_flavor(config)?;
			ensure_legacy(&config.container, "audio", name)?;
			let Some(source) = ExportSource::for_audio(&self.source, name, config, self.delay)? else {
				continue;
			};
			let track_id = u8::try_from(self.audio.len()).context("too many FLV audio tracks")?;
			self.audio.push(FlvTrack {
				name: name.clone(),
				track_id,
				source,
				pending: None,
				empty: moq_clock::now(),
				finished: false,
				flavor,
				fallback_description: None,
				config: None,
				declared: None,
				dts_reserve: 0,
				last_dts: None,
				announced: None,
			});
		}
		Ok(())
	}

	/// Header is ready once at least one track is bound and every bound track's
	/// [`ExportSource`] has resolved its codec config (from the catalog
	/// `description` or synthesized by the Avc3 transform).
	fn header_ready(&self) -> bool {
		self.has_tracks() && self.tracks().all(|t| t.source.header_ready())
	}

	/// Build the FLV file header plus every bound track's sequence-header tag.
	fn build_header(&mut self) -> anyhow::Result<Bytes> {
		let mut out = BytesMut::new();

		// FLV file header: signature, version 1, type flags, data offset = 9.
		out.put_slice(b"FLV");
		out.put_u8(1);
		let mut flags = 0u8;
		if !self.video.is_empty() {
			flags |= 0x01;
		}
		if !self.audio.is_empty() {
			flags |= 0x04;
		}
		out.put_u8(flags);
		out.put_u32(9);
		// PreviousTagSize0 is always zero.
		out.put_u32(0);

		for track in self.video.iter().chain(&self.audio) {
			if let Some(config) = track.config_record()? {
				let (tag_type, body) = self.sequence_header(track, &config);
				write_tag(&mut out, tag_type, 0, &body)?;
			}
		}
		for track in self.video.iter_mut().chain(&mut self.audio) {
			track.announced = track.config_record()?;
		}
		// A group decodes its first `reorder` frames a millisecond apart before its earliest
		// presentation, so the deepest declaration bounds how far before zero a DTS can fall.
		self.shift = self
			.video
			.iter()
			.filter(|track| track.flavor.has_composition_time())
			.filter_map(|track| track.config.as_ref()?.reorder)
			.max()
			.map_or(0, u32::from);

		Ok(out.freeze())
	}

	/// Encode one frame as a single FLV tag, after the sequence-header tag announcing its
	/// config record when the last one announced another.
	fn encode_frame(&mut self, is_video: bool, index: usize, tag: Tag) -> anyhow::Result<Bytes> {
		let Tag {
			frame,
			timestamp: timestamp_ms,
			presentation: pts_ms,
			record,
		} = tag;
		let mut out = BytesMut::with_capacity(TAG_HEADER_LEN + frame.payload.len() + 8);
		let track = match is_video {
			true => &self.video[index],
			false => &self.audio[index],
		};
		if let Some(record) = record.filter(|record| track.announced.as_ref() != Some(record)) {
			let (tag_type, body) = self.sequence_header(track, &record);
			write_tag(&mut out, tag_type, timestamp_ms, &body)?;
			let tracks = match is_video {
				true => &mut self.video,
				false => &mut self.audio,
			};
			tracks[index].announced = Some(record);
		}
		let multitrack = self.multitrack;
		if is_video {
			let track = &self.video[index];
			let flavor = track.flavor;
			let track_id = track.track_id;
			let frame_type = if frame.keyframe {
				FRAME_TYPE_KEY
			} else {
				FRAME_TYPE_INTER
			};
			let cts = if flavor.has_composition_time() {
				composition_time(pts_ms, timestamp_ms)?
			} else {
				0
			};
			let mut body = BytesMut::with_capacity(12 + frame.payload.len());
			if multitrack {
				// Enhanced multitrack CodedFrames: ex-header + framing byte + FourCC +
				// track id, then the per-codec composition time (avc1/hvc1) and payload.
				body.put_u8(VIDEO_EX_HEADER | (frame_type << 4) | VIDEO_PACKET_MULTITRACK);
				body.put_u8((MULTITRACK_ONE_TRACK << 4) | VIDEO_PACKET_CODED_FRAMES);
				body.put_slice(&flavor.multitrack_fourcc());
				body.put_u8(track_id);
				if flavor.has_composition_time() {
					write_i24(&mut body, cts)?;
				}
			} else {
				match flavor.fourcc() {
					// Legacy AVC: CodecID + AVCPacketType + signed composition time.
					None => {
						body.put_u8((frame_type << 4) | VIDEO_CODEC_AVC);
						body.put_u8(AVC_NALU);
						write_i24(&mut body, cts)?;
					}
					// Enhanced FourCC CodedFrames. hvc1 keeps the 3-byte composition
					// time; av01/vp09 omit it.
					Some(fourcc) => {
						body.put_u8(VIDEO_EX_HEADER | (frame_type << 4) | VIDEO_PACKET_CODED_FRAMES);
						body.put_slice(&fourcc);
						if flavor == Flavor::Hevc {
							write_i24(&mut body, cts)?;
						}
					}
				}
			}
			body.put_slice(&frame.payload);
			write_tag(&mut out, TAG_VIDEO, timestamp_ms, &body)?;
		} else {
			let track = &self.audio[index];
			let flavor = track.flavor;
			let track_id = track.track_id;
			let mut body = BytesMut::with_capacity(7 + frame.payload.len());
			if multitrack {
				body.put_u8((AUDIO_FORMAT_EX << 4) | AUDIO_PACKET_MULTITRACK);
				body.put_u8((MULTITRACK_ONE_TRACK << 4) | AUDIO_PACKET_CODED_FRAMES);
				body.put_slice(&flavor.multitrack_fourcc());
				body.put_u8(track_id);
			} else {
				match flavor {
					// Legacy AAC: tag header + AACPacketType (raw frame follows).
					Flavor::Aac => {
						body.put_u8(AAC_AUDIO_TAG_HEADER);
						body.put_u8(AAC_RAW);
					}
					// Legacy MP3: tag header only; the raw frame carries its own config.
					Flavor::Mp3 => body.put_u8(MP3_AUDIO_TAG_HEADER),
					// Enhanced FourCC CodedFrames.
					_ => {
						let fourcc = flavor.fourcc().expect("enhanced audio flavor missing FourCC");
						body.put_u8((AUDIO_FORMAT_EX << 4) | AUDIO_PACKET_CODED_FRAMES);
						body.put_slice(&fourcc);
					}
				}
			}
			body.put_slice(&frame.payload);
			write_tag(&mut out, TAG_AUDIO, timestamp_ms, &body)?;
		}
		Ok(out.freeze())
	}

	/// The tag type and body of the sequence-header tag announcing `config` for `track`.
	fn sequence_header(&self, track: &FlvTrack, config: &[u8]) -> (u8, BytesMut) {
		let tag_type = match track.flavor.is_video() {
			true => TAG_VIDEO,
			false => TAG_AUDIO,
		};
		let mut body = BytesMut::new();
		if self.multitrack {
			ex_multitrack_sequence_start(
				&mut body,
				tag_type,
				&track.flavor.multitrack_fourcc(),
				track.track_id,
				config,
			);
			return (tag_type, body);
		}
		match track.flavor {
			Flavor::Avc => {
				body.put_u8((FRAME_TYPE_KEY << 4) | VIDEO_CODEC_AVC);
				body.put_u8(AVC_SEQUENCE_HEADER);
				body.put_slice(&[0, 0, 0]); // composition time
				body.put_slice(config);
			}
			Flavor::Hevc => ex_video_sequence_start(&mut body, b"hvc1", config),
			Flavor::Av1 => ex_video_sequence_start(&mut body, b"av01", config),
			Flavor::Aac => {
				body.put_u8(AAC_AUDIO_TAG_HEADER);
				body.put_u8(AAC_SEQUENCE_HEADER);
				body.put_slice(config);
			}
			Flavor::Opus => {
				body.put_u8((AUDIO_FORMAT_EX << 4) | AUDIO_PACKET_SEQUENCE_START);
				body.put_slice(b"Opus");
				body.put_slice(config);
			}
			// Codecs with no out-of-band record are filtered out by `config_record`.
			Flavor::Vp9 | Flavor::Mp3 | Flavor::Ac3 | Flavor::Eac3 => {
				unreachable!("no sequence header for {:?}", track.name)
			}
		}
		(tag_type, body)
	}
}

/// Append one FLV tag (header + body + trailing `PreviousTagSize`) to `out`.
///
/// Errors if `body` exceeds FLV's 24-bit `DataSize` field (16 MiB), which would
/// otherwise be silently truncated into a corrupt header.
fn write_tag(out: &mut BytesMut, tag_type: u8, timestamp_ms: u32, body: &[u8]) -> anyhow::Result<()> {
	let size: u32 = body
		.len()
		.try_into()
		.ok()
		.filter(|n| *n <= 0x00FF_FFFF)
		.context("FLV tag body exceeds the 24-bit DataSize limit")?;
	out.put_u8(tag_type);
	out.put_slice(&size.to_be_bytes()[1..]); // 24-bit data size
	out.put_slice(&timestamp_ms.to_be_bytes()[1..]); // 24-bit timestamp (low)
	out.put_u8((timestamp_ms >> 24) as u8); // timestamp extension (high)
	out.put_slice(&[0, 0, 0]); // stream id
	out.put_slice(body);
	out.put_u32(TAG_HEADER_LEN as u32 + size);
	Ok(())
}

fn ensure_legacy(container: &Container, kind: &str, name: &str) -> anyhow::Result<()> {
	match container {
		Container::Legacy | Container::Loc => Ok(()),
		Container::Cmaf { .. } => anyhow::bail!("FLV export does not support CMAF {kind} track '{name}'"),
		Container::Unknown(unknown) => anyhow::bail!(
			"FLV export does not support container '{}' on {kind} track '{name}'",
			unknown.kind().unwrap_or("<missing>")
		),
	}
}

/// AV1's av1C is optional in the catalog, so synthesize one from the codec struct and the
/// enhanced SequenceStart tag always has a config record.
fn fallback_description(config: &hang::catalog::VideoConfig) -> Option<Bytes> {
	match (&config.codec, config.description.as_ref()) {
		(VideoCodec::AV1(av1), None) => Some(Bytes::copy_from_slice(&av1c_bytes(av1))),
		_ => None,
	}
}

fn video_flavor(config: &hang::catalog::VideoConfig) -> anyhow::Result<Flavor> {
	match &config.codec {
		VideoCodec::H264(_) => Ok(Flavor::Avc),
		VideoCodec::H265(_) => Ok(Flavor::Hevc),
		VideoCodec::AV1(_) => Ok(Flavor::Av1),
		VideoCodec::VP9(_) => Ok(Flavor::Vp9),
		other => anyhow::bail!("FLV export does not support video codec {other:?}"),
	}
}

fn audio_flavor(config: &hang::catalog::AudioConfig) -> anyhow::Result<Flavor> {
	match &config.codec {
		AudioCodec::AAC(_) => Ok(Flavor::Aac),
		AudioCodec::Mp3 => Ok(Flavor::Mp3),
		AudioCodec::Opus => Ok(Flavor::Opus),
		AudioCodec::Ac3 => Ok(Flavor::Ac3),
		AudioCodec::Ec3 => Ok(Flavor::Eac3),
		other => anyhow::bail!("FLV export does not support audio codec {other:?}"),
	}
}

/// The frame's presentation time on the tag clock, `shift` milliseconds past its own.
fn frame_timestamp_ms(frame: &Frame, shift: u32) -> anyhow::Result<u32> {
	u32::try_from(frame.timestamp.as_millis())
		.ok()
		.and_then(|pts| pts.checked_add(shift))
		.context("FLV timestamp exceeds 32 bits")
}

fn composition_time(pts: u32, dts: u32) -> anyhow::Result<i32> {
	let cts = i64::from(pts) - i64::from(dts);
	i32::try_from(cts)
		.ok()
		.filter(|cts| (-0x80_0000..=0x7f_ffff).contains(cts))
		.context("FLV composition time exceeds signed 24 bits")
}

fn write_i24(out: &mut BytesMut, value: i32) -> anyhow::Result<()> {
	anyhow::ensure!(
		(-0x80_0000..=0x7f_ffff).contains(&value),
		"FLV composition time exceeds signed 24 bits"
	);
	out.put_slice(&(value as u32).to_be_bytes()[1..]);
	Ok(())
}

fn author_dts(pts: u32, reserve: u32, last: Option<u32>) -> anyhow::Result<u32> {
	let mut dts = pts.saturating_sub(reserve);
	if let Some(prev) = last
		&& dts <= prev
	{
		dts = prev.checked_add(1).context("FLV DTS exceeds 32 bits")?;
	}
	Ok(dts)
}

fn dts_reserve(config: &hang::catalog::VideoConfig) -> u32 {
	config
		.jitter
		.and_then(|t| u32::try_from(t.as_millis()).ok())
		.filter(|reserve| *reserve > 0)
		.unwrap_or(1)
}

/// Append an enhanced-RTMP video `SequenceStart` tag body: ex-header + FourCC +
/// the codec config record.
fn ex_video_sequence_start(body: &mut BytesMut, fourcc: &[u8; 4], config: &[u8]) {
	body.put_u8(VIDEO_EX_HEADER | (FRAME_TYPE_KEY << 4) | VIDEO_PACKET_SEQUENCE_START);
	body.put_slice(fourcc);
	body.put_slice(config);
}

/// Append an enhanced-RTMP multitrack `SequenceStart` tag body (a single
/// `OneTrack` record): ex-header + multitrack framing + FourCC + track id + the
/// codec config record. `tag_type` selects the video vs audio ex-header byte.
fn ex_multitrack_sequence_start(body: &mut BytesMut, tag_type: u8, fourcc: &[u8; 4], track_id: u8, config: &[u8]) {
	if tag_type == TAG_VIDEO {
		body.put_u8(VIDEO_EX_HEADER | (FRAME_TYPE_KEY << 4) | VIDEO_PACKET_MULTITRACK);
		body.put_u8((MULTITRACK_ONE_TRACK << 4) | VIDEO_PACKET_SEQUENCE_START);
	} else {
		body.put_u8((AUDIO_FORMAT_EX << 4) | AUDIO_PACKET_MULTITRACK);
		body.put_u8((MULTITRACK_ONE_TRACK << 4) | AUDIO_PACKET_SEQUENCE_START);
	}
	body.put_slice(fourcc);
	body.put_u8(track_id);
	body.put_slice(config);
}

/// Build a minimal `AV1CodecConfigurationRecord` (av1C) from the catalog AV1
/// struct, with an empty `configOBUs` (the sequence header is carried in band).
fn av1c_bytes(av1: &AV1) -> [u8; 4] {
	let high_bitdepth = av1.bitdepth >= 10;
	let twelve_bit = av1.bitdepth >= 12;
	[
		0x81, // marker (1) + version (1)
		((av1.profile & 0x07) << 5) | (av1.level & 0x1f),
		((av1.tier == 'H') as u8) << 7
			| (high_bitdepth as u8) << 6
			| (twelve_bit as u8) << 5
			| (av1.mono_chrome as u8) << 4
			| (av1.chroma_subsampling_x as u8) << 3
			| (av1.chroma_subsampling_y as u8) << 2
			| (av1.chroma_sample_position & 0x03),
		0x00, // no initial presentation delay
	]
}
