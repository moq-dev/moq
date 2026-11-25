//! The sample index of an MP4 file: where each sample lies, and when it decodes and presents.

use std::io::{Read, Seek, SeekFrom};

use bytes::Bytes;
use hang::catalog::{AudioConfig, Container, VideoConfig};
use mp4_atom::{Atom, Decode, DecodeMaybe, Encode};

use crate::Result;
use crate::container::fmp4::{self, Error, config, edit::Edit, metadata, sample};

/// The sample index of an MP4 file, plain or fragmented, read through a seekable reader.
///
/// [`new`](Self::new) reads the boxes that describe the media and skips the media itself, so a
/// `moov` after the `mdat` costs a seek, not a read of the file. Each [`Track`] lists its samples
/// with where they lie and when they decode and present, so a caller can seek, loop, and pace
/// the file, fetching one sample at a time with [`read`](Self::read).
///
/// Every sample is checked to lie within the file, and the tracks may declare at most one sample
/// per byte of it, so no size the file declares allocates more than the file could fill. The
/// `moov` may hold up to 128 MiB, about a day of 30 fps video with 48 kHz AAC.
///
/// Audio and video tracks in a codec [`Import`](crate::container::fmp4::Import) reads are listed;
/// other tracks (timecode, chapters, subtitles, codecs moq-mux doesn't carry) are skipped. A track
/// with several sample descriptions, as a file joined from several encodes has, is refused.
pub struct Reader<R> {
	reader: R,
	/// The file's length in bytes.
	len: u64,
	tracks: Vec<Track>,
}

/// One audio or video track of an MP4 file.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Track {
	/// The track's `track_id`.
	pub id: u32,
	/// The track's catalog entry, as the fMP4 import publishes it, with a
	/// [`Legacy`](Container::Legacy) container.
	pub config: Config,
	/// The ticks a second the samples' times count.
	pub timescale: moq_net::Timescale,
	/// The samples the track's edit list presents, in decode order.
	pub samples: Vec<Sample>,
}

/// A track's catalog entry.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
#[expect(
	clippy::large_enum_variant,
	reason = "one per track of an index that holds a sample list per track, which dwarfs it"
)]
pub enum Config {
	/// A video track.
	Video(VideoConfig),
	/// An audio track.
	Audio(AudioConfig),
}

/// One sample: where it lies in the file, and when it decodes and presents.
///
/// Times count the track's [`timescale`](Track::timescale), after its edit list: the edit's
/// start presents at zero, or later by the edit's leading empty time. The first sample is a
/// keyframe at or after the edit's start, and the samples stop before the first one, in decode
/// order, presenting at or after the edit's end. A sample of a leading picture that presents
/// before the edit's start is left out. An Opus track keeps the samples its OpusHead pre-skip
/// covers, which a decoder given the catalog entry's description trims, as the fMP4 import
/// publishes them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Sample {
	/// Where the sample starts in the file.
	pub offset: u64,
	/// The sample's size in bytes.
	pub size: u32,
	/// When the sample presents.
	pub pts: u64,
	/// When the sample decodes, which for a stream with B-frames is earlier than any
	/// presentation, so it can be negative.
	pub dts: i64,
	/// How long the sample lasts, or zero when the file doesn't say.
	pub duration: u32,
	/// Whether a decoder can start at this sample: a sync sample, which every audio sample is.
	pub keyframe: bool,
}

impl Sample {
	/// The presentation time as a timestamp counting `timescale`, the track's.
	pub fn timestamp(
		&self,
		timescale: moq_net::Timescale,
	) -> std::result::Result<moq_net::Timestamp, moq_net::TimeOverflow> {
		moq_net::Timestamp::new(self.pts, timescale)
	}
}

impl<R: Read + Seek> Reader<R> {
	/// Read the sample index of the MP4 file `reader` holds.
	///
	/// Fails for a file without a `moov`, one whose boxes or samples run past its end, or one
	/// whose metadata doesn't parse. A few bytes after the last box, too few for a box header, are
	/// skipped; an `mdat` that runs past the end ends the boxes, and its samples past the end fail.
	pub fn new(mut reader: R) -> Result<Self> {
		let len = reader.seek(SeekFrom::End(0))?;
		let mut moov = None;
		let mut fragments = Vec::new();
		let mut position = 0;
		while let Some(atom) = TopLevel::read(&mut reader, position, len)? {
			match atom.kind {
				mp4_atom::Moov::KIND => {
					if moov.is_some() {
						return Err(Error::DuplicateMoov.into());
					}
					let size = atom.end - atom.start;
					if size > fmp4::MAX_MOOV_BYTES {
						return Err(Error::MoovTooLarge(size).into());
					}
					moov = Some(decode_moov(&atom.bytes(&mut reader)?)?);
				}
				// Decoded once the moov says which tracks they belong to, so only one is held.
				mp4_atom::Moof::KIND => fragments.push(atom.start..atom.end),
				_ => {}
			}
			position = atom.end;
		}
		let moov = moov.ok_or(Error::NoMoov)?;

		let mut budget = Budget(len);
		let mut declared = declare(&moov, &mut budget, Some(len))?;
		for fragment in fragments {
			let size = fragment.end - fragment.start;
			if size > fmp4::MAX_METADATA_BYTES as u64 {
				return Err(Error::MetadataTooLarge {
					kind: mp4_atom::Moof::KIND.to_string(),
					size: usize::try_from(size).unwrap_or(usize::MAX),
				}
				.into());
			}
			let atom = TopLevel {
				kind: mp4_atom::Moof::KIND,
				start: fragment.start,
				end: fragment.end,
			};
			let moof = decode_moof(&atom.bytes(&mut reader)?)?;
			// Without a base offset, a track fragment's data follows the previous one's.
			let mut data_end = fragment.start;
			for traf in &moof.traf {
				let Some(track) = declared.iter_mut().find(|track| track.id == traf.tfhd.track_id) else {
					continue;
				};
				let base = match traf.tfhd.base_data_offset {
					Some(base) => base,
					None if traf.tfhd.default_base_is_moof => fragment.start,
					None => data_end,
				};
				data_end = track.fragment(traf, base, len, &mut budget)?;
			}
		}

		let tracks = declared.into_iter().map(Declared::present).collect::<Result<_>>()?;
		Ok(Self { reader, len, tracks })
	}

