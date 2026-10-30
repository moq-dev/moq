//! Regular MP4 files, recorded crash-safe.
//!
//! [`Export`] writes a file that is a valid fragmented MP4 while it grows, so a crash still
//! leaves a playable file, and [`Export::finish`] turns it into a regular MP4 with its `moov`
//! at the end without moving any media, the way OBS's hybrid MP4 does.
//!
//! The file starts with `ftyp`, a reserved 16-byte `free` box and the fragmented `moov`, then
//! the moof+mdat fragments. Finishing appends a `moov` that indexes every sample where it
//! already lies, then overwrites the reserved box with a 64-bit `mdat` header covering
//! everything before that `moov`. The reserved box must stay `free` until then: an `mdat`
//! header there would swallow the fragmented `moov` and leave a crashed file unreadable.
//!
//! A rendition that joins after the fragmented `moov` is written into `free` boxes, which a
//! reader of the unfinished file skips, and only the final `moov` indexes it.

use std::collections::{BTreeMap, HashSet};
use std::task::Poll;
use std::time::Duration;

use bytes::{BufMut, Bytes, BytesMut};
use mp4_atom::{Decode, Encode};

use crate::Result;
use crate::catalog::Stream;
use crate::container::fmp4::{self, Chunk, Error};

/// The reserved box: the size of a 64-bit `mdat` header.
const RESERVED: u64 = 16;

/// The `sample_is_non_sync_sample` bit of ISO/IEC 14496-12 sample flags.
const NON_SYNC: u32 = 0x0001_0000;

/// Subscribe to a moq broadcast and record it as a crash-safe regular MP4.
///
/// [`next`](Self::next) returns the bytes to append to the file, the same fMP4 an
/// [`fmp4::Export`] writes plus the reserved box, so the file plays if the recording never
/// finishes. [`finish`](Self::finish) then returns the trailing `moov` and the `mdat` header
/// that make it a regular MP4. Writing those needs a file that can be overwritten in place,
/// not a stream.
///
/// Unlike [`fmp4::Export`], a rendition that joins after the fragmented `moov` is recorded.
pub struct Export<S: Stream> {
	fmp4: fmp4::Export<S>,

	/// Bytes returned so far, which is where the next chunk starts in the file.
	written: u64,

	/// The fragmented `moov`, once written.
	header: Option<Header>,

	/// What each track wrote, by track id.
	tracks: BTreeMap<u32, Track>,
}

/// What the fragmented `moov` fixed.
struct Header {
	/// Where the reserved box starts.
	reserved: u64,
	/// The movie timescale.
	timescale: u32,
	/// The tracks it declares. Any other track joined later.
	declared: HashSet<u32>,
}

/// What one track wrote, in decode order.
#[derive(Default)]
struct Track {
	/// One chunk per `trun`, its samples contiguous in the file.
	chunks: Vec<Run>,
	samples: Vec<Sample>,
}

struct Run {
	/// Where the first sample starts in the file.
	offset: u64,
	/// The first sample's decode time, in media ticks.
	decode_time: u64,
	/// How many samples the run holds.
	count: u32,
}

struct Sample {
	size: u32,
	duration: u32,
	cts: i32,
	sync: bool,
}

/// The writes that finish a recording, in order.
#[derive(Clone, Debug)]
pub struct Finish {
	/// The regular `moov`, appended after everything [`Export::next`] returned.
	pub moov: Bytes,

	/// Where [`mdat`](Self::mdat) overwrites the reserved box, from the start of the file.
	pub mdat_offset: u64,

	/// The 64-bit `mdat` header that covers the media, written last.
	pub mdat: Bytes,
}

impl<S: Stream> Export<S> {
	/// Subscribe to `source` and record it, driving track (un)subscription from `catalog`.
	pub fn new(source: crate::Source, catalog: S) -> Self {
		Self {
			fmp4: fmp4::Export::new(source, catalog).with_late_tracks(),
			written: 0,
			header: None,
			tracks: BTreeMap::new(),
		}
	}

	/// Set the max delay for each per-track source; see [`fmp4::Export::with_max_delay`].
	pub fn with_max_delay(mut self, max_delay: Duration) -> Self {
		self.fmp4 = self.fmp4.with_max_delay(max_delay);
		self
	}

