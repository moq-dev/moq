use bytes::{Bytes, BytesMut};
use hang::catalog::{AAC, AudioCodec, AudioConfig, Container, H264, H265, VP9, VideoCodec, VideoConfig};
use moq_net::Timestamp;
use mp4_atom::{Any, Atom, DecodeMaybe, Encode, Mdat, Moof, Moov, Trak};
use std::collections::{HashMap, HashSet};

use super::{Error, Kind, edit::Edit, sample};
use crate::Result;
use crate::catalog::Estimator;

/// Imports fragmented MP4 (fMP4/CMAF) into a MoQ broadcast.
///
/// By default each fragment is unwrapped: every sample becomes one
/// [`Legacy`](Container::Legacy) frame carrying the codec payload, timed at its presentation time,
/// so a consumer needs no ISO BMFF parser. Either way, each track's edit list is applied to the
/// frame timestamps and left out of the catalog. [`with_passthrough`](Self::with_passthrough) instead
/// republishes each fragment whole as one [`Cmaf`](Container::Cmaf) frame, for a source whose exact
/// `trun` timing must survive.
///
/// ## Supported Codecs
///
/// **Video:**
/// - H.264 (AVC1/AVC3)
/// - H.265 (HEV1/HVC1)
/// - VP8
/// - VP9
/// - AV1
///
/// **Audio:**
/// - AAC (MP4A)
/// - Opus
/// - FLAC
///
/// **Text:** WebVTT (`wvtt`), reconstructed as timed text, and TTML (`stpp`) documents moved onto the
/// catalog clock; unsupported subtitle codecs, including other XML namespaces, are skipped.
pub struct Import<E: crate::catalog::hang::CatalogExt = ()> {
	/// The broadcast being produced
	broadcast: moq_net::broadcast::Producer,

	/// The catalog being produced
	catalog: crate::catalog::Producer<E>,

	/// Held until the first fragment anchors the clock, so the catalog is withheld from the broadcast
	/// until every rendition is in and its root `clock` is final (and, when composed with other
	/// importers, until they release theirs too).
	initial_reservation: Option<crate::catalog::Reserved<E>>,

	/// The source's timestamp base: the first fragment anchors it, and every sample's timestamp
	/// shifts by its offset onto the catalog clock. The `tfdt` inside a passthrough fragment stays
	/// verbatim.
	timebase: crate::catalog::Timebase<E>,

	// Which track roles to publish. `None` imports every supported track.
	select: Option<crate::select::Broadcast>,

	// Republish each fragment whole as CMAF instead of unwrapping its samples.
	passthrough: bool,

	// A lookup to tracks in the broadcast
	tracks: HashMap<u32, Fmp4Track<E>>,
	text: HashMap<u32, super::text::Track>,

	// Track ids skipped by `select`, so their moof fragments are ignored rather
	// than treated as referencing an unknown track.
	skipped: HashSet<u32>,

	// The moov atom at the start of the file.
	moov: Option<Moov>,

	// The latest moof header
	moof: Option<Moof>,
	// Bytes consumed since that moof began, including intervening boxes.
	moof_span: usize,

	// Bytes carried across calls: a partial atom at the tail of one `decode` waits
	// here for the rest to arrive on the next call.
	buffer: BytesMut,

	// A segment boundary (a styp or an explicit `cut()`) waiting to land on the next fragment.
	pending_cut: bool,

	// Which segment the fragments arriving now belong to. Bumped at each boundary; a track whose
	// open group belongs to an older one rolls, so every track segments identically.
	segment: u64,

	// The current segment's boundary was declared by the source (rather than inferred from a
	// keyframe), so each track cuts its HLS segments where it opens its group for the segment.
	declared: bool,
	dropped_metadata: u64,
}

/// The catalog entry for one passthrough track, whichever section it lives in.
///
/// Both arms own the private catalog entry, so publishing what the estimator measured and retiring
/// the entry on drop is the same code either way.
enum Rendition<E: crate::catalog::hang::CatalogExt> {
	Video(crate::catalog::VideoTrack<E>),
	Audio(crate::catalog::AudioTrack<E>),
}

impl<E: crate::catalog::hang::CatalogExt> Rendition<E> {
	fn estimate(&mut self, estimate: crate::catalog::Estimate) -> crate::Result<()> {
		match self {
			Self::Video(rendition) => rendition.estimate(estimate),
			Self::Audio(rendition) => rendition.estimate(estimate),
		}
	}
}

struct Fmp4Track<E: crate::catalog::hang::CatalogExt> {
	kind: Kind,

	/// The `trex` values this track's samples fall back to.
	defaults: sample::Defaults,

	/// The `mdhd` timescale the samples count in.
	timescale: moq_net::Timescale,

	/// The edit list, applied to every sample's presentation time.
	edit: Edit,

	/// No sync sample precedes the next sample, since the import joined mid-GOP or the edit list
	/// dropped the last one, so the samples decoding from it go too.
	orphaned: bool,

	// The decode time of the last fragment, which the next one has to advance past.
	last_decode_time: Option<u64>,

	// The shortest declared sample duration, used when a final sample leaves its end open.
	sample_duration: Option<Timestamp>,

	// The segment this track's open group belongs to. A mismatch with `Import::segment` rolls the
	// group, which is what keeps audio on the same boundaries as video.
	segment: Option<u64>,

	output: Output<E>,
}

/// Where a track's samples are published.
enum Output<E: crate::catalog::hang::CatalogExt> {
	/// One CMAF frame per fragment.
	Passthrough(Box<Passthrough<E>>),
	/// One Legacy frame per sample.
	Video(crate::container::Producer<crate::catalog::hang::Container, VideoConfig>),
	/// One Legacy frame per sample.
	Audio(crate::container::Producer<crate::catalog::hang::Container, AudioConfig>),
}

/// A track republishing its source fragments whole, writing groups by hand.
struct Passthrough<E: crate::catalog::hang::CatalogExt> {
	/// The catalog entry, which owns the published bitrate and jitter and removes itself on drop.
	rendition: Rendition<E>,

	track: moq_net::track::Producer,
	group: Option<moq_net::group::Producer>,
	/// The latest group's start, which the next group may not start below.
	start: Option<Timestamp>,

	// Reports this track's groups into its HLS records, unless disabled. Passthrough writes groups by
	// hand (no `container::Producer`), so the recorder is fed directly rather than through
	// `with_recorder`.
	recorder: Option<crate::hls::Recorder>,

	// Sequence to use for the next group, set by `Import::seek`.
	pending_sequence: Option<u64>,

	// Measures the track's catalog jitter and bitrate from the fragments; the descriptor's own
	// bitrate, when it declares one, still wins over what this measures.
	estimator: Estimator,

	/// Peak-hold claim on the connection allocator. Passthrough writes groups by
	/// hand, so this sits beside the estimator the same way it does on a
	/// [`container::Producer`](crate::container::Producer).
	claim: crate::catalog::Claim,

	/// The samples and sample bytes the catalog reports this track has written.
	sent: std::sync::Arc<crate::stats::Sent>,
}