	/// Read the payload of `sample`, one of this file's.
	pub fn read(&mut self, sample: &Sample) -> Result<Bytes> {
		let end = sample.offset.checked_add(u64::from(sample.size));
		if end.is_none_or(|end| end > self.len) {
			return Err(Error::SampleOutsideMedia {
				offset: sample.offset,
				size: sample.size,
			}
			.into());
		}
		let mut data = vec![0; sample.size as usize];
		self.reader.seek(SeekFrom::Start(sample.offset))?;
		self.reader.read_exact(&mut data)?;
		Ok(data.into())
	}
}

impl<R> Reader<R> {
	/// The file's audio and video tracks, in the order its `moov` lists them.
	pub fn tracks(&self) -> &[Track] {
		&self.tracks
	}

	/// Take back the reader.
	pub fn into_inner(self) -> R {
		self.reader
	}
}

/// One top-level box in a file.
#[derive(Debug)]
struct TopLevel {
	kind: mp4_atom::FourCC,
	/// Where the box starts.
	start: u64,
	/// Where the box ends.
	end: u64,
}

impl TopLevel {
	/// The box at `start` of a file of `len` bytes, or `None` after the last box.
	///
	/// The boxes end at the end of the file, at fewer bytes than a header after the last box,
	/// which some files carry as a stray line break, or at a box other than a `moov` or `moof` that
	/// runs past the end: an `mdat` cut short, whose samples past the end fail on their own.
	fn read<R: Read + Seek>(reader: &mut R, start: u64, len: u64) -> Result<Option<Self>> {
		let available = len.saturating_sub(start);
		if available < 8 {
			if available > 0 {
				tracing::debug!(bytes = available, "skipping the bytes after the last box");
			}
			return Ok(None);
		}
		let mut header = [0u8; 16];
		reader.seek(SeekFrom::Start(start))?;
		reader.read_exact(&mut header[..8])?;
		let kind = mp4_atom::FourCC::new(&[header[4], header[5], header[6], header[7]]);
		let end = match u32::from_be_bytes(header[..4].try_into().expect("four bytes")) {
			0 => len,
			1 if available < 16 => return Err(Error::BoxPastEnd(kind.to_string()).into()),
			1 => {
				reader.read_exact(&mut header[8..])?;
				match u64::from_be_bytes(header[8..].try_into().expect("eight bytes")) {
					size if size < 16 => return Err(Error::mp4(mp4_atom::Error::InvalidSize).into()),
					size => start.checked_add(size).ok_or(Error::BoxPastEnd(kind.to_string()))?,
				}
			}
			size if size < 8 => return Err(Error::mp4(mp4_atom::Error::InvalidSize).into()),
			size => start + u64::from(size),
		};
		if end > len {
			if matches!(kind, mp4_atom::Moov::KIND | mp4_atom::Moof::KIND) {
				return Err(Error::BoxPastEnd(kind.to_string()).into());
			}
			tracing::debug!(%kind, start, end, len, "a box runs past the end of the file, ending the boxes");
			return Ok(None);
		}
		Ok(Some(Self { kind, start, end }))
	}

	/// The whole box, header included, whose size its caller bounded.
	fn bytes<R: Read + Seek>(&self, reader: &mut R) -> Result<Vec<u8>> {
		let mut data = vec![0; (self.end - self.start) as usize];
		reader.seek(SeekFrom::Start(self.start))?;
		reader.read_exact(&mut data)?;
		Ok(data)
	}
}

/// Decode a `moov` box, header included, whose sample tables may be larger than the metadata a
/// stream sends.
pub(super) fn decode_moov(data: &[u8]) -> Result<mp4_atom::Moov> {
	let data = without_metadata(data)?;
	metadata::check_unbounded(&data)?;
	mp4_atom::Moov::decode(&mut data.as_slice()).map_err(|err| Error::mp4(err).into())
}

/// Decode a `moof` box, header included.
fn decode_moof(data: &[u8]) -> Result<mp4_atom::Moof> {
	let data = without_metadata(data)?;
	metadata::check(&data)?;
	mp4_atom::Moof::decode(&mut data.as_slice()).map_err(|err| Error::mp4(err).into())
}

/// The box in `data`, header included, without the `udta` and `meta` boxes among its children
/// and those of each `trak` and `traf` in it.
///
/// No sample's place or time depends on them, and mp4-atom 0.17.1 refuses a repeated `ilst`
/// item, a second `chpl` or `meta` in a `udta`, and a second `udta` or `meta` in a `moov`,
/// `trak`, or `traf`, which players accept and some files carry.
fn without_metadata(data: &[u8]) -> Result<Vec<u8>> {
	let mut input = data;
	let header = mp4_atom::Header::decode(&mut input).map_err(Error::mp4)?;
	let body = children(input)?;
	let mut out = Vec::with_capacity(body.len() + 16);
	mp4_atom::Header {
		kind: header.kind,
		size: Some(body.len()),
	}
	.encode(&mut out)
	.map_err(Error::mp4)?;
	out.extend_from_slice(&body);
	Ok(out)
}