	/// Cap the fragment duration; see [`fmp4::Export::with_fragment_duration`].
	///
	/// A fragment is what a crash can lose, besides being the unit a `moof` indexes.
	pub fn with_fragment_duration(mut self, duration: impl Into<Option<Duration>>) -> Self {
		self.fmp4 = self.fmp4.with_fragment_duration(duration);
		self
	}

	/// The next bytes to append to the file, or `None` once the broadcast ends.
	pub async fn next(&mut self) -> Result<Option<Bytes>> {
		kio::wait(|waiter| self.poll_next(waiter)).await
	}

	/// Poll-based variant of [`Self::next`].
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Bytes>>> {
		let Some(chunk) = std::task::ready!(self.fmp4.poll_next_chunk(waiter)?) else {
			return Poll::Ready(Ok(None));
		};
		let data = match chunk {
			Chunk::Init(init) => self.init(&init)?,
			Chunk::Fragment(fragment) => self.fragment(fragment.data)?,
		};
		self.written += data.len() as u64;
		Poll::Ready(Ok(Some(data)))
	}

	/// The writes that turn the file into a regular MP4, or `None` when nothing was written.
	///
	/// Media still buffered for a fragment that has not closed is left out.
	pub fn finish(self) -> Result<Option<Finish>> {
		let Some(header) = &self.header else {
			return Ok(None);
		};

		let mut timelines = Vec::new();
		for (&id, track) in self.tracks.iter().filter(|(_, track)| !track.samples.is_empty()) {
			let Some((trak, _)) = self.fmp4.declaration(id) else {
				return Err(unindexable(id, "the track has no declaration").into());
			};
			timelines.push((trak, track, Timeline::new(id, trak, track)?));
		}

		let movie = u128::from(header.timescale);
		let to_movie = |ticks: u64, timescale: u32| {
			u64::try_from(u128::from(ticks) * movie / u128::from(timescale)).map_err(|_| Error::PtsOverflow)
		};
		// The earliest track starts the movie; the rest start that much later.
		let mut origin = u64::MAX;
		for (trak, _, timeline) in &timelines {
			origin = origin.min(to_movie(timeline.start, trak.mdia.mdhd.timescale)?);
		}

		let mut traks = Vec::new();
		for (trak, track, timeline) in timelines {
			let timescale = trak.mdia.mdhd.timescale;
			let delay = to_movie(timeline.start, timescale)? - origin;
			let presented = to_movie(timeline.duration, timescale)?;
			let mut trak = trak.clone();
			trak.mdia.mdhd.duration = timeline.deltas.iter().map(|&delta| u64::from(delta)).sum();
			trak.tkhd.duration = delay + presented;
			trak.edts = edits(delay, timeline.media_time, presented);
			trak.mdia.minf.stbl = sample_table(&trak.mdia.minf.stbl.stsd, track, &timeline.deltas);
			traks.push(trak);
		}
		traks.sort_by_key(|trak| (trak.mdia.hdlr.handler != b"vide".into(), trak.tkhd.track_id));

		let moov = mp4_atom::Moov {
			mvhd: mp4_atom::Mvhd {
				timescale: header.timescale,
				duration: traks.iter().map(|trak| trak.tkhd.duration).max().unwrap_or(0),
				// Normal playback rate and full volume; both default to 0.
				rate: mp4_atom::FixedPoint::from(1),
				volume: mp4_atom::FixedPoint::from(1),
				next_track_id: traks.iter().map(|trak| trak.tkhd.track_id).max().unwrap_or(0) + 1,
				..Default::default()
			},
			trak: traks,
			..Default::default()
		};
		let mut buf = Vec::new();
		moov.encode(&mut buf)?;

		// Everything from the reserved box to the moov, the fragmented moov and every moof
		// included, becomes one mdat that a reader skips through to the moov.
		let mut mdat = BytesMut::with_capacity(RESERVED as usize);
		mdat.put_u32(1);
		mdat.put_slice(b"mdat");
		mdat.put_u64(self.written - header.reserved);

		Ok(Some(Finish {
			moov: Bytes::from(buf),
			mdat_offset: header.reserved,
			mdat: mdat.freeze(),
		}))
	}