impl<E: crate::catalog::hang::CatalogExt> Passthrough<E> {
	fn publish_estimate(&mut self) -> crate::Result<()> {
		let estimate = self.estimator.estimate();
		self.claim.update(&self.track.demand(), estimate.bitrate);
		self.rendition.estimate(estimate)
	}

	/// Close the open group, so the next fragment opens one at `pending_sequence` or the next.
	fn close(&mut self) -> crate::Result<()> {
		self.estimator.cut(None);
		self.publish_estimate()?;
		if let Some(mut group) = self.group.take() {
			group.finish()?;
		}
		Ok(())
	}

	/// Close the open group and finish the track.
	fn finish(&mut self) -> crate::Result<()> {
		self.close()?;
		self.track.finish()?;
		// The importer may be kept around, but its media stops holding the other tracks back.
		self.estimator.discontinuity();
		// Dropping the recorder stops the open HLS segment, so the track is advertised.
		self.recorder = None;
		Ok(())
	}

	/// [`finish`](Self::finish) the track unless it already ended, for an importer about to drop it.
	fn end(&mut self) -> crate::Result<()> {
		if self.track.finished().is_some() || self.track.is_closed() {
			return Ok(());
		}
		self.finish()
	}
}

/// Forward a call to whichever unwrapped producer a track writes.
macro_rules! media {
	($output:expr, $producer:ident => $body:expr, $passthrough:pat => $other:expr) => {
		match $output {
			Output::Video($producer) => $body,
			Output::Audio($producer) => $body,
			Output::Passthrough($passthrough) => $other,
		}
	};
}

/// One sample of a fragment, resolved against the track's defaults.
struct Sample {
	/// Decode time, in media ticks.
	dts: u64,
	/// Composition time, in media ticks.
	pts: u64,
	/// Presentation time once the edit list applies, at the track's scale.
	timestamp: Timestamp,
	duration: Option<u32>,
	flags: u32,
	sync: bool,
	/// The sample's bytes within the `mdat` payload.
	range: std::ops::Range<usize>,
}

/// The samples one `traf` places in its `mdat`.
struct Run {
	samples: Vec<Sample>,
	/// The earliest presentation time, which a passthrough fragment is stamped with.
	earliest: Timestamp,
	/// The furthest presentation end any sample reaches.
	end: Timestamp,
	/// The edit list dropped some of the fragment's samples.
	dropped: bool,
}

impl<E: crate::catalog::hang::CatalogExt> Import<E> {
	/// Create a new fMP4 importer that will write to the given broadcast.
	///
	/// The broadcast will be populated with tracks as they're discovered in the fMP4 file.
	pub fn new(broadcast: moq_net::broadcast::Producer, reserved: crate::catalog::Reserved<E>) -> Self {
		Self {
			catalog: reserved.producer(),
			timebase: reserved.timebase(),
			initial_reservation: Some(reserved),
			select: None,
			passthrough: false,
			tracks: HashMap::default(),
			text: HashMap::default(),
			skipped: HashSet::default(),
			moov: None,
			moof: None,
			moof_span: 0,
			broadcast,
			buffer: BytesMut::new(),
			pending_cut: false,
			segment: 0,
			declared: false,
			dropped_metadata: 0,
		}
	}

	/// Return the number of metadata units discarded by this importer.
	#[cfg(test)]
	pub(crate) fn dropped_metadata(&self) -> u64 {
		self.dropped_metadata
	}

	/// Declare that the next fragment starts a new segment, for callers that know the source's
	/// segmentation out of band (e.g. an HLS import following its playlist).
	///
	/// A `styp` atom (a CMAF segment on disk) does this on its own, so an importer reading a
	/// segmented file needs no help. Each track's HLS records then start a segment where that track
	/// opens its group for the new segment, following the source's segmentation instead of the
	/// minimum-duration pacing.
	///
	/// This is where audio groups are drawn too: audio rolls its group at a segment boundary, so
	/// it follows the same segmentation as video. A passthrough video track rolls there as well,
	/// while an unwrapped one opens a group at each sync sample.
	pub fn cut(&mut self) -> Result<()> {
		self.pending_cut = true;
		Ok(())
	}

	/// Restrict which track roles are published.
	///
	/// fMP4 import selects whole roles: a [`select::Broadcast`](crate::select::Broadcast)
	/// that doesn't select video drops every video track, and likewise for audio. The
	/// rendition-level narrowing inside a [`select::Video`](crate::select::Video) /
	/// [`select::Audio`](crate::select::Audio) (by name or codec) is a consume-side
	/// concern and is ignored here. Without this, every supported track is imported.
	pub fn with_select(mut self, select: crate::select::Broadcast) -> Self {
		self.select = Some(select);
		self
	}

	/// Republish each fragment whole as one [`Cmaf`](Container::Cmaf) frame instead of
	/// unwrapping it into one [`Legacy`](Container::Legacy) frame per sample.
	///
	/// Keeps the source's exact `trun` timing (sample durations, composition offsets) at the cost
	/// of every consumer parsing ISO BMFF. Text tracks are reconstructed as timed text either way.
	pub fn with_passthrough(mut self, passthrough: bool) -> Self {
		self.passthrough = passthrough;
		self
	}

	/// Whether `kind` is selected for import (every role when unset).
	fn selects(&self, kind: &Kind) -> bool {
		match (&self.select, kind) {
			(None, _) => true,
			(Some(select), Kind::Video) => select.has_video(),
			(Some(select), Kind::Audio) => select.has_audio(),
		}
	}

	/// Decode a buffer of bytes.
	///
	/// Samples must be nonempty and use moof-relative offsets; absolute `base_data_offset`
	/// addressing is unsupported. With ffmpeg, use `default_base_moof`.
	pub fn decode(&mut self, data: &[u8]) -> Result<()> {
		self.buffer.extend_from_slice(data);
		self.drain()
	}

	/// Parse every whole top-level atom buffered so far, leaving any trailing
	/// partial atom for the next call.
	fn drain(&mut self) -> Result<()> {
		// Parse complete atoms first, recording each one's byte range, then process
		// them. Collecting up front keeps `self.buffer` un-borrowed while the handlers
		// (`init`/`extract`) take `&mut self`.
		let mut parsed = Vec::new();
		let mut position = 0;
		loop {
			let mut cursor = std::io::Cursor::new(&self.buffer[position..]);
			let Some(atom) = mp4_atom::Any::decode_maybe(&mut cursor)? else {
				break;
			};
			let size = cursor.position() as usize;
			parsed.push((atom, position, size));
			position += size;
		}

		if position == 0 {
			return Ok(());
		}

		// Detach the fully-parsed prefix as a cheap ref-counted buffer so each mdat's
		// raw bytes can be sliced out without copying or borrowing `self`.
		let consumed = self.buffer.split_to(position).freeze();

		for (atom, start, size) in parsed {
			if self.moof.is_some() {
				self.moof_span = self.moof_span.checked_add(size).ok_or(Error::InvalidDataOffset)?;
			}
			match atom {
				Any::Ftyp(_) => {}
				Any::Emsg(_) => {
					self.dropped_metadata += 1;
					if self.dropped_metadata == 1 {
						tracing::warn!(count = self.dropped_metadata, "discarding fMP4 emsg metadata");
					}
				}
				// A styp opens a CMAF segment: the on-disk segmentation is the boundary,
				// landing on the next fragment.
				Any::Styp(_) => self.pending_cut = true,
				Any::Moov(moov) => {
					self.init(moov)?;
				}
				Any::Moof(moof) => {
					if self.moof.is_some() {
						return Err(Error::DuplicateMoof.into());
					}
					self.moof.replace(moof);
					self.moof_span = size;
				}
				Any::Mdat(mdat) => {
					let raw = consumed.slice(start..start + size);
					self.extract(mdat, raw)?;
				}
				_ => {
					// Skip unknown atoms (e.g., sidx, which is optional and used for segment indexing)
					// These are safe to ignore and don't affect playback
				}
			}
		}

		Ok(())
	}