/// The child boxes in `body` without their `udta` and `meta` boxes, descending into `trak` and
/// `traf`.
fn children(mut body: &[u8]) -> Result<Vec<u8>> {
	let mut kept = Vec::with_capacity(body.len());
	while let Some(header) = mp4_atom::Header::decode_maybe(&mut body).map_err(Error::mp4)? {
		let size = header.size.unwrap_or(body.len());
		let (child, rest) = body
			.split_at_checked(size)
			.ok_or(Error::mp4(mp4_atom::Error::OutOfBounds))?;
		body = rest;
		let filtered;
		let child = match header.kind {
			mp4_atom::Udta::KIND | mp4_atom::Meta::KIND => continue,
			mp4_atom::Trak::KIND | mp4_atom::Traf::KIND => {
				filtered = children(child)?;
				filtered.as_slice()
			}
			_ => child,
		};
		mp4_atom::Header {
			kind: header.kind,
			size: Some(child.len()),
		}
		.encode(&mut kept)
		.map_err(Error::mp4)?;
		kept.extend_from_slice(child);
	}
	// Keep what is too short for a box, such as the padding mp4-atom tolerates.
	kept.extend_from_slice(body);
	Ok(kept)
}

/// The samples a file may still declare, one per byte of the file, so a table of billions of
/// empty samples is refused before it is walked.
pub(super) struct Budget(pub u64);

impl Budget {
	fn take(&mut self, count: u64) -> Result<()> {
		self.0 = self.0.checked_sub(count).ok_or(Error::TooManyFileSamples(count))?;
		Ok(())
	}
}

/// A sample as its tables list it, before the edit list.
#[derive(Clone, Copy, Debug)]
pub(super) struct Raw {
	pub offset: u64,
	pub size: u32,
	pub dts: i64,
	pub pts: i64,
	pub duration: u32,
	pub keyframe: bool,
}

/// One audio or video track a `moov` declares, with the samples its tables list.
pub(super) struct Declared {
	pub id: u32,
	pub config: Config,
	pub timescale: moq_net::Timescale,
	pub edit: Edit,
	defaults: sample::Defaults,
	pub samples: Vec<Raw>,
}

/// The audio and video tracks `moov` declares, in a file of `len` bytes when it is known.
///
/// A track that is neither, or in a codec moq-mux doesn't carry, is skipped.
pub(super) fn declare(moov: &mp4_atom::Moov, budget: &mut Budget, len: Option<u64>) -> Result<Vec<Declared>> {
	let mut tracks = Vec::new();
	for trak in &moov.trak {
		let id = trak.tkhd.track_id;
		let handler = trak.mdia.hdlr.handler;
		let video = match handler.as_ref() {
			b"vide" => true,
			b"soun" => false,
			_ => {
				tracing::debug!(track = id, %handler, "skipping a track that is neither audio nor video");
				continue;
			}
		};
		let entry = config::sample_entry(trak)?;
		if !config::supported(entry) {
			tracing::warn!(track = id, codec = %Error::unsupported_codec(entry), "skipping a track in an unsupported codec");
			continue;
		}
		let config = match video {
			true => Config::Video(config::video(entry, Container::Legacy)?),
			false => Config::Audio(config::audio(entry, Container::Legacy)?),
		};
		tracks.push(Declared {
			id,
			config,
			timescale: moq_net::Timescale::new(u64::from(trak.mdia.mdhd.timescale))?,
			edit: Edit::lenient(trak, moov.mvhd.timescale)?,
			defaults: sample::Defaults::from_moov(moov, id),
			samples: sample_table(&trak.mdia.minf.stbl, budget, len)?,
		});
	}
	Ok(tracks)
}

/// Where a sample of `size` bytes at `offset` ends, checked against the file's `len` when known.
fn sample_end(offset: u64, size: u32, len: Option<u64>) -> Result<u64> {
	offset
		.checked_add(u64::from(size))
		.filter(|&end| len.is_none_or(|len| end <= len))
		.ok_or_else(|| Error::SampleOutsideMedia { offset, size }.into())
}