	/// The file header: the init segment's `ftyp`, the reserved box, then its `moov`.
	fn init(&mut self, init: &Bytes) -> Result<Bytes> {
		let mut cursor = std::io::Cursor::new(init.as_ref());
		mp4_atom::Ftyp::decode(&mut cursor)?;
		let split = cursor.position() as usize;
		let moov = mp4_atom::Moov::decode(&mut cursor)?;

		let mut data = BytesMut::with_capacity(init.len() + RESERVED as usize);
		data.put_slice(&init[..split]);
		data.put_u32(RESERVED as u32);
		data.put_slice(b"free");
		data.put_u64(0);
		data.put_slice(&init[split..]);

		self.header = Some(Header {
			reserved: self.written + split as u64,
			timescale: moov.mvhd.timescale,
			declared: moov.trak.iter().map(|trak| trak.tkhd.track_id).collect(),
		});
		Ok(data.freeze())
	}

	/// Index a moof+mdat fragment where it will land, wrapping a late track's in a `free` box.
	fn fragment(&mut self, fragment: Bytes) -> Result<Bytes> {
		let header = self.header.as_ref().expect("the init segment comes first");
		let moof = mp4_atom::Moof::decode(&mut std::io::Cursor::new(fragment.as_ref()))?;

		let mut data = BytesMut::new();
		if moof
			.traf
			.iter()
			.any(|traf| !header.declared.contains(&traf.tfhd.track_id))
		{
			mp4_atom::Header {
				kind: b"free".into(),
				size: Some(fragment.len()),
			}
			.encode(&mut data)?;
		}
		let start = self.written + data.len() as u64;
		for traf in &moof.traf {
			self.index(start, traf)?;
		}
		data.put_slice(&fragment);
		Ok(data.freeze())
	}

	/// Record the samples of `traf`, whose moof starts at `moof` in the file.
	fn index(&mut self, moof: u64, traf: &mp4_atom::Traf) -> Result<()> {
		let id = traf.tfhd.track_id;
		let Some((_, trex)) = self.fmp4.declaration(id) else {
			return Err(unindexable(id, "the track has no declaration").into());
		};
		let Some(tfdt) = &traf.tfdt else {
			return Err(unindexable(id, "a fragment has no decode time").into());
		};
		// The data offsets are then relative to the moof, as fmp4::Export writes them.
		if traf.tfhd.base_data_offset.is_some() {
			return Err(unindexable(id, "a fragment has an explicit base data offset").into());
		}
		let duration = traf
			.tfhd
			.default_sample_duration
			.unwrap_or(trex.default_sample_duration);
		let size = traf.tfhd.default_sample_size.unwrap_or(trex.default_sample_size);
		let flags = traf.tfhd.default_sample_flags.unwrap_or(trex.default_sample_flags);

		let track = self.tracks.entry(id).or_default();
		let mut decode_time = tfdt.base_media_decode_time;
		for trun in traf.trun.iter().filter(|trun| !trun.entries.is_empty()) {
			let Some(offset) = trun.data_offset.and_then(|offset| u64::try_from(offset).ok()) else {
				return Err(unindexable(id, "a run has no data offset").into());
			};
			track.chunks.push(Run {
				offset: moof + offset,
				decode_time,
				count: trun.entries.len() as u32,
			});
			for entry in &trun.entries {
				let sample = Sample {
					size: entry.size.unwrap_or(size),
					duration: entry.duration.unwrap_or(duration),
					cts: entry.cts.unwrap_or(0),
					sync: entry.flags.unwrap_or(flags) & NON_SYNC == 0,
				};
				decode_time = decode_time
					.checked_add(u64::from(sample.duration))
					.ok_or(Error::PtsOverflow)?;
				track.samples.push(sample);
			}
		}
		Ok(())
	}
}

fn unindexable(track: u32, reason: &'static str) -> Error {
	Error::Unindexable { track, reason }
}