	fn init(&mut self, moov: Moov) -> Result<()> {
		// A second moov would re-declare a track set the catalog already advertises.
		if self.moov.is_some() {
			return Err(Error::DuplicateMoov.into());
		}
		// Only `finish()` releases the reservation before a moov, since a fragment needs one.
		let reserved = self.initial_reservation.clone().ok_or(Error::MoovAfterFinish)?;

		for trak in &moov.trak {
			let track_id = trak.tkhd.track_id;
			let handler = &trak.mdia.hdlr.handler;

			if matches!(handler.as_ref(), b"sbtl" | b"subt" | b"text") {
				if self.init_text(trak, &moov, &reserved)? {
					continue;
				}
				tracing::warn!(track_id, handler = %handler, "unsupported fMP4 subtitle encoding, skipping");
				self.skipped.insert(track_id);
				continue;
			}

			let kind = Kind::from_handler(*handler)?;

			// Drop tracks whose role isn't selected before minting or publishing them; their
			// moof fragments are ignored in `extract`.
			if !self.selects(&kind) {
				self.skipped.insert(track_id);
				continue;
			}

			let timescale = moq_net::Timescale::new(trak.mdia.mdhd.timescale as u64)?;
			let edit = Edit::new(trak, moov.mvhd.timescale)?;
			let output = match self.passthrough {
				true => Output::Passthrough(Box::new(
					self.init_passthrough(trak, &moov, &reserved, kind, timescale)?,
				)),
				false => self.init_unwrap(trak, &moov, &reserved, kind)?,
			};

			self.tracks.insert(
				track_id,
				Fmp4Track {
					kind,
					defaults: sample::Defaults::from_moov(&moov, track_id),
					timescale,
					edit,
					// Audio samples decode alone, whatever their sync flags say.
					orphaned: kind == Kind::Video,
					last_decode_time: None,
					sample_duration: None,
					segment: None,
					output,
				},
			);
		}

		// The reservation stays held until the first fragment anchors the clock, unless no track
		// was declared: then no fragment ever will.
		if self.tracks.is_empty() && self.text.is_empty() {
			self.initial_reservation = None;
		}

		self.moov = Some(moov);

		Ok(())
	}

	fn init_passthrough(
		&mut self,
		trak: &Trak,
		moov: &Moov,
		reserved: &crate::catalog::Reserved<E>,
		kind: Kind,
		timescale: moq_net::Timescale,
	) -> Result<Passthrough<E>> {
		// Declare the track at the fMP4's native timescale, which the moq-hang draft requires
		// of a CMAF track: frame timestamps are emitted at this same scale (see `extract`), so
		// each one carries its fragment's earliest presentation time exactly.
		let track = self.broadcast.create_track(
			self.broadcast.unique_name(".m4s"),
			self.catalog.track_info(kind.priority()).with_timescale(timescale),
		)?;

		// Enroll every track for HLS records: passthrough writes groups by hand (no
		// `container::Producer`), so the recorder is fed directly at each fragment.
		let mut recorder = self.catalog.enroll(track.name(), None)?;
		if let Some(recorder) = recorder.as_mut() {
			recorder.lend(&track);
		}

		// Whatever the descriptor declared (a bitrate) is authoritative; the rest is filled by
		// `estimate` as fragments arrive.
		let rendition = match kind {
			Kind::Video => {
				let config = self.init_video(trak, moov)?;
				let mut rendition = reserved.init(track.name())?;
				rendition.set(config)?;
				Rendition::Video(rendition)
			}
			Kind::Audio => {
				let config = self.init_audio(trak, moov)?;
				let mut rendition = reserved.init(track.name())?;
				rendition.set(config)?;
				Rendition::Audio(rendition)
			}
		};

		Ok(Passthrough {
			sent: self.catalog.sent(track.name()),
			rendition,
			track,
			group: None,
			start: None,
			recorder,
			pending_sequence: None,
			estimator: self.catalog.estimator(),
			claim: crate::catalog::Claim::new(self.catalog.bandwidth()),
		})
	}

	fn init_unwrap(
		&mut self,
		trak: &Trak,
		moov: &Moov,
		reserved: &crate::catalog::Reserved<E>,
		kind: Kind,
	) -> Result<Output<E>> {
		// Legacy frames carry microseconds, which the track's default timescale already is.
		let (suffix, wire_kind) = match kind {
			Kind::Video => (".mp4-v", crate::container::Kind::Video),
			Kind::Audio => (".mp4-a", crate::container::Kind::Audio),
		};
		let track = self.broadcast.create_track(
			self.broadcast.unique_name(suffix),
			self.catalog.track_info(kind.priority()),
		)?;
		let wire = crate::catalog::hang::Container::new(&Container::Legacy, wire_kind)?;
		Ok(match kind {
			Kind::Video => Output::Video(reserved.video(track, wire, self.init_video(trak, moov)?)?),
			Kind::Audio => Output::Audio(reserved.audio(track, wire, self.init_audio(trak, moov)?)?),
		})
	}

	fn init_text(&mut self, trak: &Trak, moov: &Moov, reserved: &crate::catalog::Reserved<E>) -> Result<bool> {
		let (format, mut config) = match trak.mdia.minf.stbl.stsd.codecs.as_slice() {
			[mp4_atom::Codec::Wvtt(codec)] => {
				if codec.plaintext.data_reference_index != 1 {
					return Err(Error::InvalidSubtitle("external subtitle data reference").into());
				}
				let header = crate::container::text::header(&codec.config.config).map_err(Error::InvalidSubtitle)?;
				let mut config = hang::catalog::TextConfig::new(hang::catalog::TextFormat::Vtt);
				config.role = crate::container::text::role(&header);
				(super::text::Format::Vtt { header }, config)
			}
			[mp4_atom::Codec::Unknown(kind, body)] if kind.as_ref() == b"stpp" && super::text::stpp(body)? => (
				super::text::Format::Ttml,
				hang::catalog::TextConfig::new(hang::catalog::TextFormat::Ttml),
			),
			_ => return Ok(false),
		};
		let edit = Edit::new(trak, moov.mvhd.timescale)?;
		config.lang = Some(trak.mdia.mdhd.language.clone());
		// The picker label is the HandlerBox name; `vlab` is an opaque source identifier.
		config.label = (!trak.mdia.hdlr.name.is_empty()).then(|| trak.mdia.hdlr.name.clone());
		let wire = crate::catalog::hang::Container::new(&config.container, crate::container::Kind::Data)?;
		let track = self.broadcast.create_track(
			self.broadcast.unique_name(".mp4-t"),
			self.catalog.track_info(hang::catalog::PRIORITY.text),
		)?;
		let producer = reserved.text(track, wire, config)?;
		self.text.insert(
			trak.tkhd.track_id,
			super::text::Track {
				producer,
				format,
				defaults: sample::Defaults::from_moov(moov, trak.tkhd.track_id),
				timescale: moq_net::Timescale::new(trak.mdia.mdhd.timescale as u64)?,
				edit,
				end_decode_time: None,
				discontinuity: false,
			},
		);
		Ok(true)
	}