/// The samples a regular MP4 track's sample tables list.
fn sample_table(stbl: &mp4_atom::Stbl, budget: &mut Budget, len: Option<u64>) -> Result<Vec<Raw>> {
	let sizes = &stbl.stsz.samples;
	let count = match sizes {
		mp4_atom::StszSamples::Identical { count, .. } => u64::from(*count),
		mp4_atom::StszSamples::Different { sizes } => sizes.len() as u64,
	};
	budget.take(count)?;
	if count == 0 {
		return Ok(Vec::new());
	}
	let size = |index: usize| match sizes {
		mp4_atom::StszSamples::Identical { size, .. } => *size,
		mp4_atom::StszSamples::Different { sizes } => sizes[index],
	};
	let stco = stbl
		.stco
		.as_ref()
		.map(|stco| stco.entries.iter().map(|&offset| u64::from(offset)));
	let co64 = stbl.co64.as_ref().map(|co64| co64.entries.iter().copied());
	let chunks: Box<dyn Iterator<Item = u64>> = match (stco, co64) {
		(Some(stco), _) => Box::new(stco),
		(None, Some(co64)) => Box::new(co64),
		(None, None) => return Err(Error::InvalidSampleTable("samples without chunk offsets").into()),
	};

	let mut durations = stbl
		.stts
		.entries
		.iter()
		.flat_map(|entry| std::iter::repeat_n(entry.sample_delta, entry.sample_count as usize));
	// A version 0 `ctts` holds unsigned offsets and a version 1 one signed, which mp4-atom reads
	// alike.
	let mut offsets = stbl
		.ctts
		.iter()
		.flat_map(|ctts| &ctts.entries)
		.flat_map(|entry| std::iter::repeat_n(entry.sample_offset, entry.sample_count as usize));
	let sync = stbl.stss.as_ref().map(|stss| &stss.entries);
	let entries = &stbl.stsc.entries;
	let mut entry = 0;
	let count = count as usize;
	let mut samples = Vec::with_capacity(count);
	let mut dts = 0i64;
	'chunks: for (chunk, mut offset) in chunks.enumerate() {
		// Each sample-to-chunk entry runs until the next one's first chunk, counting from one.
		let number = chunk as u64 + 1;
		while entries
			.get(entry + 1)
			.is_some_and(|next| u64::from(next.first_chunk) <= number)
		{
			entry += 1;
		}
		let per_chunk = entries.get(entry).map_or(0, |entry| entry.samples_per_chunk);
		for _ in 0..per_chunk {
			let index = samples.len();
			if index == count {
				break 'chunks;
			}
			let size = size(index);
			let duration = durations.next().unwrap_or(0);
			let pts = dts.checked_add(offsets.next().unwrap_or(0)).ok_or(Error::PtsOverflow)?;
			samples.push(Raw {
				offset,
				size,
				dts,
				pts,
				duration,
				// Without an `stss`, every sample is a sync sample.
				keyframe: sync.is_none_or(|sync| sync.binary_search(&(index as u32 + 1)).is_ok()),
			});
			offset = sample_end(offset, size, len)?;
			dts = dts.checked_add(i64::from(duration)).ok_or(Error::PtsOverflow)?;
		}
	}
	if samples.len() < count {
		return Err(Error::InvalidSampleTable("the chunks hold fewer samples than the sizes list").into());
	}
	Ok(samples)
}

impl Declared {
	/// Append the samples of one track fragment whose data offsets count from `base`, and return
	/// where its data ends. A fragment without a `tfdt` continues from the track's last sample.
	fn fragment(&mut self, traf: &mp4_atom::Traf, base: u64, len: u64, budget: &mut Budget) -> Result<u64> {
		budget.take(sample::count(traf))?;
		let mut dts = match &traf.tfdt {
			Some(tfdt) => i64::try_from(tfdt.base_media_decode_time).map_err(|_| Error::PtsOverflow)?,
			None => match self.samples.last() {
				Some(last) => last
					.dts
					.checked_add(i64::from(last.duration))
					.ok_or(Error::PtsOverflow)?,
				None => 0,
			},
		};
		let mut offset = base;
		for run in &traf.trun {
			if let Some(data_offset) = run.data_offset {
				offset = base
					.checked_add_signed(i64::from(data_offset))
					.ok_or(Error::InvalidDataOffset)?;
			}
			for entry in run.samples.iter() {
				let sample = self.defaults.resolve(&traf.tfhd, &entry);
				let duration = sample.duration.unwrap_or(0);
				let pts = dts.checked_add(entry.cts.unwrap_or(0)).ok_or(Error::PtsOverflow)?;
				self.samples.push(Raw {
					offset,
					size: sample.size,
					dts,
					pts,
					duration,
					keyframe: sample.is_sync(),
				});
				offset = sample_end(offset, sample.size, Some(len))?;
				dts = dts.checked_add(i64::from(duration)).ok_or(Error::PtsOverflow)?;
			}
		}
		Ok(offset)
	}

	/// The track with its edit list applied to its samples.
	fn present(self) -> Result<Track> {
		let mut presenter = Presenter::new(self.edit);
		let mut samples = Vec::with_capacity(self.samples.len());
		for raw in self.samples {
			match presenter.present(raw)? {
				Presented::Sample(sample) => samples.push(sample),
				Presented::Skipped => {}
				Presented::Ended => break,
			}
		}
		Ok(Track {
			id: self.id,
			config: self.config,
			timescale: self.timescale,
			samples,
		})
	}
}

/// Applies a track's edit list to its samples in decode order.
pub(super) struct Presenter {
	edit: Edit,
	/// Whether a keyframe at or after the edit's start has opened the presentation.
	started: bool,
}

/// What [`Presenter::present`] made of a sample.
pub(super) enum Presented {
	/// The sample presents.
	Sample(Sample),
	/// The sample precedes the edit's start, or the first keyframe at or after it.
	Skipped,
	/// The sample presents at or after the edit's end, which ends the track.
	Ended,
}

impl Presenter {
	pub fn new(edit: Edit) -> Self {
		Self { edit, started: false }
	}

	/// Place `raw`, the next sample in decode order, on the presentation timeline.
	pub fn present(&mut self, raw: Raw) -> Result<Presented> {
		if let Some(end) = self.edit.end()
			&& i128::from(raw.pts) >= i128::from(end)
		{
			return Ok(Presented::Ended);
		}
		// A version 1 `ctts` can present a sample before zero, which precedes any edit.
		let presented = match u64::try_from(raw.pts) {
			Ok(pts) => self.edit.present(pts)?,
			Err(_) => None,
		};
		let Some(pts) = presented else {
			return Ok(Presented::Skipped);
		};
		// A decoder starts at a keyframe, so a sample before the first one has nothing to decode
		// against.
		self.started |= raw.keyframe;
		if !self.started {
			return Ok(Presented::Skipped);
		}
		Ok(Presented::Sample(Sample {
			offset: raw.offset,
			size: raw.size,
			pts,
			dts: self.edit.decode(raw.dts)?,
			duration: raw.duration,
			keyframe: raw.keyframe,
		}))
	}
}