/// A track's samples laid on one media timeline.
struct Timeline {
	/// Each sample's `stts` delta: up to the next sample's decode time, which stretches a
	/// sample over any gap after it, and the last sample's own duration.
	deltas: Vec<u32>,
	/// Where presentation starts on the media timeline, past the first decode time.
	media_time: u64,
	/// When presentation starts, in the media ticks the fragments were written in.
	start: u64,
	/// How long the track presents, in media ticks.
	duration: u64,
}

impl Timeline {
	fn new(id: u32, trak: &mp4_atom::Trak, track: &Track) -> Result<Self> {
		let mut decode_times = Vec::with_capacity(track.samples.len());
		let mut samples = track.samples.iter();
		for run in &track.chunks {
			let mut time = run.decode_time;
			for sample in samples.by_ref().take(run.count as usize) {
				decode_times.push(time);
				time += u64::from(sample.duration);
			}
		}

		let mut deltas = Vec::with_capacity(decode_times.len());
		for (i, &time) in decode_times.iter().enumerate() {
			let delta = match decode_times.get(i + 1) {
				Some(&next) if next <= time => return Err(unindexable(id, "decode time stops advancing").into()),
				Some(&next) => {
					u32::try_from(next - time).map_err(|_| unindexable(id, "a gap overflows a sample delta"))?
				}
				None => track.samples[i].duration,
			};
			deltas.push(delta);
		}

		let Some(&first) = decode_times.first() else {
			return Ok(Self {
				deltas,
				media_time: 0,
				start: 0,
				duration: 0,
			});
		};
		let mut presented = i128::MAX;
		let mut end = i128::MIN;
		for ((&time, sample), &delta) in decode_times.iter().zip(&track.samples).zip(&deltas) {
			let pts = i128::from(time) + i128::from(sample.cts);
			presented = presented.min(pts);
			end = end.max(pts + i128::from(delta));
		}
		// An Opus track presents one pre-skip into its first packet, and nothing is presented
		// before the first decode time.
		let start = (presented + i128::from(fmp4::priming(trak))).max(i128::from(first));
		let start = u64::try_from(start).map_err(|_| Error::PtsOverflow)?;
		let duration = u64::try_from((end - i128::from(start)).max(0)).map_err(|_| Error::PtsOverflow)?;
		Ok(Self {
			deltas,
			media_time: start - first,
			start,
			duration,
		})
	}
}

/// The edit list placing a track `delay` into the movie and its media from `media_time`, both
/// lasting `presented`; `None` when the media plays from its start at the movie's.
fn edits(delay: u64, media_time: u64, presented: u64) -> Option<mp4_atom::Edts> {
	if delay == 0 && media_time == 0 {
		return None;
	}
	let mut entries = Vec::new();
	if delay > 0 {
		entries.push(mp4_atom::ElstEntry {
			segment_duration: delay,
			media_time: None,
			media_rate: mp4_atom::FixedPoint::from(1),
		});
	}
	entries.push(mp4_atom::ElstEntry {
		segment_duration: presented,
		media_time: Some(media_time),
		media_rate: mp4_atom::FixedPoint::from(1),
	});
	Some(mp4_atom::Edts {
		elst: Some(mp4_atom::Elst { entries }),
	})
}