	/// The container a rendition advertises: unwrapped samples are Legacy frames, while a
	/// passthrough track carries its own single-track init segment.
	fn container(&self, trak: &Trak, moov: &Moov) -> Result<Container> {
		if !self.passthrough {
			return Ok(Container::Legacy);
		}

		// Build a single-track init segment (ftyp+moov) for this track.
		{
			let ftyp = mp4_atom::Ftyp {
				major_brand: b"isom".into(),
				minor_version: 0x200,
				compatible_brands: vec![b"isom".into(), b"iso6".into(), b"mp41".into()],
			};

			// Build a moov with just this single track and matching mvex/trex.
			let track_id = trak.tkhd.track_id;
			let trex = moov
				.mvex
				.as_ref()
				.and_then(|mvex| mvex.trex.iter().find(|trex| trex.track_id == track_id))
				.cloned()
				.unwrap_or(mp4_atom::Trex {
					track_id,
					default_sample_description_index: 1,
					..Default::default()
				});

			// The importer applies the edit list to every frame timestamp, so a consumer applying it
			// again would shift the track twice.
			let mut trak = trak.clone();
			trak.edts = None;

			let single_moov = Moov {
				mvhd: moov.mvhd.clone(),
				trak: vec![trak],
				mvex: Some(mp4_atom::Mvex {
					mehd: None,
					trex: vec![trex],
				}),
				ainf: None,
				meta: None,
				udta: None,
			};

			let mut buf = Vec::new();
			ftyp.encode(&mut buf)?;
			single_moov.encode(&mut buf)?;

			Ok(Container::Cmaf { init: buf.into() })
		}
	}

	fn init_video(&mut self, trak: &Trak, moov: &Moov) -> Result<VideoConfig> {
		let container = self.container(trak, moov)?;
		let stsd = &trak.mdia.minf.stbl.stsd;

		let codec = match stsd.codecs.len() {
			0 => return Err(Error::MissingCodec.into()),
			1 => &stsd.codecs[0],
			_ => return Err(Error::MultipleCodecs.into()),
		};

		let (mut config, colr, pasp) = match codec {
			mp4_atom::Codec::Avc1(avc1) => (
				self.init_h264(false, &avc1.avcc, &avc1.visual, container)?,
				avc1.colr.as_ref(),
				avc1.pasp.as_ref(),
			),
			mp4_atom::Codec::Avc3(avc3) => (
				self.init_h264(true, &avc3.avcc, &avc3.visual, container)?,
				avc3.colr.as_ref(),
				avc3.pasp.as_ref(),
			),
			mp4_atom::Codec::Hev1(hev1) => (
				self.init_h265(true, &hev1.hvcc, &hev1.visual, container)?,
				hev1.colr.as_ref(),
				hev1.pasp.as_ref(),
			),
			mp4_atom::Codec::Hvc1(hvc1) => (
				self.init_h265(false, &hvc1.hvcc, &hvc1.visual, container)?,
				hvc1.colr.as_ref(),
				hvc1.pasp.as_ref(),
			),
			mp4_atom::Codec::Vp08(vp08) => {
				let mut config = VideoConfig::new(VideoCodec::VP8);
				// vpcC carries VP8's colour too; its range is always explicit.
				let vpcc = &vp08.vpcc;
				config.color = crate::codec::color::cicp(
					vpcc.color_primaries,
					vpcc.transfer_characteristics,
					vpcc.matrix_coefficients,
					Some(vpcc.video_full_range_flag),
				);
				config.coded_width = Some(vp08.visual.width as _);
				config.coded_height = Some(vp08.visual.height as _);
				config.container = container;
				(config, vp08.colr.as_ref(), vp08.pasp.as_ref())
			}
			mp4_atom::Codec::Vp09(vp09) => {
				// https://github.com/gpac/mp4box.js/blob/325741b592d910297bf609bc7c400fc76101077b/src/box-codecs.js#L238
				let vpcc = &vp09.vpcc;

				let vp9 = VP9 {
					profile: vpcc.profile,
					level: vpcc.level,
					bit_depth: vpcc.bit_depth,
					color_primaries: vpcc.color_primaries,
					chroma_subsampling: vpcc.chroma_subsampling,
					transfer_characteristics: vpcc.transfer_characteristics,
					matrix_coefficients: vpcc.matrix_coefficients,
					full_range: vpcc.video_full_range_flag,
				};
				let mut config = VideoConfig::new(vp9.clone());
				config.color = crate::codec::vp9::color(&vp9);
				config.coded_width = Some(vp09.visual.width as _);
				config.coded_height = Some(vp09.visual.height as _);
				config.container = container;
				(config, vp09.colr.as_ref(), vp09.pasp.as_ref())
			}
			mp4_atom::Codec::Av01(av01) => {
				let mut config = VideoConfig::new(crate::codec::av1::av1_from_av1c(&av01.av1c));
				config.color = crate::codec::av1::color(&av01.av1c.config_obus);
				config.coded_width = Some(av01.visual.width as _);
				config.coded_height = Some(av01.visual.height as _);
				config.container = container;
				(config, av01.colr.as_ref(), av01.pasp.as_ref())
			}
			mp4_atom::Codec::Unknown(unknown, _) => return Err(Error::UnknownCodec(*unknown).into()),
			unsupported => return Err(Error::UnsupportedCodec(Box::new(unsupported.clone())).into()),
		};
		// The codec config outranks the colr box a muxer added, which only fills its gaps.
		let colr = colr.map(super::colr_color).transpose()?.flatten();
		crate::codec::color::fill(&mut config.color, colr.as_ref(), "colr");
		if let Some(pasp) = pasp {
			let width = config.coded_width.ok_or(Error::InvalidAspect("missing width"))?;
			let height = config.coded_height.ok_or(Error::InvalidAspect("missing height"))?;
			let aspect = crate::codec::video::reduce_aspect(
				u128::from(width) * u128::from(pasp.h_spacing),
				u128::from(height) * u128::from(pasp.v_spacing),
			)
			.ok_or(Error::InvalidAspect("invalid or unrepresentable pasp"))?;
			// Square samples add no catalog field, but can still contradict an SPS.
			if pasp.h_spacing != pasp.v_spacing || config.display_aspect_width.is_some() {
				crate::codec::video::fill_display_aspect(&mut config, aspect, "pasp");
			}
		}

		Ok(config)
	}

