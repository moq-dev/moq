//! Importing a regular MP4 fed in chunks.

use std::collections::VecDeque;

use bytes::{Bytes, BytesMut};
use hang::catalog::{AudioConfig, Container, VideoConfig};
use mp4_atom::Atom;

use super::index::{self, Budget, Config, Presented, Presenter, Sample};
use crate::Result;
use crate::catalog::hang::CatalogExt;
use crate::container::Frame;
use crate::container::fmp4::{self, Error};

/// Imports a regular MP4, fed in chunks split anywhere, into a MoQ broadcast.
///
/// Each sample becomes one [`Legacy`](Container::Legacy) frame, as the fMP4 import unwraps a
/// fragment's, published once its last byte arrives, with its track's edit list applied as
/// [`Sample`] describes. The tracks and codecs are those a [`Reader`](super::Reader) lists. A
/// fragmented file, whose `moov` declares `mvex`, is handed whole to [`fmp4::Import`], so this
/// reads any MP4.
///
/// A regular MP4 says where its samples lie only in its `moov`. With the `moov` first, as a file
/// written for streaming has it, each sample publishes as it arrives and the import holds no more
/// than the sample arriving. With the `moov` last, as most recorders leave it, the import holds the
/// media until the `moov` arrives, up to 64 MiB, and refuses more with
/// [`MediaBeforeMoov`](fmp4::Error::MediaBeforeMoov). A [`Reader`](super::Reader) reads such a file
/// from a seekable source instead, seeking to the `moov`.
///
/// Samples publish in file order, so one that starts before the previous one ends, such as a track
/// stored out of decode order or two samples sharing bytes, is refused with
/// [`BackwardsDataOffset`](fmp4::Error::BackwardsDataOffset). An empty sample is skipped.
pub struct Import<E: CatalogExt = ()> {
	broadcast: moq_net::broadcast::Producer,
	catalog: crate::catalog::Producer<E>,

	/// Held until the first sample is published, so the catalog is withheld until every rendition
	/// is in and its root `clock` is final.
	initial_reservation: Option<crate::catalog::Reserved<E>>,

	/// The file's timestamp base: the earliest sample anchors it, and every sample shifts by its
	/// offset onto the catalog clock.
	timebase: crate::catalog::Timebase<E>,

	/// Whether input is still accepted.
	input: crate::container::Input,

	state: State<E>,
}

enum State<E: CatalogExt> {
	/// Reading the top-level boxes of a regular MP4.
	Plain(Box<Plain>),
	/// A fragmented MP4, read by the fMP4 import.
	Fragmented(Box<fmp4::Import<E>>),
}

/// The top-level boxes of a regular MP4, read as they arrive.
#[derive(Default)]
struct Plain {
	/// The file offset of the next input byte.
	position: u64,
	/// The box being read.
	body: Body,
	/// The media held for samples still to publish, by file offset.
	held: VecDeque<Held>,
	/// The media held before the `moov` arrived.
	pending: usize,
	/// Every track the `moov` declared, once it arrived.
	tracks: Option<Vec<Output>>,
	/// Where the last published sample ends in the file.
	published: u64,
	/// The latest video keyframe published, on the catalog clock, which audio groups follow.
	keyframe: Option<moq_net::Timestamp>,
	/// Whether the file has a video track for audio groups to follow.
	video: bool,
}

/// What the importer does with the box being read.
enum Body {
	/// Read a box header, of which these bytes arrived.
	Header(Vec<u8>),
	/// Hold a `moov`, header included, until its `size` bytes arrived.
	Moov { data: Vec<u8>, size: usize },
	/// Hold media for samples, until the file offset where the box ends or the end of the input.
	Media(Option<u64>),
	/// Discard the box, which ends at this file offset or with the input.
	Skip(Option<u64>),
}

impl Default for Body {
	fn default() -> Self {
		Self::Header(Vec::new())
	}
}

/// Media bytes held from file offset `start`.
struct Held {
	start: u64,
	data: BytesMut,
}

impl Held {
	fn end(&self) -> u64 {
		self.start + self.data.len() as u64
	}
}

/// One published track and the samples it has still to publish.
struct Output {
	media: Media,
	timescale: moq_net::Timescale,
	/// The samples to publish, in decode order.
	samples: VecDeque<Sample>,
	/// The video keyframe an audio track's open group follows.
	follows: Option<moq_net::Timestamp>,
	/// Where an audio track's open group starts, and the frames it holds, in a file without video.
	group: Option<(moq_net::Timestamp, u32)>,
}