#[cfg(test)]
pub(super) mod tests {
	//! Index tests on files built in memory, each a `moov` and an `mdat` or `moof` boxes, and on
	//! the encodes in `test_data`.
	//!
	//! - `faststart.mp4`: 64x48 H.264 Baseline at 10 fps with a keyframe every 5 frames, and 16 kHz
	//!   mono AAC, one second each, `moov` first: `ffmpeg -f lavfi -i
	//!   testsrc2=size=64x48:rate=10:duration=1 -f lavfi -i sine=frequency=440:sample_rate=16000:duration=1
	//!   -c:v libx264 -profile:v baseline -crf 40 -g 5 -pix_fmt yuv420p -c:a aac -b:a 16k -ac 1
	//!   -fflags +bitexact -flags:v +bitexact -flags:a +bitexact -movflags +faststart faststart.mp4`.
	//!   The audio's edit list trims the encoder's priming packet.
	//! - `bframes.mp4`: the same video in High profile with two B-frames (`-profile:v high -bf 2
	//!   -g 10`), no audio, and the `moov` last. Its edit list starts the presentation two frames
	//!   into the decode timeline.

	use std::io::Cursor;

	use mp4_atom::{Decode, Encode};

	use super::*;

	/// The size of every test sample.
	pub(crate) const SIZE: u32 = 10;