	/// The avcC stays the description even for avc3, where it may list no parameter
	/// sets: it still carries the NAL length size the samples are framed with.
	fn init_h264(
		&mut self,
		inline: bool,
		avcc: &mp4_atom::Avcc,
		visual: &mp4_atom::Visual,
		container: Container,
	) -> Result<VideoConfig> {
		let mut description = BytesMut::new();
		avcc.encode_body(&mut description)?;
		let description = description.freeze();

		// Start from everything the avcC's SPS signals, so no field is dropped by a copy list.
		let mut config = crate::codec::h264::config(&description)?;
		config.codec = H264 {
			profile: avcc.avc_profile_indication,
			constraints: avcc.profile_compatibility,
			level: avcc.avc_level_indication,
			inline,
		}
		.into();
		config.coded_width = Some(visual.width as _);
		config.coded_height = Some(visual.height as _);
		config.description = Some(description);
		config.container = container;
		Ok(config)
	}

	fn init_h265(
		&mut self,
		in_band: bool,
		hvcc: &mp4_atom::Hvcc,
		visual: &mp4_atom::Visual,
		container: Container,
	) -> Result<VideoConfig> {
		let mut description = BytesMut::new();
		hvcc.encode_body(&mut description)?;

		let mut config = VideoConfig::new(H265 {
			in_band,
			profile_space: hvcc.general_profile_space,
			profile_idc: hvcc.general_profile_idc,
			profile_compatibility_flags: hvcc.general_profile_compatibility_flags,
			tier_flag: hvcc.general_tier_flag,
			level_idc: hvcc.general_level_idc,
			constraint_flags: hvcc.general_constraint_indicator_flags,
		});
		config.description = Some(description.freeze());
		config.coded_width = Some(visual.width as _);
		config.coded_height = Some(visual.height as _);
		crate::codec::h265::apply_hvcc_sps(hvcc, &mut config)?;
		config.container = container;
		Ok(config)
	}

	fn init_audio(&mut self, trak: &Trak, moov: &Moov) -> Result<AudioConfig> {
		let container = self.container(trak, moov)?;
		let stsd = &trak.mdia.minf.stbl.stsd;

		let codec = match stsd.codecs.len() {
			0 => return Err(Error::MissingCodec.into()),
			1 => &stsd.codecs[0],
			_ => return Err(Error::MultipleCodecs.into()),
		};

		let config = match codec {
			mp4_atom::Codec::Mp4a(mp4a) => {
				let desc = &mp4a.esds.es_desc.dec_config;

				// TODO Also support mp4a.67
				if desc.object_type_indication != 0x40 {
					return Err(Error::UnsupportedMpeg2.into());
				}

				// The descriptor's bitrate is optional: a 0 means "unknown", so leave the field unset
				// and let the frame-size detector fill it instead of publishing a bogus 0.
				let bitrate = desc.avg_bitrate.max(desc.max_bitrate);
				let dec_specific = desc.dec_specific.as_ref().ok_or(Error::MissingDecoderSpecific)?;
				let profile = dec_specific.profile;
				let sample_rate = mp4a.audio.sample_rate.integer() as u32;
				let channel_count = mp4a.audio.channel_count as u32;

				// Keep the complete AudioSpecificConfig so SBR/PS extensions reach the decoder.
				let description = bytes::Bytes::copy_from_slice(&dec_specific.raw);

				let mut config = AudioConfig::new(AAC { profile }, sample_rate, channel_count);
				if bitrate > 0 {
					config.bitrate = Some(bitrate.into());
				}
				config.description = Some(description);
				config.container = container;
				config
			}
			mp4_atom::Codec::Opus(opus) => {
				// dOps carries the OpusHead fields in ISOBMFF byte order; republish them as the
				// OpusHead description so decoders apply its pre-skip and gain. mp4-atom already
				// refuses a nonzero channel mapping family, so the table is never dropped here.
				let dops = &opus.dops;
				let mut head =
					crate::codec::opus::Config::new(dops.input_sample_rate, dops.output_channel_count as u32)
						.with_pre_skip(dops.pre_skip);
				head.output_gain = dops.output_gain;

				// The shared conversion sets the decoder's 48 kHz output rate and Opus warmup,
				// whatever the sample entry or the informational input rate claims.
				let description = head.encode()?;
				let mut config: AudioConfig = head.into();
				config.description = Some(description);
				config.container = container;
				config
			}
			mp4_atom::Codec::Flac(flac) => {
				// Rate/channels come from STREAMINFO, not the sample entry's `Audio`
				// box: the latter's sample rate is 16.16 fixed point and can't carry
				// FLAC's high rates (96k/192k). STREAMINFO is also where the decoder
				// description comes from.
				let info = flac
					.dfla
					.blocks
					.iter()
					.find_map(|block| match block {
						mp4_atom::FlacMetadataBlock::StreamInfo {
							minimum_block_size,
							maximum_block_size,
							minimum_frame_size,
							maximum_frame_size,
							sample_rate,
							num_channels_minus_one,
							bits_per_sample_minus_one,
							number_of_interchannel_samples,
							md5_checksum,
						} => Some(crate::codec::flac::Config {
							min_block_size: *minimum_block_size,
							max_block_size: *maximum_block_size,
							min_frame_size: u32::from(*minimum_frame_size),
							max_frame_size: u32::from(*maximum_frame_size),
							sample_rate: *sample_rate,
							channel_count: *num_channels_minus_one as u32 + 1,
							bits_per_sample: *bits_per_sample_minus_one as u32 + 1,
							total_samples: *number_of_interchannel_samples,
							md5: md5_checksum.as_slice().try_into().unwrap_or([0; 16]),
						}),
						_ => None,
					})
					.ok_or(crate::codec::flac::Error::MissingStreamInfo)?;

				let mut config = AudioConfig::new(AudioCodec::Flac, info.sample_rate, info.channel_count);
				config.description = Some(info.description());
				config.container = container;
				config
			}
			mp4_atom::Codec::Unknown(unknown, _) => return Err(Error::UnknownCodec(*unknown).into()),
			unsupported => return Err(Error::UnsupportedCodec(Box::new(unsupported.clone())).into()),
		};

		Ok(config)
	}

	/// Whether the fragments in `moof` open a new segment, and so roll a group on every track.
	///
	/// A segment is the unit a group carries, which is what lets audio (whose samples are all
	/// independently decodable, so it has no boundary of its own) follow the same segmentation as
	/// video instead of opening a group per fragment.
	fn starts_segment(&self, moof: &Moof, has_video: bool) -> bool {
		if self.pending_cut {
			return true;
		}

		if !has_video {
			// Audio-only and unsegmented: nothing in the container says where a segment ends, so
			// fall back to a group per fragment. Bounded, unlike one group for the whole source.
			return true;
		}

		// A video keyframe starts a segment, but only once the current segment already has its
		// video group open. Otherwise this IS that segment's opening fragment, arriving after a
		// `styp` or a `cut()` that already declared the boundary, and bumping a second time would
		// strand video a segment ahead of the audio that rolled on the declaration.
		if !self.video_rolled() {
			return false;
		}

		moof.traf.iter().any(|traf| {
			let Some(track) = self.tracks.get(&traf.tfhd.track_id) else {
				return false;
			};
			track.kind == Kind::Video
				&& traf
					.trun
					.iter()
					.flat_map(|trun| trun.entries.iter())
					.any(|entry| track.defaults.resolve(&traf.tfhd, entry).is_sync())
		})
	}