/// How long an audio group spans in a file without video, which has no keyframes for the audio
/// to follow.
const AUDIO_GROUP: std::time::Duration = std::time::Duration::from_secs(1);

/// The fewest frames an audio group holds in a file without video, so a file of tiny frames a
/// second apart costs no group per byte.
const AUDIO_GROUP_FRAMES: u32 = 8;

enum Media {
	Video(crate::container::Producer<crate::catalog::hang::Container, VideoConfig>),
	Audio(crate::container::Producer<crate::catalog::hang::Container, AudioConfig>),
}

impl Media {
	/// Whether no group is open, so the next frame opens one.
	fn needs_keyframe(&self) -> bool {
		match self {
			Self::Video(track) => track.needs_keyframe(),
			Self::Audio(track) => track.needs_keyframe(),
		}
	}

	fn write(&mut self, frame: Frame) -> Result<()> {
		match self {
			Self::Video(track) => track.write(frame),
			Self::Audio(track) => track.write(frame),
		}
	}

	fn seek(&mut self, start: u64) -> Result<()> {
		match self {
			Self::Video(track) => track.seek(start),
			Self::Audio(track) => track.seek(start),
		}
	}

	fn end(&mut self) -> Result<()> {
		match self {
			Self::Video(track) => track.end(),
			Self::Audio(track) => track.end(),
		}
	}

	fn abort(self, err: moq_net::Error) {
		match self {
			Self::Video(track) => track.abort(err),
			Self::Audio(track) => track.abort(err),
		}
	}
}

/// The size of a box header that starts with `header`: 16 bytes with a 64-bit size, else 8.
fn header_len(header: &[u8]) -> usize {
	match header.get(..4) {
		Some([0, 0, 0, 1]) => 16,
		_ => 8,
	}
}

impl<E: CatalogExt> Import<E> {
	/// Import a byte stream into the broadcast using its reserved catalog producer.
	pub fn new(broadcast: moq_net::broadcast::Producer, reserved: crate::catalog::Reserved<E>) -> Self {
		Self {
			broadcast,
			catalog: reserved.producer(),
			timebase: reserved.timebase(),
			initial_reservation: Some(reserved),
			input: crate::container::Input::Open,
			state: State::Plain(Box::default()),
		}
	}

	/// Parse `data`, a chunk of the file split anywhere.
	///
	/// A refused chunk fails the import: later input is refused with the same error, and
	/// [`finish`](Self::finish) ends the tracks with what was published.
	pub fn decode(&mut self, data: &[u8]) -> Result<()> {
		self.input.check()?;
		let result = self.drain(data);
		if let Err(err) = &result {
			self.input = crate::container::Input::Refused(err.clone());
		}
		result
	}

	fn drain(&mut self, mut data: &[u8]) -> Result<()> {
		while !data.is_empty() {
			let plain = match &mut self.state {
				State::Plain(plain) => plain,
				State::Fragmented(import) => return import.decode(data),
			};
			match &mut plain.body {
				Body::Header(header) => {
					let count = (header_len(header) - header.len()).min(data.len());
					header.extend_from_slice(&data[..count]);
					data = &data[count..];
					plain.position += count as u64;
					// The first 8 bytes may say that 8 more hold the size.
					if header.len() == header_len(header) {
						let header = std::mem::take(header);
						plain.begin(&header)?;
					}
				}
				Body::Moov { data: moov, size } => {
					let count = (*size - moov.len()).min(data.len());
					moov.extend_from_slice(&data[..count]);
					data = &data[count..];
					plain.position += count as u64;
					if moov.len() == *size {
						let moov = std::mem::take(moov);
						plain.body = Body::default();
						self.init(&moov)?;
					}
				}
				Body::Media(end) | Body::Skip(end) => {
					let end = *end;
					let media = matches!(plain.body, Body::Media(_));
					let start = plain.position;
					let count = match end {
						Some(end) => usize::try_from(end - start).unwrap_or(usize::MAX).min(data.len()),
						None => data.len(),
					};
					let (bytes, rest) = data.split_at(count);
					data = rest;
					plain.position += count as u64;
					if plain.body.ended(plain.position) {
						plain.body = Body::default();
					}
					if media {
						plain.hold(start, bytes)?;
						self.publish()?;
					}
				}
			}
		}
		Ok(())
	}