	/// A box of `kind` around `body`.
	pub(crate) fn mp4_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
		let mut data = (8 + body.len() as u32).to_be_bytes().to_vec();
		data.extend_from_slice(kind);
		data.extend_from_slice(body);
		data
	}

	/// An `mdat` of `samples` test samples.
	pub(crate) fn mdat(samples: u32) -> Vec<u8> {
		mp4_box(b"mdat", &vec![0; (samples * SIZE) as usize])
	}

	/// Sample tables of `count` samples of 100 ticks in one chunk, which an `mdat` at the start of
	/// the file holds.
	pub(crate) fn stbl(count: u32) -> mp4_atom::Stbl {
		mp4_atom::Stbl {
			stsd: mp4_atom::Stsd {
				codecs: vec![
					mp4_atom::Opus {
						audio: mp4_atom::Audio {
							data_reference_index: 1,
							channel_count: 2,
							sample_size: 16,
							sample_rate: mp4_atom::FixedPoint::from(48_000),
						},
						dops: mp4_atom::Dops {
							output_channel_count: 2,
							pre_skip: 0,
							input_sample_rate: 48_000,
							output_gain: 0,
							channel_mapping: None,
						},
						btrt: None,
					}
					.into(),
				],
			},
			stts: mp4_atom::Stts {
				entries: vec![mp4_atom::SttsEntry {
					sample_count: count,
					sample_delta: 100,
				}],
			},
			stsc: mp4_atom::Stsc {
				entries: vec![mp4_atom::StscEntry {
					first_chunk: 1,
					samples_per_chunk: count,
					sample_description_index: 1,
				}],
			},
			stsz: mp4_atom::Stsz {
				samples: mp4_atom::StszSamples::Identical { count, size: SIZE },
			},
			stco: Some(mp4_atom::Stco { entries: vec![8] }),
			..Default::default()
		}
	}

	/// A `moov` with one Opus track counting 1000 ticks a second, its tables `stbl`, and the edit
	/// list `edits`.
	pub(crate) fn moov(stbl: mp4_atom::Stbl, edits: Vec<mp4_atom::ElstEntry>) -> mp4_atom::Moov {
		let trak = mp4_atom::Trak {
			tkhd: mp4_atom::Tkhd {
				track_id: 1,
				..Default::default()
			},
			edts: (!edits.is_empty()).then_some(mp4_atom::Edts {
				elst: Some(mp4_atom::Elst { entries: edits }),
			}),
			mdia: mp4_atom::Mdia {
				mdhd: mp4_atom::Mdhd {
					timescale: 1000,
					language: "und".into(),
					..Default::default()
				},
				hdlr: mp4_atom::Hdlr {
					handler: b"soun".into(),
					name: String::new(),
				},
				minf: mp4_atom::Minf {
					stbl,
					..Default::default()
				},
			},
			..Default::default()
		};
		mp4_atom::Moov {
			mvhd: mp4_atom::Mvhd {
				timescale: 1000,
				..Default::default()
			},
			trak: vec![trak],
			..Default::default()
		}
	}

	fn index(data: Vec<u8>) -> Result<Vec<Track>> {
		Ok(Reader::new(Cursor::new(data))?.tracks().to_vec())
	}

	/// Index an `mdat` of `samples` test samples followed by `moov`.
	fn index_plain(samples: u32, moov: &mp4_atom::Moov) -> Result<Vec<Sample>> {
		let mut data = mdat(samples);
		moov.encode(&mut data).unwrap();
		Ok(index(data)?.remove(0).samples)
	}

	/// An edit list entry in the movie timescale of 1000.
	pub(crate) fn edit(segment_duration: u64, media_time: Option<u64>) -> mp4_atom::ElstEntry {
		mp4_atom::ElstEntry {
			segment_duration,
			media_time,
			media_rate: mp4_atom::FixedPoint::from(1),
		}
	}

	fn pts(samples: &[Sample]) -> Vec<u64> {
		samples.iter().map(|sample| sample.pts).collect()
	}

	/// An empty edit delays the track, and the media edit trims its start and its end.
	#[test]
	fn edits_delay_and_trim() {
		let delayed = moov(stbl(10), vec![edit(500, None), edit(0, Some(200))]);
		let samples = index_plain(10, &delayed).unwrap();
		assert_eq!(pts(&samples), [500, 600, 700, 800, 900, 1000, 1100, 1200]);
		assert_eq!(samples[0].offset, 8 + 2 * u64::from(SIZE));

		let trimmed = moov(stbl(10), vec![edit(300, Some(200))]);
		assert_eq!(pts(&index_plain(10, &trimmed).unwrap()), [0, 100, 200]);

		// A splice plays the media whole from the first edit's start.
		let spliced = moov(stbl(10), vec![edit(300, Some(200)), edit(300, Some(700))]);
		assert_eq!(index_plain(10, &spliced).unwrap().len(), 8);
	}

	/// The presentation starts at the first keyframe at or after the edit's start, so a decoder
	/// needs no sample before it.
	#[test]
	fn the_presentation_starts_at_a_keyframe() {
		let mut tables = stbl(10);
		tables.stss = Some(mp4_atom::Stss { entries: vec![1, 6] });
		let samples = index_plain(10, &moov(tables, vec![edit(0, Some(200))])).unwrap();
		assert_eq!(pts(&samples), [300, 400, 500, 600, 700]);
		assert!(samples[0].keyframe);
		assert_eq!(samples[0].dts, 300);
	}

	/// A version 1 `ctts` presents samples before they decode, and one before zero precedes the
	/// presentation.
	#[test]
	fn negative_composition_offsets() {
		let mut tables = stbl(4);
		tables.ctts = Some(mp4_atom::Ctts {
			entries: [200, 200, -100, -100]
				.map(|sample_offset| mp4_atom::CttsEntry {
					sample_count: 1,
					sample_offset,
				})
				.into(),
		});
		let samples = index_plain(4, &moov(tables.clone(), Vec::new())).unwrap();
		assert_eq!(pts(&samples), [200, 300, 100, 200]);

		// An edit starting at the first presentation shifts every decode time below it.
		let samples = index_plain(4, &moov(tables, vec![edit(0, Some(100))])).unwrap();
		assert_eq!(pts(&samples), [100, 200, 0, 100]);
		let dts: Vec<i64> = samples.iter().map(|sample| sample.dts).collect();
		assert_eq!(dts, [-100, 0, 100, 200]);

		let mut before = stbl(2);
		before.ctts = Some(mp4_atom::Ctts {
			entries: vec![mp4_atom::CttsEntry {
				sample_count: 2,
				sample_offset: -100,
			}],
		});
		assert_eq!(pts(&index_plain(2, &moov(before, Vec::new())).unwrap()), [0]);
	}

	/// Chunk offsets in 64 bits place the samples as 32-bit ones do, and each chunk's samples
	/// follow one another.
	#[test]
	fn co64_places_the_samples() {
		let mut tables = stbl(3);
		tables.stco = None;
		tables.co64 = Some(mp4_atom::Co64 { entries: vec![8] });
		let samples = index_plain(3, &moov(tables, Vec::new())).unwrap();
		let offsets: Vec<u64> = samples.iter().map(|sample| sample.offset).collect();
		assert_eq!(offsets, [8, 18, 28]);
	}

	/// Without an `stss` every sample is a keyframe, and with one only those it lists.
	#[test]
	fn sync_samples() {
		let all = index_plain(3, &moov(stbl(3), Vec::new())).unwrap();
		assert!(all.iter().all(|sample| sample.keyframe));
		let mut tables = stbl(3);
		tables.stss = Some(mp4_atom::Stss { entries: vec![1, 3] });
		let listed = index_plain(3, &moov(tables, Vec::new())).unwrap();
		let keyframes: Vec<bool> = listed.iter().map(|sample| sample.keyframe).collect();
		assert_eq!(keyframes, [true, false, true]);
	}

	/// Counts and offsets past what the file holds fail the index instead of allocating.
	#[test]
	fn sizes_past_the_file_fail() {
		let mut huge = stbl(3);
		huge.stsz.samples = mp4_atom::StszSamples::Identical {
			count: u32::MAX,
			size: SIZE,
		};
		assert!(matches!(
			index_plain(3, &moov(huge, Vec::new())),
			Err(crate::Error::Cmaf(Error::TooManyFileSamples(_)))
		));
		let mut past = stbl(3);
		past.stco = Some(mp4_atom::Stco { entries: vec![1 << 30] });
		assert!(matches!(
			index_plain(3, &moov(past, Vec::new())),
			Err(crate::Error::Cmaf(Error::SampleOutsideMedia { .. }))
		));
		let mut short = stbl(3);
		short.stsc.entries[0].samples_per_chunk = 2;
		assert!(matches!(
			index_plain(3, &moov(short, Vec::new())),
			Err(crate::Error::Cmaf(Error::InvalidSampleTable(_)))
		));
	}

	/// A track with two sample descriptions is refused, since the catalog can't follow a switch.
	#[test]
	fn several_sample_descriptions_fail() {
		let mut tables = stbl(1);
		tables.stsd.codecs.push(tables.stsd.codecs[0].clone());
		assert!(matches!(
			index_plain(1, &moov(tables, Vec::new())),
			Err(crate::Error::Cmaf(Error::MultipleCodecs))
		));
	}

	/// A track that is not audio or video, or in a codec moq-mux doesn't carry, is skipped.
	#[test]
	fn other_tracks_are_skipped() {
		let mut file = moov(stbl(1), Vec::new());
		let mut text = file.trak[0].clone();
		text.mdia.hdlr.handler = b"text".into();
		let mut ac3 = file.trak[0].clone();
		ac3.mdia.minf.stbl.stsd.codecs = vec![
			mp4_atom::Ac3 {
				audio: file.trak[0].mdia.minf.stbl.stsd.codecs[0].audio().unwrap().clone(),
				dac3: mp4_atom::Ac3SpecificBox {
					fscod: 0,
					bsid: 8,
					bsmod: 0,
					acmod: 2,
					lfeon: false,
					bit_rate_code: 0,
				},
			}
			.into(),
		];
		file.trak.extend([text, ac3]);
		let mut data = mdat(1);
		file.encode(&mut data).unwrap();
		let tracks = index(data).unwrap();
		assert_eq!(tracks.len(), 1);
		assert!(matches!(tracks[0].config, Config::Audio(_)));
	}

	/// A `moof` with its `trun` sizes and durations left to the `trex` defaults.
	fn moof(count: u32, tfdt: Option<u64>) -> mp4_atom::Moof {
		mp4_atom::Moof {
			traf: vec![mp4_atom::Traf {
				tfhd: mp4_atom::Tfhd {
					track_id: 1,
					default_base_is_moof: true,
					..Default::default()
				},
				tfdt: tfdt.map(|base_media_decode_time| mp4_atom::Tfdt { base_media_decode_time }),
				trun: vec![mp4_atom::Trun {
					data_offset: Some(0),
					samples: mp4_atom::TrunSamples::Inherited {
						count,
						first_flags: None,
					},
				}],
				..Default::default()
			}],
			..Default::default()
		}
	}

	/// A fragment without a `tfdt` continues the track where the last one ended, and the samples
	/// take the `trex` defaults.
	#[test]
	fn fragments_without_tfdt_continue() {
		let mut init = moov(stbl(0), Vec::new());
		init.mvex = Some(mp4_atom::Mvex {
			trex: vec![mp4_atom::Trex {
				track_id: 1,
				default_sample_description_index: 1,
				default_sample_duration: 100,
				default_sample_size: SIZE,
				default_sample_flags: 0,
			}],
			..Default::default()
		});
		let mut data = Vec::new();
		init.encode(&mut data).unwrap();
		for (count, tfdt) in [(3, Some(0)), (2, None)] {
			let mut moof = moof(count, tfdt);
			let mut encoded = Vec::new();
			moof.encode(&mut encoded).unwrap();
			// The samples follow the `moof` and the `mdat` header.
			let start = data.len() as u64;
			moof.traf[0].trun[0].data_offset = Some(encoded.len() as i32 + 8);
			moof.encode(&mut data).unwrap();
			data.extend(mdat(count));
			let samples = index(data.clone()).unwrap().remove(0).samples;
			assert_eq!(
				samples.last().unwrap().offset,
				start + encoded.len() as u64 + 8 + u64::from(SIZE * (count - 1))
			);
		}
		let samples = index(data).unwrap().remove(0).samples;
		assert_eq!(pts(&samples), [0, 100, 200, 300, 400]);
	}

	/// A `udta` whose `meta/ilst` repeats its copyright item, as some encoders write.
	fn repeated_metadata() -> Vec<u8> {
		// A `data` box of text: type 1 for UTF-8, then the locale.
		let data = mp4_box(b"data", &[0, 0, 0, 1, 0, 0, 0, 0, b'x']);
		let item = mp4_box(b"cprt", &data);
		let ilst = mp4_box(b"ilst", &[item.clone(), item].concat());
		let hdlr = mp4_box(b"hdlr", &[&[0; 8][..], b"mdir", &[0; 13]].concat());
		// `meta` is a full box: version and flags precede its children.
		let meta = mp4_box(b"meta", &[&[0; 4][..], &hdlr, &ilst].concat());
		mp4_box(b"udta", &meta)
	}

	/// Repeated metadata in the `moov` and a `trak`, which mp4-atom refuses, is skipped.
	#[test]
	fn repeated_metadata_is_skipped() {
		let mp4_atom::Moov { mvhd, trak, .. } = moov(stbl(3), Vec::new());
		let mut mvhd_box = Vec::new();
		mvhd.encode(&mut mvhd_box).unwrap();
		let mut trak_box = Vec::new();
		trak[0].encode(&mut trak_box).unwrap();
		let trak = mp4_box(b"trak", &[&trak_box[8..], &repeated_metadata()].concat());
		let moov = mp4_box(b"moov", &[mvhd_box, trak, repeated_metadata()].concat());
		// When this starts failing, mp4-atom accepts repeated metadata, and the reader can decode
		// the `moov` as it is.
		assert!(matches!(
			mp4_atom::Moov::decode(&mut moov.as_slice()),
			Err(mp4_atom::Error::DuplicateBox(_))
		));
		let tracks = index([mdat(3), moov].concat()).unwrap();
		assert_eq!(pts(&tracks[0].samples), [0, 100, 200]);

		// The same in a `traf`.
		let mut mfhd = Vec::new();
		mp4_atom::Mfhd::default().encode(&mut mfhd).unwrap();
		let mut traf = Vec::new();
		mp4_atom::Traf::default().encode(&mut traf).unwrap();
		let traf = mp4_box(b"traf", &[&traf[8..], &repeated_metadata()].concat());
		let moof = mp4_box(b"moof", &[mfhd, traf].concat());
		assert!(matches!(
			mp4_atom::Moof::decode(&mut moof.as_slice()),
			Err(mp4_atom::Error::DuplicateBox(_))
		));
		assert_eq!(decode_moof(&moof).unwrap().traf.len(), 1);
	}

	/// Bytes after the last box, too few for a box header, are skipped; a longer tail is a box,
	/// and one claiming a `moov` past the end fails.
	#[test]
	fn trailing_bytes() {
		let mut data = mdat(3);
		moov(stbl(3), Vec::new()).encode(&mut data).unwrap();
		let tracks = index([&data[..], b"\r\n"].concat()).unwrap();
		assert_eq!(pts(&tracks[0].samples), [0, 100, 200]);

		data.extend_from_slice(&[0, 0, 1, 0, b'm', b'o', b'o', b'v', 0, 0]);
		assert!(matches!(index(data), Err(crate::Error::Cmaf(Error::BoxPastEnd(_)))));
	}

	/// A `moov` declaring more than the file holds is refused before any of it is read.
	#[test]
	fn a_huge_moov_is_refused() {
		let mut data = mp4_box(b"moov", &[]);
		data[..4].copy_from_slice(&[0, 0, 0, 1]);
		data.extend_from_slice(&(fmp4::MAX_MOOV_BYTES + 17).to_be_bytes());
		assert!(matches!(index(data), Err(crate::Error::Cmaf(Error::BoxPastEnd(_)))));
	}

	/// Where the box `kind` starts in `data`, at or after `from`.
	fn find(data: &[u8], from: usize, kind: &[u8; 4]) -> usize {
		from + data[from..]
			.windows(4)
			.position(|window| window == kind)
			.expect("the file has the box")
	}

	/// `faststart.mp4` with its `avc1` sample entry declaring a height of 47.
	fn declares_height_47() -> Vec<u8> {
		let mut data = include_bytes!("test_data/faststart.mp4").to_vec();
		// The width and height follow the `avc1` kind after 24 bytes of reserved fields, data
		// reference index, and pre-defined fields. The `ftyp` names `avc1` as a brand too, so
		// the search starts at the `stsd`.
		let height = find(&data, find(&data, 0, b"stsd"), b"avc1") + 4 + 26;
		assert_eq!(data[height..height + 2], 48u16.to_be_bytes());
		data[height..height + 2].copy_from_slice(&47u16.to_be_bytes());
		data
	}

	fn video(tracks: &[Track]) -> &VideoConfig {
		tracks
			.iter()
			.find_map(|track| match &track.config {
				Config::Video(config) => Some(config),
				Config::Audio(_) => None,
			})
			.expect("a video track")
	}

	/// An H.264 track takes its picture size from the SPS rather than a sample entry that
	/// disagrees, and from the entry when no SPS parses.
	#[test]
	fn the_sps_sets_the_picture_size() {
		let tracks = index(declares_height_47()).unwrap();
		let config = video(&tracks);
		assert_eq!((config.coded_width, config.coded_height), (Some(64), Some(48)));

		let mut data = declares_height_47();
		// After the `avcC` kind come 6 bytes of fields, the 2-byte length of the first SPS, and
		// its NAL header. Zeros then fail its first Exp-Golomb code.
		let avcc = find(&data, 0, b"avcC");
		let len = usize::from(u16::from_be_bytes([data[avcc + 10], data[avcc + 11]]));
		data[avcc + 13..avcc + 12 + len].fill(0);
		let tracks = index(data).unwrap();
		let config = video(&tracks);
		assert_eq!((config.coded_width, config.coded_height), (Some(64), Some(47)));
	}

	/// A `moov`-first file lists both tracks, with the audio's priming trimmed, and reads each
	/// sample back from where it lies.
	#[test]
	fn a_faststart_file_reads_back() {
		let data = include_bytes!("test_data/faststart.mp4");
		let mut reader = Reader::new(Cursor::new(&data[..])).unwrap();
		let tracks = reader.tracks().to_vec();
		assert_eq!(tracks.len(), 2);

		let video = &tracks[0];
		assert!(matches!(&video.config, Config::Video(config) if config.codec.to_string().starts_with("avc1")));
		let step = video.timescale.as_u64() / 10;
		assert_eq!(
			pts(&video.samples),
			(0..10).map(|frame| frame * step).collect::<Vec<_>>()
		);
		let keyframes: Vec<bool> = video.samples.iter().map(|sample| sample.keyframe).collect();
		assert_eq!(keyframes, (0..10).map(|frame| frame % 5 == 0).collect::<Vec<_>>());

		let audio = &tracks[1];
		let Config::Audio(config) = &audio.config else {
			panic!("the second track is audio");
		};
		assert_eq!((config.sample_rate, config.channel_count), (16_000, 1));
		// 17 AAC frames, the first of which the edit list trims as the encoder's priming.
		assert_eq!(audio.samples.len(), 16);
		assert_eq!(audio.samples[0].pts, 0);
		assert!(audio.samples.iter().all(|sample| sample.duration == 1024));

		for sample in video.samples.iter().chain(&audio.samples) {
			let payload = reader.read(sample).unwrap();
			let at = sample.offset as usize;
			assert_eq!(payload, data[at..at + sample.size as usize]);
		}
	}

	/// A `moov`-last file with B-frames decodes two frames ahead of its presentation.
	#[test]
	fn a_moov_last_file_with_b_frames() {
		let tracks = index(include_bytes!("test_data/bframes.mp4").to_vec()).unwrap();
		let samples = &tracks[0].samples;
		assert_eq!(samples.len(), 10);
		let step = tracks[0].timescale.as_u64() as i64 / 10;
		let dts: Vec<i64> = samples.iter().map(|sample| sample.dts).collect();
		assert_eq!(dts, (-2..8).map(|frame| frame * step).collect::<Vec<_>>());
		let mut presented = pts(samples);
		presented.sort();
		assert_eq!(presented, (0..10).map(|frame| frame * step as u64).collect::<Vec<_>>());
	}
}