	/// Whether every video track has already opened its group for the current segment.
	fn video_rolled(&self) -> bool {
		self.tracks
			.values()
			.filter(|track| track.kind == Kind::Video)
			.all(|track| track.segment == Some(self.segment))
	}

	// Publish the samples an mdat carries for each traf of the preceding moof.
	fn extract(&mut self, mdat: Mdat, raw: Bytes) -> Result<()> {
		let has_video = self.tracks.values().any(|t| t.kind == Kind::Video);
		let moof = self.moof.take().ok_or(Error::NoMoof)?;
		let header_size = raw.len() - mdat.data.len();
		// The mdat payload as a view of the input, so each unwrapped sample is sliced, not copied.
		let payload = raw.slice(header_size..);

		// Resolve the segment boundary BEFORE the per-track loop. A moof can carry both an audio
		// and a video traf, and whichever comes first must see the same answer, or the other rolls
		// a fragment late and the two tracks stop agreeing on where segments fall.
		if self.starts_segment(&moof, has_video) {
			self.segment += 1;
			// A boundary the source declared is also an HLS segment boundary; one merely inferred
			// from a keyframe is not, since the default pacing already sees that group open.
			self.declared = self.pending_cut;
			self.pending_cut = false;
		}

		if self.moov.is_none() {
			return Err(Error::NoMoov.into());
		}

		// Offsets include any boxes between moof and mdat, across decode calls too.
		let data_start = self
			.moof_span
			.checked_sub(payload.len())
			.ok_or(Error::InvalidDataOffset)?;
		let mut resolver = sample::Resolver::new(data_start, payload.len());

		// Loop over all of the traf boxes in the moof.
		for traf in &moof.traf {
			let track_id = traf.tfhd.track_id;
			if let Some(track) = self.text.get_mut(&track_id) {
				track.extract(
					super::text::Fragment {
						traf,
						data: &payload,
						resolver: &mut resolver,
					},
					&mut self.timebase,
				)?;
				self.initial_reservation = None;
				continue;
			}
			let track = match self.tracks.get_mut(&track_id) {
				Some(track) => track,
				// A fragment for a track `select` dropped: ignore it.
				None if self.skipped.contains(&track_id) => continue,
				None => return Err(Error::UnknownTrack(track_id).into()),
			};

			// A fragment wholly before the edit or the first sync sample, such as priming samples or
			// a mid-GOP join. Skipped before the segment is counted as opened, or the next keyframe
			// would roll a second segment.
			let Some(run) = track.run(traf, &mut resolver)? else {
				continue;
			};

			// Shift onto the catalog clock. Anchor before releasing the reservation, so the first
			// snapshot carries the final clock; the moov declared every track, so any track's
			// fragment releases it.
			let offset = self.timebase.anchor(run.earliest)?;
			self.initial_reservation = None;

			let new_segment = track.segment != Some(self.segment);
			let placement = Placement {
				offset,
				new_segment,
				// A group opening for a declared segment is an HLS segment boundary on this track.
				declared: new_segment && self.declared,
			};

			let opened = match &mut track.output {
				Output::Passthrough(passthrough) => {
					let fragment = fragment(traf, &moof, &run, &payload, &track.defaults)?;
					passthrough.write(fragment, &run, track.kind, placement)?
				}
				output => unwrap(output, &run, &payload, track.timescale, placement)?,
			};
			// Consuming the boundary on a dependent fragment would let its next sync sample
			// advance the segment again before this track has opened the first one.
			if opened {
				track.segment = Some(self.segment);
			}
		}

		Ok(())
	}
}

/// Where a fragment lands on its track.
#[derive(Clone, Copy)]
struct Placement {
	/// Shifts the source's timestamps onto the catalog clock.
	offset: crate::catalog::Offset,
	/// The fragment is this track's first in the current segment.
	new_segment: bool,
	/// That segment was declared by the source, so it is an HLS segment boundary too.
	declared: bool,
}

impl Placement {
	/// Video opens only on a sync sample; audio follows the source's segment boundary.
	fn keyframe(self, kind: Kind, sample: &Sample) -> bool {
		match kind {
			Kind::Video => sample.sync,
			Kind::Audio => self.new_segment,
		}
	}
}

impl<E: crate::catalog::hang::CatalogExt> Fmp4Track<E> {
	/// Apply this track's edit list to the resolved samples, or return `None` if none remain.
	fn run(&mut self, traf: &mp4_atom::Traf, resolver: &mut sample::Resolver) -> Result<Option<Run>> {
		let tfdt = traf.tfdt.as_ref().ok_or(Error::MissingTfdt)?;
		let timescale = self.timescale;
		let mut dts = tfdt.base_media_decode_time;

		// Every fragment restates its decode time, so a stale one puts two different samples
		// on the same timestamp, which reads downstream as an undeclared hole.
		if let Some(previous) = self.last_decode_time
			&& dts <= previous
		{
			return Err(Error::NonMonotonicDecodeTime {
				track: traf.tfhd.track_id,
				decode_time: Timestamp::new(dts, timescale)?,
				previous: Timestamp::new(previous, timescale)?,
			}
			.into());
		}

		self.last_decode_time = Some(dts);

		let mut earliest = None;
		let mut end = None;
		let mut samples = Vec::new();
		let mut dropped = false;
		for resolved in resolver.resolve(traf, self.defaults)? {
			let sample::Resolved { sample, cts, range } = resolved?;
			let duration = sample.duration;

			// Checked: a negative composition offset must not wrap into a huge u64 PTS.
			let pts = dts.checked_add_signed(cts as i64).ok_or(Error::PtsOverflow)?;
			let duration_ts = duration
				.map(|duration| Timestamp::new(duration as u64, timescale))
				.transpose()?;
			if let Some(duration) = duration_ts {
				self.sample_duration = Some(self.sample_duration.map_or(duration, |min| min.min(duration)));
			}

			let presented = self.edit.present(pts)?;
			if sample.is_sync() {
				self.orphaned = presented.is_none();
			}
			if let Some(presented) = presented.filter(|_| !self.orphaned) {
				let timestamp = Timestamp::new(presented, timescale)?;
				let sample_end = match duration_ts.or(self.sample_duration) {
					Some(duration) => timestamp.checked_add(duration)?,
					None => timestamp,
				};
				end = Some(end.map_or(sample_end, |max: Timestamp| max.max(sample_end)));
				earliest = Some(earliest.map_or(timestamp, |min: Timestamp| min.min(timestamp)));

				samples.push(Sample {
					dts,
					pts,
					timestamp,
					duration,
					flags: sample.flags,
					sync: sample.is_sync(),
					range,
				});
			} else {
				dropped = true;
			}

			if let Some(duration) = duration {
				dts = dts.checked_add(duration as u64).ok_or(Error::PtsOverflow)?;
			}
		}

		let (Some(earliest), Some(end)) = (earliest, end) else {
			return match dropped {
				true => Ok(None),
				false => Err(Error::MissingTrun.into()),
			};
		};
		Ok(Some(Run {
			samples,
			earliest,
			end,
			dropped,
		}))
	}
}