	/// Read the `moov`, header included: declare its tracks and publish the media already held,
	/// or hand a fragmented file to the fMP4 import.
	fn init(&mut self, data: &[u8]) -> Result<()> {
		let moov = index::decode_moov(data)?;
		if moov.mvex.is_some() {
			let reserved = self.initial_reservation.take().ok_or(crate::Error::Finished)?;
			let mut import = fmp4::Import::new(self.broadcast.clone(), reserved);
			import.decode(data)?;
			self.state = State::Fragmented(Box::new(import));
			return Ok(());
		}
		if moov.trak.len() > fmp4::MAX_TRACKS {
			return Err(Error::TooManyTracks(moov.trak.len()).into());
		}
		let reserved = self.initial_reservation.clone().ok_or(crate::Error::Finished)?;

		// The tables declare at most one sample per byte of the moov past a small base: a sample
		// costs its size table at least four bytes, unless every sample has the same size.
		let mut budget = Budget(data.len() as u64 + fmp4::FRAGMENT_SAMPLE_BASE);
		let mut tracks = Vec::new();
		let mut earliest = None;
		for track in index::declare(&moov, &mut budget, None)? {
			let mut presenter = Presenter::new(track.edit);
			let mut samples = VecDeque::with_capacity(track.samples.len());
			for raw in track.samples {
				match presenter.present(raw)? {
					Presented::Sample(sample) => samples.push_back(sample),
					Presented::Skipped => {}
					Presented::Ended => break,
				}
			}
			if let Some(first) = samples.iter().map(|sample| sample.pts).min() {
				let first = moq_net::Timestamp::new(first, track.timescale)?;
				earliest = Some(earliest.map_or(first, |earliest| crate::container::earlier(earliest, first)));
			}

			let (suffix, kind, priority) = match &track.config {
				Config::Video(_) => (".mp4-v", crate::container::Kind::Video, hang::catalog::PRIORITY.video),
				Config::Audio(_) => (".mp4-a", crate::container::Kind::Audio, hang::catalog::PRIORITY.audio),
			};
			let output = self.broadcast.unique_track(suffix, self.catalog.track_info(priority));
			let wire = crate::catalog::hang::Container::new(&Container::Legacy, kind)?;
			let media = match track.config {
				Config::Video(config) => Media::Video(reserved.video(output, wire, config)?),
				Config::Audio(config) => Media::Audio(reserved.audio(output, wire, config)?),
			};
			tracks.push(Output {
				media,
				timescale: track.timescale,
				samples,
				follows: None,
				group: None,
			});
		}

		match earliest {
			// Anchor on the earliest sample, so none shifts before the catalog clock's zero.
			Some(earliest) => {
				self.timebase.anchor(earliest)?;
			}
			// No sample follows to anchor the clock, so publish the track set now.
			None => self.initial_reservation = None,
		}
		let State::Plain(plain) = &mut self.state else {
			unreachable!("a regular MP4 reads its own moov");
		};
		plain.video = tracks.iter().any(|track| matches!(track.media, Media::Video(_)));
		plain.tracks = Some(tracks);
		plain.trim();
		self.publish()
	}