/// The sample tables indexing `track` where its samples lie, each `trun` one chunk.
fn sample_table(stsd: &mp4_atom::Stsd, track: &Track, deltas: &[u32]) -> mp4_atom::Stbl {
	let mut stts: Vec<mp4_atom::SttsEntry> = Vec::new();
	for &delta in deltas {
		match stts.last_mut() {
			Some(last) if last.sample_delta == delta => last.sample_count += 1,
			_ => stts.push(mp4_atom::SttsEntry {
				sample_count: 1,
				sample_delta: delta,
			}),
		}
	}

	let mut ctts: Vec<mp4_atom::CttsEntry> = Vec::new();
	for sample in &track.samples {
		let offset = i64::from(sample.cts);
		match ctts.last_mut() {
			Some(last) if last.sample_offset == offset => last.sample_count += 1,
			_ => ctts.push(mp4_atom::CttsEntry {
				sample_count: 1,
				sample_offset: offset,
			}),
		}
	}

	let mut stsc: Vec<mp4_atom::StscEntry> = Vec::new();
	for (i, run) in track.chunks.iter().enumerate() {
		if stsc.last().is_none_or(|last| last.samples_per_chunk != run.count) {
			stsc.push(mp4_atom::StscEntry {
				first_chunk: i as u32 + 1,
				samples_per_chunk: run.count,
				sample_description_index: 1,
			});
		}
	}

	let sizes: Vec<u32> = track.samples.iter().map(|sample| sample.size).collect();
	let stsz = match sizes.first() {
		Some(&size) if sizes.iter().all(|&other| other == size) => mp4_atom::StszSamples::Identical {
			count: sizes.len() as u32,
			size,
		},
		_ => mp4_atom::StszSamples::Different { sizes },
	};

	let all_sync = track.samples.iter().all(|sample| sample.sync);
	mp4_atom::Stbl {
		stsd: stsd.clone(),
		stts: mp4_atom::Stts { entries: stts },
		ctts: ctts
			.iter()
			.any(|entry| entry.sample_offset != 0)
			.then_some(mp4_atom::Ctts { entries: ctts }),
		stss: (!all_sync).then(|| mp4_atom::Stss {
			entries: (1..)
				.zip(&track.samples)
				.filter(|(_, sample)| sample.sync)
				.map(|(number, _)| number)
				.collect(),
		}),
		stsc: mp4_atom::Stsc { entries: stsc },
		stsz: mp4_atom::Stsz { samples: stsz },
		co64: Some(mp4_atom::Co64 {
			entries: track.chunks.iter().map(|run| run.offset).collect(),
		}),
		..Default::default()
	}
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use mp4_atom::DecodeMaybe;

	use crate::container::test_util::{Live, raw_frame, video_frame};

	/// Every retained group, since these tests publish before they record.
	const MAX_DELAY: Duration = Duration::from_secs(30);

	/// A 20 ms Opus packet (TOC config 1, one frame).
	const OPUS: &[u8] = &[0x08, 0xaa, 0xbb, 0xcc];

	type Producer = crate::container::Producer<crate::catalog::hang::Container>;

	fn add_opus(live: &mut Live) -> Producer {
		use hang::catalog::{AudioCodec, AudioConfig, Container};

		live.add_track(".opus", |catalog, name| {
			let mut config = AudioConfig::new(AudioCodec::Opus, 48_000, 2);
			config.container = Container::Legacy;
			catalog.modify().unwrap().audio.renditions.insert(name, config);
		})
	}

	async fn export(live: &Live) -> super::Export<crate::catalog::Consumer> {
		super::Export::new(live.source().await, live.catalog_stream().await).with_max_delay(MAX_DELAY)
	}

	/// Append everything the export writes without waiting for more input.
	async fn record(export: &mut super::Export<crate::catalog::Consumer>, file: &mut Vec<u8>) {
		while let Ok(next) = tokio::time::timeout(Duration::from_millis(1), export.next()).await {
			match next.expect("export failed") {
				Some(data) => file.extend_from_slice(&data),
				None => break,
			}
		}
	}

	/// Finish the recording in `file` the way a writer does.
	fn finish(export: super::Export<crate::catalog::Consumer>, file: &mut Vec<u8>) {
		let finish = export.finish().expect("finish").expect("something was written");
		file.extend_from_slice(&finish.moov);
		let offset = finish.mdat_offset as usize;
		file[offset..offset + finish.mdat.len()].copy_from_slice(&finish.mdat);
	}

	fn atoms(file: &[u8]) -> Vec<mp4_atom::Any> {
		let mut cursor = std::io::Cursor::new(file);
		let mut atoms = Vec::new();
		while let Some(atom) = mp4_atom::Any::decode_maybe(&mut cursor).expect("decode file") {
			atoms.push(atom);
		}
		atoms
	}

	fn kinds(atoms: &[mp4_atom::Any]) -> Vec<String> {
		atoms.iter().map(|atom| atom.kind().to_string()).collect()
	}

	fn moov(atoms: Vec<mp4_atom::Any>) -> mp4_atom::Moov {
		atoms
			.into_iter()
			.find_map(|atom| match atom {
				mp4_atom::Any::Moov(moov) => Some(moov),
				_ => None,
			})
			.expect("a moov")
	}

	fn trak<'a>(moov: &'a mp4_atom::Moov, handler: &[u8; 4]) -> &'a mp4_atom::Trak {
		moov.trak
			.iter()
			.find(|trak| trak.mdia.hdlr.handler == handler.into())
			.expect("a trak")
	}

	/// Each sample's bytes, found through the sample tables alone.
	fn samples<'a>(file: &'a [u8], trak: &mp4_atom::Trak) -> Vec<&'a [u8]> {
		let stbl = &trak.mdia.minf.stbl;
		let sizes: Vec<u32> = match &stbl.stsz.samples {
			mp4_atom::StszSamples::Identical { count, size } => vec![*size; *count as usize],
			mp4_atom::StszSamples::Different { sizes } => sizes.clone(),
		};
		let offsets = &stbl.co64.as_ref().expect("co64").entries;
		let mut sizes = sizes.into_iter();
		let mut samples = Vec::new();
		for (index, &offset) in offsets.iter().enumerate() {
			let chunk = index as u32 + 1;
			let entry = stbl
				.stsc
				.entries
				.iter()
				.rev()
				.find(|entry| entry.first_chunk <= chunk)
				.expect("stsc");
			let mut offset = offset as usize;
			for size in sizes.by_ref().take(entry.samples_per_chunk as usize) {
				samples.push(&file[offset..offset + size as usize]);
				offset += size as usize;
			}
		}
		assert!(sizes.next().is_none(), "every sample is in a chunk");
		samples
	}

	/// An Annex-B frame's NALs with 4-byte lengths for start codes, as the export writes it.
	fn length_prefixed(frame: &crate::container::Frame) -> Vec<u8> {
		let payload = &frame.payload[..];
		let starts: Vec<usize> = (0..payload.len().saturating_sub(3))
			.filter(|&i| payload[i..i + 4] == [0, 0, 0, 1])
			.collect();
		let mut out = Vec::new();
		for (i, &start) in starts.iter().enumerate() {
			let end = starts.get(i + 1).copied().unwrap_or(payload.len());
			let nal = &payload[start + 4..end];
			out.extend_from_slice(&(nal.len() as u32).to_be_bytes());
			out.extend_from_slice(nal);
		}
		out
	}

	/// A recording that runs to its end is fragmented until finished, and a regular MP4
	/// indexing every sample in place after.
	#[tokio::test(start_paused = true)]
	async fn a_finished_recording_is_a_regular_mp4() {
		let mut live = Live::avc3();
		let mut audio = add_opus(&mut live);
		let video: Vec<_> = (0..9u64).map(|i| video_frame(i * 33_000, i % 3 == 0)).collect();
		for frame in &video {
			live.track.write(frame.clone()).unwrap();
		}
		for i in 0..12u64 {
			audio.write(raw_frame(i * 20_000, OPUS, i % 5 == 0)).unwrap();
		}
		live.track.finish().unwrap();
		audio.finish().unwrap();

		let mut export = export(&live).await;
		let mut file = Vec::new();
		record(&mut export, &mut file).await;

		// Unfinished, it is the fMP4 a crash would leave.
		let unfinished = atoms(&file);
		assert_eq!(kinds(&unfinished[..3]), ["ftyp", "free", "moov"]);
		assert!(
			kinds(&unfinished[3..])
				.iter()
				.all(|kind| kind == "moof" || kind == "mdat")
		);
		assert!(moov(unfinished).mvex.is_some());

		let media = file.len();
		finish(export, &mut file);
		let finished = atoms(&file);
		assert_eq!(kinds(&finished), ["ftyp", "mdat", "moov"]);
		let moov = moov(finished);
		assert!(moov.mvex.is_none(), "a regular MP4 has no fragments to extend into");

		let video_trak = trak(&moov, b"vide");
		let written: Vec<Vec<u8>> = video.iter().map(length_prefixed).collect();
		assert_eq!(samples(&file, video_trak), written);
		let stbl = &video_trak.mdia.minf.stbl;
		assert_eq!(stbl.stss.as_ref().expect("stss").entries, [1, 4, 7]);
		assert_eq!(stbl.stsc.entries.len(), 1, "every GOP is a chunk of three");
		assert!(
			stbl.co64
				.as_ref()
				.unwrap()
				.entries
				.iter()
				.all(|&offset| (offset as usize) < media)
		);

		let audio_trak = trak(&moov, b"soun");
		assert_eq!(samples(&file, audio_trak), vec![OPUS; 12]);
		assert!(
			audio_trak.mdia.minf.stbl.stss.is_none(),
			"every audio sample is a sync sample"
		);
	}

	/// A rendition that joins after the fragmented moov hides in free boxes until the final
	/// moov indexes it.
	#[tokio::test(start_paused = true)]
	async fn a_late_rendition_is_recorded() {
		let mut live = Live::avc3();
		live.track.write(video_frame(0, true)).unwrap();
		let mut export = export(&live).await;
		let mut file = Vec::new();
		record(&mut export, &mut file).await;
		assert_eq!(moov(atoms(&file)).trak.len(), 1);

		let mut audio = add_opus(&mut live);
		for i in 0..10u64 {
			audio.write(raw_frame(i * 20_000, OPUS, i % 5 == 0)).unwrap();
		}
		audio.finish().unwrap();
		live.track.write(video_frame(200_000, true)).unwrap();
		live.track.finish().unwrap();
		record(&mut export, &mut file).await;

		let unfinished = atoms(&file);
		let frees = unfinished
			.iter()
			.filter(|atom| matches!(atom, mp4_atom::Any::Free(_)))
			.count();
		assert_eq!(frees, 3, "the reserved box and both audio fragments");

		finish(export, &mut file);
		let moov = moov(atoms(&file));
		assert_eq!(moov.trak.len(), 2);
		assert_eq!(samples(&file, trak(&moov, b"soun")), vec![OPUS; 10]);
		assert_eq!(samples(&file, trak(&moov, b"vide")).len(), 2);
	}

	/// A track that starts later waits behind an empty edit, and a gap stretches the sample
	/// before it rather than pulling later samples early.
	#[tokio::test(start_paused = true)]
	async fn later_starts_and_gaps_keep_their_time() {
		let mut live = Live::avc3();
		let mut audio = add_opus(&mut live);
		for i in 0..10u64 {
			audio.write(raw_frame(i * 20_000, OPUS, i % 5 == 0)).unwrap();
		}
		audio.finish().unwrap();
		// Video starts 100 ms in, and its second GOP comes a second after the first.
		for (timestamp, keyframe) in [(100_000, true), (133_000, false), (1_133_000, true), (1_166_000, false)] {
			live.track.write(video_frame(timestamp, keyframe)).unwrap();
		}
		live.track.finish().unwrap();

		let mut export = export(&live).await;
		let mut file = Vec::new();
		record(&mut export, &mut file).await;
		finish(export, &mut file);
		let moov = moov(atoms(&file));

		let video = trak(&moov, b"vide");
		let movie = u64::from(moov.mvhd.timescale);
		let edits = &video.edts.as_ref().expect("edts").elst.as_ref().expect("elst").entries;
		assert_eq!(edits[0].media_time, None, "an empty edit first");
		assert_eq!(edits[0].segment_duration, movie / 10);
		assert_eq!(edits[1].media_time, Some(0));

		let timescale = u64::from(video.mdia.mdhd.timescale);
		let deltas: Vec<u64> = video
			.mdia
			.minf
			.stbl
			.stts
			.entries
			.iter()
			.flat_map(|entry| std::iter::repeat_n(u64::from(entry.sample_delta), entry.sample_count as usize))
			.collect();
		assert_eq!(deltas.len(), 4);
		assert_eq!(deltas[1], timescale, "the frame before the gap lasts through it");
	}

	/// Nothing to finish before the header is written.
	#[tokio::test(start_paused = true)]
	async fn nothing_written_needs_no_finish() {
		let live = Live::avc3();
		let export = export(&live).await;
		assert!(export.finish().unwrap().is_none());
	}
}