/// Re-encode `traf`'s samples as a standalone moof+mdat fragment addressing only its own bytes.
///
/// The mdat holds `run`'s samples back to back in decode order, whatever separated them in the
/// source (padding, another track's runs), so each rewritten `data_offset` is the sizes before it.
fn fragment(
	traf: &mp4_atom::Traf,
	moof: &Moof,
	run: &Run,
	payload: &Bytes,
	defaults: &sample::Defaults,
) -> Result<Bytes> {
	// Build a per-track moof containing only this traf, and a per-track mdat
	// with only the samples belonging to this track.
	let mut adjusted_moof = Moof {
		mfhd: moof.mfhd.clone(),
		traf: vec![match run.dropped {
			true => kept(traf, run)?,
			false => traf.clone(),
		}],
	};

	// Apply structural (flag-presence) changes BEFORE measuring the encoded
	// size, otherwise the rewritten data_offset values are computed from a
	// stale size and the resulting fragment misaddresses sample data.
	// Ensuring trun.data_offset is Some(...) reserves 4 bytes per trun.
	for traf_mut in &mut adjusted_moof.traf {
		// A zero default/sample duration is "unknown", not "instantaneous": drop it so
		// the re-emitted fragment carries no bogus zero that a decoder would honor.
		if traf_mut.tfhd.default_sample_duration == Some(0) {
			traf_mut.tfhd.default_sample_duration = None;
		}
		for trun_mut in &mut traf_mut.trun {
			// Reserve the data_offset field; the real value is filled in below.
			trun_mut.data_offset = Some(0);
			for entry in &mut trun_mut.entries {
				if entry.duration == Some(0) {
					entry.duration = None;
				}
			}
		}
	}

	let mut moof_buf = Vec::new();
	adjusted_moof.encode(&mut moof_buf)?;
	let new_moof_size = moof_buf.len();

	// Re-encode moof with corrected per-trun data_offset for the per-track fragment.
	// Each trun's data_offset points to the start of that run's data within the new mdat.
	let mdat_header_size_new = 8u64; // 4 bytes size + 4 bytes 'mdat'
	let mut cumulative_offset = 0u64;
	for traf_mut in &mut adjusted_moof.traf {
		for trun_mut in &mut traf_mut.trun {
			trun_mut.data_offset = Some((new_moof_size as u64 + mdat_header_size_new + cumulative_offset) as i32);

			// Advance past this trun's sample data
			let trun_data_size: u64 = trun_mut
				.entries
				.iter()
				.map(|entry| u64::from(defaults.resolve(&traf_mut.tfhd, entry).size))
				.sum();
			cumulative_offset += trun_data_size;
		}
	}

	moof_buf.clear();
	adjusted_moof.encode(&mut moof_buf)?;

	// Adjacent samples are copied as one span.
	let mut data = Vec::with_capacity(run.samples.iter().map(|sample| sample.range.len()).sum());
	let mut span: Option<std::ops::Range<usize>> = None;
	for sample in &run.samples {
		match &mut span {
			Some(span) if span.end == sample.range.start => span.end = sample.range.end,
			_ => {
				if let Some(span) = span.replace(sample.range.clone()) {
					data.extend_from_slice(&payload[span]);
				}
			}
		}
	}
	if let Some(span) = span {
		data.extend_from_slice(&payload[span]);
	}
	Mdat { data }.encode(&mut moof_buf)?;

	Ok(Bytes::from(moof_buf))
}

/// `traf` restated as one run of `run`'s samples, for a fragment the edit list dropped some of.
///
/// Each sample keeps its decode and composition time: `tfdt` starts at the first kept sample, and
/// each kept sample lasts until the next one decodes. The per-sample tables (`sbgp`,
/// `subs`, sample auxiliary information) would index the wrong samples, and no consumer reads
/// them, so they go.
fn kept(traf: &mp4_atom::Traf, run: &Run) -> Result<mp4_atom::Traf> {
	let mut entries = Vec::with_capacity(run.samples.len());
	for (index, sample) in run.samples.iter().enumerate() {
		let duration = match run.samples.get(index + 1) {
			Some(next) => Some(u32::try_from(next.dts - sample.dts).map_err(|_| Error::PtsOverflow)?),
			None => sample.duration,
		};
		let cts = i128::from(sample.pts) - i128::from(sample.dts);
		entries.push(mp4_atom::TrunEntry {
			duration,
			size: Some(sample.range.len() as u32),
			flags: Some(sample.flags),
			cts: Some(i32::try_from(cts).map_err(|_| Error::PtsOverflow)?),
		});
	}
	let first = run.samples.first().ok_or(Error::MissingTrun)?;
	Ok(mp4_atom::Traf {
		tfhd: traf.tfhd.clone(),
		tfdt: Some(mp4_atom::Tfdt {
			base_media_decode_time: first.dts,
		}),
		trun: vec![mp4_atom::Trun {
			data_offset: None,
			entries,
		}],
		..Default::default()
	})
}

impl<E: crate::catalog::hang::CatalogExt> Passthrough<E> {
	/// Write one per-track `fragment` as a single frame, reporting whether it opened a group.
	fn write(&mut self, fragment: Bytes, run: &Run, kind: Kind, placement: Placement) -> Result<bool> {
		// Carry the fragment's earliest presentation time as the frame timestamp, in the
		// track's native timescale and shifted onto the catalog clock. Consumers present the
		// fragment at it; `tfdt` only places the samples relative to each other.
		let timestamp = placement.offset.apply(run.earliest)?;
		let end = placement.offset.apply(run.end)?;

		// Audio follows segment boundaries; video also waits for a sync first sample so a new
		// group's first fragment can be decoded independently.
		let keyframe = placement.keyframe(kind, run.samples.first().ok_or(Error::MissingTrun)?);
		let start_group = self.group.is_none() || (placement.new_segment && keyframe);
		// A group may not start below the one before it. Refused before the open group closes or
		// a new one opens, so a rewound fragment publishes nothing, as an unwrapped track does.
		if start_group
			&& let Some(floor) = self.start
			&& timestamp < floor
		{
			return Err(crate::container::TimestampRewind { timestamp, floor }.into());
		}
		let mut group = if start_group {
			if let Some(mut prev) = self.group.take() {
				prev.finish()?;
			}
			match self.pending_sequence.take() {
				Some(sequence) => self.track.create_group(moq_net::group::Info { sequence })?,
				None => self.track.append_group()?,
			}
		} else {
			self.group.take().ok_or(Error::NoKeyframe)?
		};

		let sequence = group.sequence;

		if start_group {
			// A group reopened mid-segment (after a `seek`) is no segment boundary.
			if placement.declared
				&& let Some(recorder) = self.recorder.as_mut()
			{
				recorder.cut(timestamp);
			}

			// Close the previous group for the bitrate estimator (used only when the
			// descriptor didn't declare a bitrate).
			self.estimator.cut(Some(timestamp));
		}
		let fragment_len = fragment.len();
		group.write_frame(timestamp, fragment)?;
		if start_group {
			self.start = Some(timestamp);
		}

		// Audio is always independently decodable; video counts the IDRs it carries.
		let keyframes = match kind {
			Kind::Video => run.samples.iter().filter(|sample| sample.sync).count() as u64,
			Kind::Audio => 0,
		};
		// Only once published, so a rejected fragment is never listed.
		if let Some(recorder) = self.recorder.as_mut() {
			recorder.frame(sequence, timestamp);
		}

		self.group = Some(group);

		self.estimator.write(timestamp, fragment_len);
		let bytes = run.samples.iter().map(|sample| sample.range.len()).sum();
		self.sent.add(run.samples.len() as u64, bytes, keyframes);
		if let Some(recorder) = self.recorder.as_mut() {
			recorder.end(end);
		}
		self.estimator.burst(end.checked_sub(timestamp)?.into());
		self.publish_estimate()?;
		Ok(start_group)
	}
}