	/// Publish every sample whose bytes arrived, in file order.
	fn publish(&mut self) -> Result<()> {
		let State::Plain(plain) = &mut self.state else {
			return Ok(());
		};
		let Some(tracks) = &mut plain.tracks else {
			return Ok(());
		};
		while let Some(index) = next(tracks) {
			let track = &mut tracks[index];
			let sample = *track.samples.front().expect("the next track has a sample");
			if sample.offset < plain.published {
				return Err(Error::BackwardsDataOffset.into());
			}
			let end = sample
				.offset
				.checked_add(u64::from(sample.size))
				.ok_or(Error::InvalidDataOffset)?;
			if end > plain.position {
				break;
			}
			track.samples.pop_front();
			plain.published = end;
			if sample.size == 0 {
				continue;
			}
			// The held media is in file order, without overlaps.
			let index = plain.held.partition_point(|held| held.end() <= sample.offset);
			let held = plain
				.held
				.get(index)
				.filter(|held| held.start <= sample.offset && end <= held.end())
				.ok_or(Error::SampleOutsideMedia {
					offset: sample.offset,
					size: sample.size,
				})?;
			let at = (sample.offset - held.start) as usize;
			let payload = Bytes::copy_from_slice(&held.data[at..at + sample.size as usize]);

			let timestamp = self.timebase.shift(sample.timestamp(track.timescale)?)?;
			self.initial_reservation = None;
			let duration = match sample.duration {
				0 => None,
				duration => Some(moq_net::Timestamp::new(u64::from(duration), track.timescale)?),
			};
			// Audio groups follow the video's, as an fMP4 import's follow its segments: an audio
			// sample opens a group once the video opened one at or before it. In a file without video,
			// an audio group spans a second.
			let keyframe = match &track.media {
				Media::Video(_) => {
					if sample.keyframe {
						plain.keyframe = Some(timestamp);
					}
					sample.keyframe
				}
				Media::Audio(_) if !plain.video => {
					let spanned = |start| match (crate::container::time(start), crate::container::time(timestamp)) {
						(Some(start), Some(at)) => at.saturating_sub(start) >= AUDIO_GROUP,
						_ => false,
					};
					let opens = track.media.needs_keyframe()
						|| track
							.group
							.is_none_or(|(start, frames)| frames >= AUDIO_GROUP_FRAMES && spanned(start));
					match &mut track.group {
						Some((_, frames)) if !opens => *frames += 1,
						group => *group = Some((timestamp, 1)),
					}
					opens
				}
				Media::Audio(_) => {
					let follows = plain
						.keyframe
						.filter(|&keyframe| keyframe <= timestamp && Some(keyframe) != track.follows);
					if follows.is_some() {
						track.follows = follows;
					}
					follows.is_some() || track.media.needs_keyframe()
				}
			};
			track.media.write(Frame {
				timestamp,
				duration,
				payload,
				keyframe,
			})?;
		}
		plain.trim();
		Ok(())
	}

	/// Close every track's group and open the next one at `start`.
	pub fn seek(&mut self, start: u64) -> Result<()> {
		self.input.check()?;
		match &mut self.state {
			State::Fragmented(import) => import.seek(start),
			State::Plain(plain) => {
				for track in plain.tracks.iter_mut().flatten() {
					track.media.seek(start)?;
				}
				Ok(())
			}
		}
	}

	/// Finish every track and return the [`Finished`] import.
	///
	/// Input that ends before the `moov`, inside it, or before a sample it lists is refused, but
	/// the tracks still end with what was published, as they do after a refused
	/// [`decode`](Self::decode). Either way every track has ended, so a failed finish can't be
	/// retried.
	pub fn finish(mut self) -> Result<Finished<E>> {
		let inner = match std::mem::replace(&mut self.state, State::Plain(Box::default())) {
			State::Fragmented(import) => Inner::Fragmented(Box::new(import.finish()?)),
			plain => {
				self.state = plain;
				self.finish_plain()?;
				Inner::Plain(Box::new(self))
			}
		};
		Ok(Finished { inner })
	}

	fn finish_plain(&mut self) -> Result<()> {
		let mut result = match &self.input {
			crate::container::Input::Finished => return Ok(()),
			crate::container::Input::Failed(err) => return Err(err.clone()),
			crate::container::Input::Refused(_) => Ok(()),
			crate::container::Input::Open => self.complete(),
		};
		self.initial_reservation = None;
		if let State::Plain(plain) = &mut self.state {
			for track in plain.tracks.iter_mut().flatten() {
				// End every track even on refusal, keeping the first error.
				result = result.and(track.media.end());
			}
		}
		match &result {
			// A refusal keeps refusing input with its own cause.
			Ok(()) if matches!(self.input, crate::container::Input::Refused(_)) => {}
			Ok(()) => self.input = crate::container::Input::Finished,
			Err(err) => self.input = crate::container::Input::Failed(err.clone()),
		}
		result
	}

	/// Check that the input held the whole file: its `moov`, and every sample it lists.
	fn complete(&self) -> Result<()> {
		let State::Plain(plain) = &self.state else {
			return Ok(());
		};
		match &plain.body {
			Body::Moov { .. } => return Err(Error::BoxPastEnd("moov".to_string()).into()),
			Body::Header(header) if !header.is_empty() => {
				tracing::debug!(bytes = header.len(), "skipping the bytes after the last box");
			}
			_ => {}
		}
		let tracks = plain.tracks.as_ref().ok_or(Error::NoMoov)?;
		match tracks
			.iter()
			.filter_map(|track| track.samples.front())
			.min_by_key(|sample| sample.offset)
		{
			Some(sample) => Err(Error::SampleOutsideMedia {
				offset: sample.offset,
				size: sample.size,
			}
			.into()),
			None => Ok(()),
		}
	}

	/// Abort every track with `err` instead of finishing, so subscribers see the real cause rather
	/// than [`moq_net::Error::Dropped`]. Consumes the importer.
	pub fn abort(mut self, err: moq_net::Error) {
		match std::mem::replace(&mut self.state, State::Plain(Box::default())) {
			State::Fragmented(import) => import.abort(err),
			State::Plain(mut plain) => {
				for track in plain.tracks.take().into_iter().flatten() {
					track.media.abort(err.clone());
				}
			}
		}
	}
}

impl Plain {
	/// Start the box whose whole `header` arrived.
	fn begin(&mut self, header: &[u8]) -> Result<()> {
		let kind = mp4_atom::FourCC::new(&[header[4], header[5], header[6], header[7]]);
		let total = match u32::from_be_bytes([header[0], header[1], header[2], header[3]]) {
			0 => None,
			1 => Some(u64::from_be_bytes(header[8..16].try_into().expect("eight bytes"))),
			size => Some(u64::from(size)),
		};
		// The body's size, or `None` for a box running to the end of the file.
		let body = match total {
			Some(total) => Some(
				total
					.checked_sub(header.len() as u64)
					.ok_or(Error::mp4(mp4_atom::Error::InvalidSize))?,
			),
			None => None,
		};
		let end = body.map(|body| self.position.saturating_add(body));
		self.body = match kind {
			mp4_atom::Moov::KIND => {
				if self.tracks.is_some() {
					return Err(Error::DuplicateMoov.into());
				}
				let total = total.ok_or(Error::mp4(mp4_atom::Error::InvalidSize))?;
				if total > fmp4::MAX_MOOV_BYTES {
					return Err(Error::MoovTooLarge(total).into());
				}
				// Grown as it arrives, so a header alone allocates nothing it declares.
				let data = header.to_vec();
				Body::Moov {
					data,
					size: total as usize,
				}
			}
			mp4_atom::Mdat::KIND => Body::Media(end),
			_ => Body::Skip(end),
		};
		if self.body.ended(self.position) {
			self.body = Body::default();
		}
		Ok(())
	}

	/// Hold the media in `bytes`, which start at file offset `start`, for the samples to publish.
	fn hold(&mut self, start: u64, bytes: &[u8]) -> Result<()> {
		let (start, bytes) = match &self.tracks {
			// Before the moov, any of it may be a sample.
			None => {
				self.pending += bytes.len();
				if self.pending > fmp4::MAX_PENDING_BYTES {
					return Err(Error::MediaBeforeMoov.into());
				}
				(start, bytes)
			}
			// After it, only what the next sample or a later one takes.
			Some(tracks) => match next(tracks) {
				Some(index) => {
					let floor = tracks[index].samples[0].offset;
					let skip = usize::try_from(floor.saturating_sub(start)).unwrap_or(usize::MAX);
					match bytes.get(skip..) {
						Some(bytes) => (start.max(floor), bytes),
						None => return Ok(()),
					}
				}
				None => return Ok(()),
			},
		};
		if bytes.is_empty() {
			return Ok(());
		}
		match self.held.back_mut() {
			Some(last) if last.end() == start => last.data.extend_from_slice(bytes),
			_ => self.held.push_back(Held {
				start,
				data: BytesMut::from(bytes),
			}),
		}
		Ok(())
	}

	/// Drop the held media before the next sample to publish, which no later one takes.
	fn trim(&mut self) {
		let Some(tracks) = &self.tracks else {
			return;
		};
		let floor = next(tracks).map_or(u64::MAX, |index| tracks[index].samples[0].offset);
		while let Some(held) = self.held.front_mut() {
			if held.end() <= floor {
				self.held.pop_front();
				continue;
			}
			if held.start < floor {
				let _ = held.data.split_to((floor - held.start) as usize);
				held.start = floor;
			}
			break;
		}
	}
}

impl Body {
	/// Whether the box ended at file offset `position`.
	fn ended(&self, position: u64) -> bool {
		matches!(self, Self::Media(Some(end)) | Self::Skip(Some(end)) if *end <= position)
	}
}

/// The track whose next sample comes first in the file.
fn next(tracks: &[Output]) -> Option<usize> {
	tracks
		.iter()
		.enumerate()
		.filter_map(|(index, track)| Some((index, track.samples.front()?.offset)))
		.min_by_key(|&(_, offset)| offset)
		.map(|(index, _)| index)
}