/// Write each sample of `run` as its own frame on an unwrapped track.
fn unwrap<E: crate::catalog::hang::CatalogExt>(
	output: &mut Output<E>,
	run: &Run,
	payload: &Bytes,
	timescale: moq_net::Timescale,
	placement: Placement,
) -> Result<bool> {
	let earliest = placement.offset.apply(run.earliest)?;
	let end = placement.offset.apply(run.end)?;
	let kind = if matches!(output, Output::Video(_)) {
		Kind::Video
	} else {
		Kind::Audio
	};

	media!(output, producer => {
		let mut opened = false;
		for (index, sample) in run.samples.iter().enumerate() {
			let keyframe = match kind {
				Kind::Video => placement.keyframe(kind, sample),
				Kind::Audio => (index == 0 && placement.keyframe(kind, sample)) || producer.needs_keyframe(),
			};
			if keyframe {
				if !opened && placement.declared {
					producer.boundary(earliest);
				}
				opened = true;
			}
			if sample.pts > sample.dts {
				producer.reorder(Timestamp::new(sample.pts - sample.dts, timescale)?);
			}
			let frame = crate::container::Frame {
				timestamp: placement.offset.apply(sample.timestamp)?,
				payload: payload.slice(sample.range.clone()),
				keyframe,
				duration: sample
					.duration
					.map(|duration| Timestamp::new(duration as u64, timescale))
					.transpose()?,
			};
			producer.write(frame)?;
		}

		// The fragment arrived at once, so a consumer has to buffer its whole span.
		producer.burst(end.checked_sub(earliest)?.into())?;
		Ok(opened)
	}, _ => unreachable!("passthrough tracks write whole fragments"))
}

impl<E: crate::catalog::hang::CatalogExt> Import<E> {
	/// Finish all tracks, flushing current groups.
	pub fn finish(&mut self) -> Result<()> {
		// No input follows, so the buffer that waited for the rest of an atom is done.
		self.buffer = BytesMut::new();
		for track in self.text.values_mut() {
			track.producer.finish()?;
		}
		// No frame follows to anchor the clock, so publish the declared track set now.
		self.initial_reservation = None;
		for track in self.tracks.values_mut() {
			media!(&mut track.output, producer => producer.finish()?, passthrough => passthrough.finish()?);
		}
		Ok(())
	}

	/// Abort all tracks with `err` instead of finishing, so subscribers see the real
	/// cause rather than [`moq_net::Error::Dropped`]. Consumes the importer.
	///
	/// Unlike a drop, this leaves the tracks' HLS records to the catalog, whose abort fails them
	/// rather than ending them as a complete recording.
	pub fn abort(mut self, err: moq_net::Error) {
		for track in std::mem::take(&mut self.text).into_values() {
			track.producer.abort(err.clone());
		}
		// Dropping each track's rendition retires its catalog entry.
		for track in std::mem::take(&mut self.tracks).into_values() {
			media!(track.output, producer => producer.abort(err.clone()), mut passthrough => {
				if let Some(group) = passthrough.group.take() {
					let _ = group.abort(err.clone());
				}
				let _ = passthrough.track.abort(err.clone());
			});
		}
	}

	/// Close the current group on every track and open the next one at `sequence`.
	///
	/// Broadcast-wide: every track inside this fMP4 import advances together; per-track
	/// control is intentionally not exposed. Skipping sequences is not a new timeline, so the
	/// next fragment must still advance past the last decode time; see
	/// [`discontinuity`](Self::discontinuity). Text spends a group per cue, so a text track that
	/// is already past `sequence` continues from its own next group instead. Video resumes at its
	/// next sync sample, dropping the samples before it.
	pub fn seek(&mut self, sequence: u64) -> Result<()> {
		for track in self.text.values_mut() {
			let next = track.producer.next_sequence();
			track.producer.seek(sequence.max(next))?;
		}
		for track in self.tracks.values_mut() {
			// The next group opens on a sync sample, so video drops the samples before one.
			if track.kind == Kind::Video {
				track.orphaned = true;
			}
			media!(&mut track.output, producer => producer.seek(sequence)?, passthrough => {
				passthrough.close()?;
				passthrough.pending_sequence = Some(sequence);
			});
		}
		Ok(())
	}

	/// The source signalled a new timeline (an HLS `EXT-X-DISCONTINUITY`), so the next
	/// fragment's decode time is not compared with the last one on each track. A timeline that
	/// restarts below where it was still fails with
	/// [`TimestampRewind`](crate::container::TimestampRewind): a track refuses a group that
	/// rewinds, before publishing any of it.
	pub fn discontinuity(&mut self) -> Result<()> {
		for track in self.text.values_mut() {
			track.end_decode_time = None;
			track.discontinuity = true;
		}
		for track in self.tracks.values_mut() {
			track.last_decode_time = None;
		}
		Ok(())
	}
}

impl<E: crate::catalog::hang::CatalogExt> Drop for Import<E> {
	/// End each track, then retire its HLS records: every track takes a fresh name, so nothing
	/// publishes it again, as when an HLS import replaces this init segment's importer.
	fn drop(&mut self) {
		for track in self.text.values_mut() {
			if let Err(err) = track.producer.end() {
				tracing::warn!(%err, "failed to end a dropped fMP4 text track");
			}
		}
		for mut track in std::mem::take(&mut self.tracks).into_values() {
			let (name, ended) = media!(&mut track.output,
				producer => (producer.name().to_string(), producer.end()),
				passthrough => (passthrough.track.name().to_string(), passthrough.end()));
			drop(track);
			if let Err(err) = ended {
				tracing::warn!(%err, "failed to end a dropped fMP4 track");
			}
			self.catalog.retire_dropped(&name);
		}
	}
}