/// What [`Finished`] holds.
enum Inner<E: CatalogExt> {
	Plain(Box<Import<E>>),
	Fragmented(Box<fmp4::Finished<E>>),
}

/// An MP4 import that [finished](Import::finish): every track has ended, and they can still be
/// read or aborted, but it takes no more input.
///
/// Dropping it retires the tracks and their catalog renditions, as dropping the [`Import`] does.
///
/// ```compile_fail,E0599
/// # fn cannot_decode(mut import: moq_mux::container::mp4::Finished) {
/// import.decode(b"").unwrap();
/// # }
/// ```
#[must_use = "dropping it retires the tracks and their catalog renditions"]
pub struct Finished<E: CatalogExt = ()> {
	inner: Inner<E>,
}

impl<E: CatalogExt> Finished<E> {
	/// Abort every track with `err`; see [`Import::abort`]. Readers still draining a track get its
	/// clean end instead once every group below its end finished.
	pub fn abort(self, err: moq_net::Error) {
		match self.inner {
			Inner::Plain(import) => import.abort(err),
			Inner::Fragmented(finished) => finished.abort(err),
		}
	}
}

impl<E: CatalogExt> Drop for Import<E> {
	/// End each track, then retire its HLS records: every track takes a fresh name, so nothing
	/// publishes it again.
	fn drop(&mut self) {
		let State::Plain(plain) = &mut self.state else {
			return;
		};
		for mut track in plain.tracks.take().into_iter().flatten() {
			let ended = track.media.end();
			drop(track);
			if let Err(err) = ended {
				tracing::warn!(%err, "failed to end a dropped MP4 track");
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use std::io::Cursor;

	use mp4_atom::Encode;

	use super::super::index::tests::{SIZE, mdat, moov, stbl};
	use super::*;

	fn import() -> (Import, crate::catalog::Producer, moq_net::broadcast::Consumer) {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
		(Import::new(broadcast, catalog.reserve()), catalog, consumer)
	}

	/// One published frame: its timestamp in microseconds, whether it opens a group, its bytes.
	type Published = (u128, bool, Bytes);

	/// Every frame the broadcast published, video renditions then audio, as a consumer reads them.
	async fn published(
		consumer: &moq_net::broadcast::Consumer,
		catalog: &crate::catalog::Producer,
	) -> Vec<Vec<Published>> {
		let snapshot = catalog.snapshot();
		let renditions = snapshot
			.video
			.renditions
			.iter()
			.map(|(name, config)| (name, crate::catalog::hang::Container::try_from(config).unwrap()))
			.chain(
				snapshot
					.audio
					.renditions
					.iter()
					.map(|(name, config)| (name, crate::catalog::hang::Container::try_from(config).unwrap())),
			);
		let mut out = Vec::new();
		for (name, container) in renditions {
			let replay = crate::container::test_util::within(std::time::Duration::from_secs(3600));
			let track = consumer.track(name).unwrap().subscribe(replay).await.unwrap();
			let mut reader = crate::container::Consumer::new(track, container);
			let mut frames = Vec::new();
			while let Some(frame) = kio::clock::timeout(std::time::Duration::from_secs(5), reader.read_frame())
				.await
				.expect("the import finished its tracks")
				.unwrap()
			{
				frames.push((frame.timestamp.as_micros().unwrap(), frame.keyframe, frame.payload));
			}
			out.push(frames);
		}
		out
	}

	/// What a [`Reader`](super::super::Reader) indexes in `data`, as [`published`] reads it back
	/// with the timeline starting at zero.
	fn indexed(data: &[u8]) -> Vec<Vec<Published>> {
		let mut reader = super::super::Reader::new(Cursor::new(data)).unwrap();
		let tracks = reader.tracks().to_vec();
		tracks
			.iter()
			.map(|track| {
				track
					.samples
					.iter()
					.map(|sample| {
						let micros = sample.timestamp(track.timescale).unwrap().as_micros().unwrap();
						(micros, sample.keyframe, reader.read(sample).unwrap())
					})
					.collect()
			})
			.collect()
	}

	/// Shift each track's timestamps so the earliest frame of any presents at zero.
	fn from_zero(mut tracks: Vec<Vec<Published>>) -> Vec<Vec<Published>> {
		let earliest = tracks.iter().flatten().map(|frame| frame.0).min().unwrap_or(0);
		for frame in tracks.iter_mut().flatten() {
			frame.0 -= earliest;
		}
		tracks
	}

	/// Import `data` in writes of `chunk` bytes.
	async fn streamed(data: &[u8], chunk: usize) -> Vec<Vec<Published>> {
		let (mut import, catalog, consumer) = import();
		for chunk in data.chunks(chunk) {
			import.decode(chunk).unwrap();
		}
		let _finished = import.finish().unwrap();
		from_zero(published(&consumer, &catalog).await)
	}

	/// The times at which `track` opens a group, and its frames without the flag.
	fn groups(track: &mut [Published]) -> Vec<u128> {
		let mut starts = Vec::new();
		for frame in track {
			if std::mem::take(&mut frame.1) {
				starts.push(frame.0);
			}
		}
		starts
	}

	/// A file streamed in writes of any size publishes every sample the reader indexes, with the
	/// same bytes and times, whether its `moov` comes first or last. Video groups open at its
	/// keyframes, and audio groups follow them.
	#[tokio::test]
	async fn a_streamed_file_publishes_what_the_reader_indexes() {
		for data in [
			&include_bytes!("test_data/faststart.mp4")[..],
			&include_bytes!("test_data/bframes.mp4")[..],
		] {
			let mut expected = from_zero(indexed(data));
			assert!(expected.iter().all(|track| !track.is_empty()));
			let keyframes = groups(&mut expected[0]);
			for track in &mut expected[1..] {
				groups(track);
			}
			for chunk in [1, 7, 100, data.len()] {
				let mut tracks = streamed(data, chunk).await;
				assert_eq!(groups(&mut tracks[0]), keyframes, "writes of {chunk} bytes");
				for audio in &mut tracks[1..] {
					// The first audio frame at or after each video keyframe opens a group.
					let starts = keyframes
						.iter()
						.filter_map(|&keyframe| audio.iter().map(|frame| frame.0).find(|&at| at >= keyframe))
						.collect::<Vec<_>>();
					assert_eq!(groups(audio), starts, "writes of {chunk} bytes");
				}
				assert_eq!(tracks, expected, "writes of {chunk} bytes");
			}
		}
	}

	/// With the `moov` last, nothing publishes until it arrives.
	#[tokio::test]
	async fn a_moov_last_file_holds_its_media_until_the_moov() {
		let data = include_bytes!("test_data/bframes.mp4");
		let moov = data.windows(4).position(|window| window == b"moov").unwrap() - 4;
		let (mut import, catalog, _consumer) = import();
		import.decode(&data[..moov]).unwrap();
		assert!(catalog.snapshot().video.renditions.is_empty());
		let State::Plain(plain) = &import.state else {
			panic!("a regular MP4");
		};
		assert_eq!(plain.held.iter().map(|held| held.data.len()).sum::<usize>(), 1309 - 8);

		import.decode(&data[moov..]).unwrap();
		assert_eq!(catalog.snapshot().video.renditions.len(), 1);
		let State::Plain(plain) = &import.state else {
			panic!("a regular MP4");
		};
		assert!(plain.held.is_empty(), "every sample published");
		let _finished = import.finish().unwrap();
	}

	/// With the `moov` first, the import holds no more than the sample still arriving.
	#[tokio::test]
	async fn a_faststart_file_holds_only_the_arriving_sample() {
		let data = include_bytes!("test_data/faststart.mp4");
		let largest = super::super::Reader::new(Cursor::new(&data[..]))
			.unwrap()
			.tracks()
			.iter()
			.flat_map(|track| &track.samples)
			.map(|sample| sample.size as usize)
			.max()
			.unwrap();
		let (mut import, _catalog, _consumer) = import();
		for chunk in data.chunks(64) {
			import.decode(chunk).unwrap();
			let State::Plain(plain) = &import.state else {
				panic!("a regular MP4");
			};
			let held: usize = plain.held.iter().map(|held| held.data.len()).sum();
			assert!(held < largest + 64, "{held} bytes held");
		}
		let _finished = import.finish().unwrap();
	}

	/// More media ahead of the `moov` than the import holds is refused.
	#[test]
	fn media_past_the_bound_before_the_moov_is_refused() {
		let (mut import, _catalog, _consumer) = import();
		// An `mdat` running to the end of the file.
		import.decode(&[0, 0, 0, 0, b'm', b'd', b'a', b't']).unwrap();
		let chunk = vec![0; 1024 * 1024];
		for _ in 0..fmp4::MAX_PENDING_BYTES / chunk.len() {
			import.decode(&chunk).unwrap();
		}
		assert!(matches!(
			import.decode(&[0]),
			Err(crate::Error::Cmaf(Error::MediaBeforeMoov))
		));
	}

	/// A fragmented file publishes what the fMP4 import does.
	#[tokio::test]
	async fn a_fragmented_file_is_imported_as_fmp4() {
		// The fixture ends on a moof without its mdat, so keep its first four whole fragments.
		let data = &include_bytes!("../fmp4/test_data/bbb.mp4")[..2035];
		let mp4 = streamed(data, 100).await;

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
		let mut fmp4 = fmp4::Import::new(broadcast, catalog.reserve());
		fmp4.decode(data).unwrap();
		let _finished = fmp4.finish().unwrap();
		let expected = from_zero(published(&consumer, &catalog).await);

		assert_eq!(mp4.len(), 2);
		assert_eq!(mp4, expected);
	}

	/// Input that ends before a sample it lists, or before the `moov`, fails the finish.
	#[test]
	fn a_truncated_file_fails_its_finish() {
		let data = include_bytes!("test_data/faststart.mp4");
		let (mut import, catalog, _consumer) = import();
		import.decode(&data[..data.len() - 100]).unwrap();
		assert_eq!(
			catalog.snapshot().video.renditions.len(),
			1,
			"the samples before the cut published"
		);
		assert!(matches!(
			import.finish(),
			Err(crate::Error::Cmaf(Error::SampleOutsideMedia { .. }))
		));

		let (import, ..) = self::import();
		assert!(matches!(import.finish(), Err(crate::Error::Cmaf(Error::NoMoov))));
	}

	/// The most samples a small `moov` may declare, each a byte of audio lasting two seconds,
	/// stay within the fuzz targets' allocation bound, which a group per sample would not.
	#[test]
	fn tiny_samples_stay_within_the_allocation_bound() {
		let mut tables = stbl(0);
		let count = 4096;
		tables.stts.entries[0] = mp4_atom::SttsEntry {
			sample_count: count,
			sample_delta: 2000,
		};
		tables.stsc.entries[0].samples_per_chunk = count;
		tables.stsz.samples = mp4_atom::StszSamples::Identical { count, size: 1 };
		let mut file = Vec::new();
		let moov = moov(tables, Vec::new());
		moov.encode(&mut file).unwrap();
		let start = file.len() as u32 + 8;
		let mut moov = moov;
		moov.trak[0].mdia.minf.stbl.stco = Some(mp4_atom::Stco { entries: vec![start] });
		file.clear();
		moov.encode(&mut file).unwrap();
		file.extend(super::super::index::tests::mp4_box(b"mdat", &vec![1; count as usize]));
		let input = [&[0][..], &file].concat();
		assert!(crate::fuzz::run(crate::fuzz::mp4, &input), "the file imports");
	}

	/// A `moov` header declaring the most a file reader holds allocates only what arrives of it.
	#[test]
	fn a_declared_moov_allocates_only_what_arrived() {
		let mut input = vec![0, 0, 0, 0, 1];
		input.extend_from_slice(b"moov");
		input.extend_from_slice(&fmp4::MAX_MOOV_BYTES.to_be_bytes());
		input.extend_from_slice(&[0; 64]);
		assert!(!crate::fuzz::run(crate::fuzz::mp4, &input), "the moov never ends");
	}

	/// An audio group in a file without video spans a second.
	#[tokio::test]
	async fn an_audio_only_file_groups_by_the_second() {
		let mut data = mdat(25);
		moov(stbl(25), Vec::new()).encode(&mut data).unwrap();
		let mut tracks = streamed(&data, data.len()).await;
		assert_eq!(groups(&mut tracks[0]), [0, 1_000_000, 2_000_000]);
	}

	/// A sample stored before the one published ahead of it is refused, since the bytes went by.
	#[test]
	fn samples_out_of_file_order_are_refused() {
		let mut tables = stbl(2);
		tables.stsc.entries[0].samples_per_chunk = 1;
		tables.stco = Some(mp4_atom::Stco {
			entries: vec![8 + SIZE, 8],
		});
		let mut data = mdat(2);
		moov(tables, Vec::new()).encode(&mut data).unwrap();
		let (mut import, _catalog, _consumer) = import();
		assert!(matches!(
			import.decode(&data),
			Err(crate::Error::Cmaf(Error::BackwardsDataOffset))
		));
	}
}
