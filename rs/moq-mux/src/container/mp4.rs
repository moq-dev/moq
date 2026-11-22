//! Regular MP4 files, recorded crash-safe.
//!
//! [`Export`] writes a file that is a valid fragmented MP4 while it grows, so a crash still
//! leaves a playable file, and [`Export::finish`] turns it into a regular MP4 with its `moov`
//! at the end without moving any media, the way OBS's hybrid MP4 does. [`repair`] finishes a
//! file the same way after a crash, and [`Finish::write`] makes either finish's writes.
//!
//! The file starts with `ftyp`, a reserved 16-byte `free` box and the fragmented `moov`, then
//! the moof+mdat fragments. Finishing appends a `moov` that indexes every sample where it
//! already lies, then overwrites the reserved box with a 64-bit `mdat` header covering
//! everything before that `moov`. The reserved box must stay `free` until then: an `mdat`
//! header there would swallow the fragmented `moov` and leave a crashed file unreadable.
//!
//! A finished file lists [`Chapter`]s two ways, so every player finds them: Nero's `chpl` box
//! in the `moov`, and a QuickTime chapter text track that the first track references with
//! `tref chap`. The text lies in an `mdat` between the media and the `moov`.
//!
//! A rendition that joins after the fragmented `moov` is written into `free` boxes, which a
//! reader of the unfinished file skips. Before a track's first fragment, and again whenever
//! its description changes, a `free` box holds a `moov` declaring it, so the final `moov`
//! can be rebuilt from the file alone.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::task::Poll;
use std::time::Duration;

use bytes::{BufMut, Bytes, BytesMut};
use moq_net::Timestamp;
use mp4_atom::{Decode, Encode};

use crate::Result;
use crate::catalog::Stream;
use crate::container::fmp4::{self, Chunk, Error};

/// The reserved box: the size of a 64-bit `mdat` header.
const RESERVED: u64 = 16;

/// The reserved box as a recording writes it, an empty `free` box.
const FREE: [u8; RESERVED as usize] = [0, 0, 0, RESERVED as u8, b'f', b'r', b'e', b'e', 0, 0, 0, 0, 0, 0, 0, 0];

/// What a `chpl` box's one-byte counts allow: chapters in the list, and bytes in a title.
const CHPL_LIMIT: usize = 255;

/// The chapter text track's timescales, finest first: the first one in which every chapter's
/// duration fits 31 bits, which readers such as FFmpeg's accept. Milliseconds cover 24 days a chapter.
const TEXT_TIMESCALES: [u32; 4] = [1000, 100, 10, 1];

/// What chapters add to a finish beyond the media's `moov`: at most 255 titles of 255 bytes
/// each in the `chpl` box, the text, and the text track's tables, with room to spare. It
/// bounds what a repair reads of a trailer that is not yet known to be one.
const CHAPTER_BYTES: u64 = (CHPL_LIMIT * (CHPL_LIMIT + 64) + 4096) as u64;

/// The `sample_is_non_sync_sample` bit of ISO/IEC 14496-12 sample flags.
const NON_SYNC: u32 = 0x0001_0000;

/// A moment in a recording that a player lists as a chapter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chapter {
	/// When the chapter starts on the recorded broadcast's timeline, the clock its frames'
	/// timestamps are on. One before the first sample or after the last is dropped.
	pub time: Timestamp,

	/// The chapter's name, at most 255 bytes. The limits on titles and on how many chapters
	/// a finish lists apply to the chapters it writes, not to those it drops.
	pub title: String,
}

/// Subscribe to a moq broadcast and record it as a crash-safe regular MP4.
///
/// [`poll_next`](Self::poll_next) returns the bytes to append to the file, the same fMP4 an
/// [`fmp4::Export`] writes plus the reserved box and track descriptions, so the file plays if
/// the recording never finishes. [`finish`](Self::finish) then returns the trailing `moov` and
/// the `mdat` header that make it a regular MP4. Writing those needs a file that can be
/// overwritten in place, not a stream.
///
/// Unlike [`fmp4::Export`], a rendition that joins after the fragmented `moov` is recorded.
pub struct Export<S: Stream> {
	fmp4: fmp4::Export<S>,

	/// Bytes returned so far, which is where the next chunk starts in the file.
	written: u64,

	/// What the file holds, once the fragmented `moov` is written.
	index: Option<Index>,

	/// The declaration the file last described each track with, by track id.
	described: HashMap<u32, Description>,
}

/// The writes that finish a recording, made by [`write`](Self::write).
#[derive(Clone, Debug)]
pub struct Finish {
	/// Where `moov` is written, from the start of the file, which is cut there first: the end
	/// of everything [`Export::poll_next`] returned, or of a repaired file's last complete
	/// fragment.
	moov_offset: u64,

	/// What follows the media: the chapter text's `mdat` if there are chapters, then the regular
	/// `moov`.
	tail: Bytes,

	/// Where `mdat` overwrites the reserved box, from the start of the file.
	mdat_offset: u64,

	/// The 64-bit `mdat` header that covers the media.
	mdat: Bytes,

	/// Whether a finish cut short left the reserved box a header that skips the fragmented
	/// `moov`, which must be `free` again before the cut removes the trailing one.
	reopen: bool,

	/// Whether `tail` is already what the file ends with after `moov_offset`, which a write
	/// then leaves alone, so a crash never leaves a file without it.
	kept: bool,
}

impl Finish {
	/// Durably turn the recording in `file` into a regular MP4, so a crash part way leaves a
	/// file that still plays and that [`repair`] finishes the same way.
	///
	/// It makes a repaired file's reserved box `free` again if a finish cut short left it a
	/// header, cuts the file after the media, appends the `moov`, then overwrites the reserved box,
	/// its last 8 bytes before its first 8 and syncing before each step that needs the one
	/// before on disk. Until the last write the box reads as one a player skips, `free` or a
	/// kind it does not know, never a broken `mdat` header.
	pub fn write(&self, file: &mut std::fs::File) -> std::io::Result<()> {
		self.write_to(file)
	}

	fn write_to(&self, file: &mut impl Disk) -> std::io::Result<()> {
		if self.reopen {
			file.write_at(self.mdat_offset, &FREE[..8])?;
			file.sync()?;
		}
		if !self.kept {
			// Cut before the moov lands where the cut bytes were, which could otherwise mix with it.
			file.set_len(self.moov_offset)?;
			file.sync()?;
			file.write_at(self.moov_offset, &self.tail)?;
		}
		// The size goes into the body of the `free` box, so only the type needs the moov first.
		let (kind, size) = self.mdat.split_at(8);
		file.write_at(self.mdat_offset + 8, size)?;
		file.sync()?;
		file.write_at(self.mdat_offset, kind)?;
		file.sync()
	}
}

/// What [`Finish::write`] needs from a file.
trait Disk {
	fn set_len(&mut self, len: u64) -> std::io::Result<()>;
	fn write_at(&mut self, offset: u64, data: &[u8]) -> std::io::Result<()>;
	/// Return once everything written so far is on disk.
	fn sync(&mut self) -> std::io::Result<()>;
}

impl Disk for std::fs::File {
	fn set_len(&mut self, len: u64) -> std::io::Result<()> {
		std::fs::File::set_len(self, len)
	}

	fn write_at(&mut self, offset: u64, data: &[u8]) -> std::io::Result<()> {
		use std::io::Write;
		self.seek(SeekFrom::Start(offset))?;
		self.write_all(data)
	}

	fn sync(&mut self) -> std::io::Result<()> {
		self.sync_all()
	}
}

impl<S: Stream> Export<S> {
	/// Subscribe to `source` and record it, driving track (un)subscription from `catalog`.
	pub fn new(source: crate::Source, catalog: S) -> Self {
		Self {
			fmp4: fmp4::Export::new(source, catalog).with_late_tracks(),
			written: 0,
			index: None,
			described: HashMap::new(),
		}
	}

	/// Set how far behind the live edge each per-track source plays; see [`fmp4::Export::with_delay`].
	pub fn with_delay(mut self, delay: crate::container::Delay) -> Self {
		self.fmp4 = self.fmp4.with_delay(delay);
		self
	}

	/// Cap the fragment duration; see [`fmp4::Export::with_fragment_duration`].
	///
	/// A fragment is what a crash can lose, besides being the unit a `moof` indexes.
	pub fn with_fragment_duration(mut self, duration: impl Into<Option<Duration>>) -> Self {
		self.fmp4 = self.fmp4.with_fragment_duration(duration);
		self
	}

	/// The next bytes to append to the file, or `None` once the broadcast ends or the export
	/// [stops](Self::stop).
	#[cfg_attr(feature = "async", kio::wrapper)]
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

	/// Stop recording before the broadcast ends. [`poll_next`](Self::poll_next) still returns the media
	/// already read, the fragment each track was buffering included, then `None`.
	pub fn stop(&mut self) {
		self.fmp4.stop();
	}

	/// The writes that turn the file into a regular MP4 listing `chapters`, or `None` when
	/// nothing was written. Pass `&[]` for a file without chapters.
	///
	/// At most 255 chapters fall within the recording, and each title is at most 255 bytes:
	/// those the recording drops count toward neither.
	///
	/// Media still buffered for a fragment that has not closed is left out, unless
	/// [`stop`](Self::stop) wrote it first.
	pub fn finish(self, chapters: &[Chapter]) -> Result<Option<Finish>> {
		self.index.map(|index| index.finish(self.written, chapters)).transpose()
	}

	/// The file header: the init segment's `ftyp`, the reserved box, then its `moov`.
	fn init(&mut self, init: &Bytes) -> Result<Bytes> {
		fmp4::metadata::check(init)?;
		let mut cursor = std::io::Cursor::new(init.as_ref());
		mp4_atom::Ftyp::decode(&mut cursor).map_err(crate::Error::mp4)?;
		let split = cursor.position() as usize;
		let moov = mp4_atom::Moov::decode(&mut cursor).map_err(crate::Error::mp4)?;

		let mut data = BytesMut::with_capacity(init.len() + RESERVED as usize);
		data.put_slice(&init[..split]);
		data.put_slice(&FREE);
		data.put_slice(&init[split..]);

		self.index = Some(Index::new(self.written + split as u64, &moov)?);
		self.described = self
			.fmp4
			.declarations()
			.map(|(id, trak, trex)| (id, Description::new(trak, trex)))
			.collect();
		Ok(data.freeze())
	}

	/// Index a moof+mdat fragment where it will land, after a description of each track the
	/// file has yet to describe as it is now, and wrapping a late track's in a `free` box.
	fn fragment(&mut self, fragment: Bytes) -> Result<Bytes> {
		fmp4::metadata::check(&fragment)?;
		let index = self.index.as_mut().expect("the init segment comes first");
		let moof = mp4_atom::Moof::decode(&mut std::io::Cursor::new(fragment.as_ref())).map_err(crate::Error::mp4)?;

		let mut data = BytesMut::new();
		for traf in &moof.traf {
			let id = traf.tfhd.track_id;
			let Some((trak, trex)) = self.fmp4.declaration(id) else {
				return Err(unindexable(id, "the track has no declaration").into());
			};
			if self
				.described
				.get(&id)
				.is_none_or(|known| known.trak != *trak || known.trex != *trex)
			{
				let description = Description::new(trak, trex);
				// Indexed as a repair reads it back, which keeps the two finishes identical.
				index.describe(&description.encode(index.timescale, &mut data)?)?;
				self.described.insert(id, description);
			}
		}
		if moof
			.traf
			.iter()
			.any(|traf| !index.declared.contains(&traf.tfhd.track_id))
		{
			mp4_atom::Header {
				kind: b"free".into(),
				size: Some(fragment.len()),
			}
			.encode(&mut data)
			.map_err(crate::Error::mp4)?;
		}
		index.fragment(self.written + data.len() as u64, &moof)?;
		data.put_slice(&fragment);
		Ok(data.freeze())
	}
}

/// Rebuild the writes that finish a recording [`Export`] left unfinished, such as after a
/// crash or a cut-short finish, or `None` when the file is already finished.
///
/// The final `moov` indexes every complete fragment, the same as finishing the recording
/// right after writing the last one. Whatever follows it, such as a partial fragment or
/// `moov`, is cut, so a repair cut short can itself be repaired. A file only counts as finished
/// when its `mdat` header and trailing `moov` are exactly what finishing it writes, `chapters`
/// included. Given none, a finished file's own chapters stay, so repairing a file never
/// drops them; given some, they replace the file's.
pub fn repair<F: Read + Seek>(file: &mut F, chapters: &[Chapter]) -> Result<Option<Finish>> {
	let eof = file.seek(SeekFrom::End(0))?;
	let Some(ftyp) = Atom::read(file, 0, eof)?.filter(|atom| atom.kind == *b"ftyp") else {
		return Err(Error::Unrepairable("it does not start with an ftyp box").into());
	};
	// Read as bytes, not a box: a finish cut short can leave it half `free` and half `mdat`.
	let reserved = ftyp.end;
	let Some(slot) = read_at(file, reserved, RESERVED, eof)? else {
		return Err(Error::Unrepairable("it ends before its header").into());
	};
	// A finish never moves the fragmented moov, which the mdat header then covers.
	let Some(header) = Atom::read(file, reserved + RESERVED, eof)?.filter(|atom| atom.kind == *b"moov") else {
		return Err(Error::Unrepairable("it has no complete moov after its reserved box").into());
	};
	let moov: mp4_atom::Moov = header.decode(file)?;
	if moov.mvex.is_none() {
		return Err(Error::Unrepairable("its moov is not fragmented").into());
	}
	let mut index = Index::new(reserved, &moov)?;

	// Where the last complete fragment ends, and the descriptions written since, which only
	// count once the fragment written with them is complete.
	let mut end = header.end;
	let mut described = Vec::new();
	let mut position = header.end;
	while let Some(atom) = Atom::read(file, position, eof)? {
		let moof = match &atom.kind {
			b"moof" => {
				let Some(mdat) = Atom::read(file, atom.end, eof)? else {
					break;
				};
				if mdat.kind != *b"mdat" {
					return Err(Error::Unrepairable("a moof is not followed by its mdat").into());
				}
				position = mdat.end;
				atom
			}
			b"free" => {
				position = atom.end;
				match Atom::read(file, atom.body, atom.end)? {
					Some(inner) if inner.kind == *b"moof" => {
						let mdat = Atom::read(file, inner.end, atom.end)?;
						if mdat.is_none_or(|mdat| mdat.kind != *b"mdat" || mdat.end != atom.end) {
							return Err(Error::Unrepairable("a free box holds a moof without its mdat").into());
						}
						inner
					}
					Some(inner) if inner.kind == *b"moov" => {
						described.push(inner.decode::<_, mp4_atom::Moov>(file)?);
						continue;
					}
					_ => return Err(Error::Unrepairable("a free box holds neither a fragment nor a moov").into()),
				}
			}
			// A finish appends the chapter text and a moov, which end the recording, or a
			// cut-short one part of it. Complete boxes after the text other than the moov are
			// fragments a damaged box hides, which a finish would cut.
			b"mdat" => {
				match Atom::read(file, atom.end, eof)? {
					Some(next) if next.kind != *b"moov" => {
						return Err(Error::Unrepairable("an mdat is followed by a box other than the moov").into());
					}
					Some(moov) => terminal(file, &moov, eof)?,
					None => {}
				}
				break;
			}
			b"moov" => {
				terminal(file, &atom, eof)?;
				break;
			}
			_ => return Err(Error::Unrepairable("it holds a box a recording never writes").into()),
		};
		for moov in described.drain(..) {
			index.describe(&moov)?;
		}
		index.fragment(moof.start, &moof.decode(file)?)?;
		end = position;
	}
	let mut finish = index.finish(end, chapters)?;

	// Each byte is the reserved box's or the mdat header's, whichever reached the disk.
	let torn = slot
		.iter()
		.zip(FREE.iter().zip(finish.mdat.iter()))
		.all(|(&byte, (&free, &mdat))| byte == free || byte == mdat);
	if !torn {
		return Err(Error::Unrepairable("its reserved box is neither free nor its mdat header").into());
	}
	// What a finish appended, unless anything differs, a cut-short one included. Given no
	// chapters, a finish that listed some counts too, which they would otherwise lose, so
	// its trailer stays as it is.
	let indexed = (eof - end == finish.tail.len() as u64
		&& read_at(file, end, eof - end, eof)?.is_some_and(|trailer| trailer == finish.tail))
		|| (chapters.is_empty() && keeps_chapters(file, end, eof, &finish.tail)?);
	// A trailer that is already right stays on disk, so no crash leaves the file without it.
	finish.kept = indexed;
	if indexed && slot == finish.mdat {
		return Ok(None);
	}
	finish.reopen = slot[..8] != FREE[..8];
	if !indexed && end < eof {
		tracing::warn!(
			offset = end,
			bytes = eof - end,
			"cutting what follows the last complete fragment"
		);
	}
	Ok(Some(finish))
}

/// The track that references the chapter text: the first enabled one, since a player reads
/// the reference from an enabled track, or the first when none is.
fn chap_carrier(traks: &[mp4_atom::Trak]) -> Option<usize> {
	if traks.is_empty() {
		return None;
	}
	Some(traks.iter().position(|trak| trak.tkhd.enabled).unwrap_or(0))
}

/// Refuse a `moov` that a complete box follows: a finish ends with it, so that box is a
/// fragment a damaged box hides, which a finish would cut.
fn terminal<F: Read + Seek>(file: &mut F, moov: &Atom, eof: u64) -> Result<()> {
	if Atom::read(file, moov.end, eof)?.is_some() {
		return Err(Error::Unrepairable("a box follows the moov").into());
	}
	Ok(())
}

/// Whether the file's trailer from `end` is a finish that listed chapters: the text, and a
/// `moov` that is `plain`, the `moov` of a finish without chapters, apart from the chapters.
///
/// The chapters it lists are read from the `chpl` box, and the text track, its `mdat`, and the
/// reference to it must be what a finish writes for those chapters.
fn keeps_chapters<F: Read + Seek>(file: &mut F, end: u64, eof: u64, plain: &[u8]) -> Result<bool> {
	let Some(text) = Atom::read(file, end, eof)?.filter(|atom| atom.kind == *b"mdat") else {
		return Ok(false);
	};
	let Some(atom) = Atom::read(file, text.end, eof)?.filter(|atom| atom.kind == *b"moov" && atom.end == eof) else {
		return Ok(false);
	};
	// A damaged box can declare any size the file is long enough for, which is not worth
	// reading when a finish never writes that much.
	// A finish writes the text an 8-byte header, which its chunk offset counts.
	if text.body - text.start != 8
		|| text.end - text.body > CHAPTER_BYTES
		|| atom.end - atom.start > plain.len() as u64 + CHAPTER_BYTES
	{
		return Ok(false);
	}
	let Some(data) = read_at(file, atom.start, atom.end - atom.start, eof)? else {
		return Ok(false);
	};
	// A moov that does not decode, or nests more than the decoder is safe to read, is damage
	// for the finish to replace, not a failure.
	if fmp4::metadata::check_unbounded(&data).is_err() {
		return Ok(false);
	}
	let Ok(mut moov) = mp4_atom::Moov::decode(&mut std::io::Cursor::new(data.as_slice())) else {
		return Ok(false);
	};
	let track = moov
		.trak
		.iter()
		.rposition(|trak| trak.mdia.hdlr.handler == b"text".into());
	let (Some(track), Some(chpl)) = (track, moov.udta.take().and_then(|udta| udta.chpl)) else {
		return Ok(false);
	};
	let id = moov.trak[track].tkhd.track_id;

	// The chapters as a finish placed them, in movie ticks, which its 100 ns starts keep
	// exactly, as a finish places each on the first tick of its 100 ns.
	let movie = u128::from(moov.mvhd.timescale);
	if movie == 0 || chpl.entries.is_empty() || chpl.entries.len() > CHPL_LIMIT {
		return Ok(false);
	}
	let mut placed = Vec::with_capacity(chpl.entries.len());
	for entry in &chpl.entries {
		let ticks = (u128::from(entry.start) * movie).div_ceil(10_000_000);
		match u64::try_from(ticks) {
			Ok(start)
				if start < moov.mvhd.duration
					&& u128::from(start) * 10_000_000 / movie == u128::from(entry.start)
					&& placed.last().is_none_or(|&(last, _)| last <= start) =>
			{
				placed.push((start, entry.title.as_str()));
			}
			_ => return Ok(false),
		}
	}
	let Ok((expected, body)) = chapter_track(id, moov.mvhd.timescale, moov.mvhd.duration, &placed, end) else {
		return Ok(false);
	};
	let Some(stored) = read_at(file, text.body, text.end - text.body, eof)? else {
		return Ok(false);
	};
	if moov.trak[track] != expected || stored != body {
		return Ok(false);
	}

	// The first enabled track references the text, besides whatever references it had.
	moov.trak.remove(track);
	let Some(carrier) = chap_carrier(&moov.trak) else {
		return Ok(false);
	};
	let removed = moov.trak[carrier].tref.as_mut().and_then(|tref| {
		let boxes = &mut tref.track_reference_type_boxes;
		let at = boxes
			.iter()
			.rposition(|kind| kind.reference_type == b"chap".into() && kind.track_ids == [id])?;
		boxes.remove(at);
		Some(boxes.is_empty())
	});
	match removed {
		None => return Ok(false),
		Some(true) => moov.trak[carrier].tref = None,
		Some(false) => {}
	}
	// The text track took the next id, so the finish advertised the one after it.
	let Some(next) = moov
		.trak
		.iter()
		.map(|trak| trak.tkhd.track_id)
		.max()
		.unwrap_or(0)
		.checked_add(1)
	else {
		return Ok(false);
	};
	if id != next || moov.mvhd.next_track_id != id.wrapping_add(1) {
		return Ok(false);
	}
	moov.mvhd.next_track_id = next;
	let mut buf = Vec::new();
	moov.encode(&mut buf).map_err(crate::Error::mp4)?;
	Ok(buf == plain)
}

/// The `len` bytes at `start`, or `None` when they do not end by `limit`.
fn read_at<F: Read + Seek>(file: &mut F, start: u64, len: u64, limit: u64) -> Result<Option<Vec<u8>>> {
	if start.checked_add(len).is_none_or(|end| end > limit) {
		return Ok(None);
	}
	let mut data = vec![0u8; len as usize];
	file.seek(SeekFrom::Start(start))?;
	file.read_exact(&mut data)?;
	Ok(Some(data))
}

/// One box in a file.
struct Atom {
	kind: [u8; 4],
	/// Where the box starts.
	start: u64,
	/// Where its body starts, past the header.
	body: u64,
	/// Where the box ends.
	end: u64,
}

impl Atom {
	/// The box at `start`, or `None` when it does not end by `limit`: cut short, or not a box.
	fn read<F: Read + Seek>(file: &mut F, start: u64, limit: u64) -> Result<Option<Self>> {
		let mut header = [0u8; 16];
		let available = limit.saturating_sub(start).min(16) as usize;
		if available < 8 {
			return Ok(None);
		}
		file.seek(SeekFrom::Start(start))?;
		file.read_exact(&mut header[..available])?;
		let kind = header[4..8].try_into().unwrap();
		let (length, size) = match u32::from_be_bytes(header[..4].try_into().unwrap()) {
			1 if available == 16 => (16, u64::from_be_bytes(header[8..].try_into().unwrap())),
			// A crash can leave zeroes, and a recording never writes a box that runs to the end.
			0 | 1 => return Ok(None),
			size => (8, u64::from(size)),
		};
		let end = start.checked_add(size).filter(|&end| size >= length && end <= limit);
		Ok(end.map(|end| Self {
			kind,
			start,
			body: start + length,
			end,
		}))
	}

	/// Read and decode the whole box.
	fn decode<F: Read + Seek, T: Decode>(&self, file: &mut F) -> Result<T> {
		let mut data = vec![0u8; (self.end - self.start) as usize];
		file.seek(SeekFrom::Start(self.start))?;
		file.read_exact(&mut data)?;
		T::decode(&mut std::io::Cursor::new(data.as_slice())).map_err(crate::Error::mp4)
	}
}

/// What a recording's file holds: its header, each track's latest description, and the
/// samples of every fragment.
#[derive(Clone)]
struct Index {
	/// Where the reserved box starts.
	reserved: u64,
	/// The movie timescale.
	timescale: u32,
	/// The tracks the fragmented `moov` declares. Any other track joined later.
	declared: HashSet<u32>,
	/// The latest description of each track, by track id.
	descriptions: HashMap<u32, Description>,
	/// What each track wrote, by track id.
	tracks: BTreeMap<u32, Track>,
}

/// How a track is declared.
#[derive(Clone)]
struct Description {
	trak: mp4_atom::Trak,
	trex: mp4_atom::Trex,
}

impl Description {
	fn new(trak: &mp4_atom::Trak, trex: &mp4_atom::Trex) -> Self {
		Self {
			trak: trak.clone(),
			trex: trex.clone(),
		}
	}

	/// Write a `free` box holding a `moov` that declares the track, returning that `moov` as
	/// read back from the file.
	fn encode(&self, timescale: u32, data: &mut BytesMut) -> Result<mp4_atom::Moov> {
		let moov = mp4_atom::Moov {
			mvhd: mp4_atom::Mvhd {
				timescale,
				..Default::default()
			},
			trak: vec![self.trak.clone()],
			mvex: Some(mp4_atom::Mvex {
				trex: vec![self.trex.clone()],
				..Default::default()
			}),
			..Default::default()
		};
		let mut buf = Vec::new();
		moov.encode(&mut buf).map_err(crate::Error::mp4)?;
		mp4_atom::Header {
			kind: b"free".into(),
			size: Some(buf.len()),
		}
		.encode(data)
		.map_err(crate::Error::mp4)?;
		data.put_slice(&buf);
		mp4_atom::Moov::decode(&mut std::io::Cursor::new(buf.as_slice())).map_err(crate::Error::mp4)
	}
}

/// What one track wrote, in decode order.
#[derive(Clone, Default)]
struct Track {
	/// One chunk per `trun`, its samples contiguous in the file.
	chunks: Vec<Run>,
	samples: Vec<Sample>,
}

#[derive(Clone)]
struct Run {
	/// Where the first sample starts in the file.
	offset: u64,
	/// The first sample's decode time, in media ticks.
	decode_time: u64,
	/// How many samples the run holds.
	count: u32,
}

#[derive(Clone)]
struct Sample {
	size: u32,
	duration: u32,
	cts: i64,
	sync: bool,
}

impl Index {
	/// Index a file whose reserved box starts at `reserved`, followed by the fragmented `moov`.
	fn new(reserved: u64, moov: &mp4_atom::Moov) -> Result<Self> {
		let mut index = Self {
			reserved,
			timescale: moov.mvhd.timescale,
			declared: moov.trak.iter().map(|trak| trak.tkhd.track_id).collect(),
			descriptions: HashMap::new(),
			tracks: BTreeMap::new(),
		};
		index.describe(moov)?;
		Ok(index)
	}

	/// Take each track `moov` declares as its description from here on.
	fn describe(&mut self, moov: &mp4_atom::Moov) -> Result<()> {
		let trexs = moov.mvex.as_ref().map_or(&[][..], |mvex| mvex.trex.as_slice());
		for trak in &moov.trak {
			let id = trak.tkhd.track_id;
			let Some(trex) = trexs.iter().find(|trex| trex.track_id == id) else {
				return Err(unindexable(id, "the track has no trex").into());
			};
			self.descriptions.insert(id, Description::new(trak, trex));
		}
		Ok(())
	}

	/// Record the samples of `moof`, which starts at `offset` in the file.
	fn fragment(&mut self, offset: u64, moof: &mp4_atom::Moof) -> Result<()> {
		for traf in &moof.traf {
			self.traf(offset, traf)?;
		}
		Ok(())
	}

	/// Record the samples of `traf`, whose moof starts at `moof` in the file.
	fn traf(&mut self, moof: u64, traf: &mp4_atom::Traf) -> Result<()> {
		let id = traf.tfhd.track_id;
		let Some(Description { trex, .. }) = self.descriptions.get(&id) else {
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
		for trun in traf.trun.iter().filter(|trun| !trun.samples.is_empty()) {
			let Some(offset) = trun.data_offset.and_then(|offset| u64::try_from(offset).ok()) else {
				return Err(unindexable(id, "a run has no data offset").into());
			};
			track.chunks.push(Run {
				offset: moof + offset,
				decode_time,
				count: trun.samples.len() as u32,
			});
			for entry in trun.samples.iter() {
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

	/// The writes that make the file a regular MP4 with its chapter text and `moov` at `end`.
	fn finish(&self, end: u64, chapters: &[Chapter]) -> Result<Finish> {
		let mut timelines = Vec::new();
		for (&id, track) in self.tracks.iter().filter(|(_, track)| !track.samples.is_empty()) {
			let Some(Description { trak, .. }) = self.descriptions.get(&id) else {
				return Err(unindexable(id, "the track has no declaration").into());
			};
			timelines.push((trak, track, Timeline::new(id, trak, track)?));
		}

		let movie = u128::from(self.timescale);
		let to_movie = |ticks: u64, timescale: u32| {
			u64::try_from(u128::from(ticks) * movie / u128::from(timescale)).map_err(|_| Error::PtsOverflow)
		};
		// The earliest track starts the movie; the rest start that much later.
		let mut origin = u64::MAX;
		for (trak, _, timeline) in &timelines {
			origin = origin.min(to_movie(timeline.start, trak.mdia.mdhd.timescale)?);
		}
		// Where each track starts at its own precision, which a movie tick can round away.
		let starts: Vec<(u64, u32)> = timelines
			.iter()
			.map(|(trak, _, timeline)| (timeline.start, trak.mdia.mdhd.timescale))
			.collect();

		let mut traks = Vec::new();
		for (trak, track, timeline) in timelines {
			let timescale = trak.mdia.mdhd.timescale;
			let delay = to_movie(timeline.start, timescale)? - origin;
			let presented = to_movie(timeline.duration, timescale)?;
			let mut trak = trak.clone();
			trak.mdia.mdhd.duration = timeline.deltas.iter().map(|&delta| u64::from(delta)).sum();
			trak.tkhd.duration = delay + presented;
			trak.edts = edits(delay, timeline.media_time, presented);
			trak.mdia.minf.stbl = sample_table(&trak.mdia.minf.stbl.stsd, track, &timeline);
			traks.push(trak);
		}
		traks.sort_by_key(|trak| (trak.mdia.hdlr.handler != b"vide".into(), trak.tkhd.track_id));

		let duration = traks.iter().map(|trak| trak.tkhd.duration).max().unwrap_or(0);
		let last_track_id = traks.iter().map(|trak| trak.tkhd.track_id).max().unwrap_or(0);
		let mut next_track_id = last_track_id
			.checked_add(1)
			.ok_or_else(|| unindexable(last_track_id, "no track id follows it"))?;

		// The chapter text lies in an mdat of its own, right where the moov would start.
		let mut tail = BytesMut::new();
		let mut udta = None;
		let chapters = self.place(chapters, &starts, origin, duration)?;
		if !chapters.is_empty() {
			let id = next_track_id;
			next_track_id = id
				.checked_add(1)
				.ok_or_else(|| unindexable(id, "no track id follows it"))?;
			let (trak, text) = chapter_track(id, self.timescale, duration, &chapters, end)?;
			mp4_atom::Header {
				kind: b"mdat".into(),
				size: Some(text.len()),
			}
			.encode(&mut tail)
			.map_err(crate::Error::mp4)?;
			tail.put_slice(&text);

			let movie = u128::from(self.timescale);
			udta = Some(mp4_atom::Udta {
				chpl: Some(mp4_atom::Chpl {
					entries: chapters
						.iter()
						.map(|&(start, title)| {
							// The list counts 100 ns units.
							let start = u128::from(start) * 10_000_000 / movie;
							Ok(mp4_atom::ChplEntry {
								start: u64::try_from(start).map_err(|_| Error::PtsOverflow)?,
								title: title.to_string(),
							})
						})
						.collect::<Result<_>>()?,
				}),
				..Default::default()
			});
			// The first enabled track, a video one if there is any, points at the text, besides
			// any reference it already has.
			if let Some(carrier) = chap_carrier(&traks) {
				traks[carrier]
					.tref
					.get_or_insert_with(Default::default)
					.track_reference_type_boxes
					.push(mp4_atom::TrackReferenceTypeBox {
						reference_type: b"chap".into(),
						track_ids: vec![id],
					});
			}
			traks.push(trak);
		}

		let moov = mp4_atom::Moov {
			mvhd: mp4_atom::Mvhd {
				timescale: self.timescale,
				duration,
				// Normal playback rate and full volume; both default to 0.
				rate: mp4_atom::FixedPoint::from(1),
				volume: mp4_atom::FixedPoint::from(1),
				next_track_id,
				..Default::default()
			},
			trak: traks,
			udta,
			..Default::default()
		};
		moov.encode(&mut tail).map_err(crate::Error::mp4)?;

		// Everything from the reserved box to the moov, the fragmented moov and every moof
		// included, becomes one mdat that a reader skips through to the moov.
		let mut mdat = BytesMut::with_capacity(RESERVED as usize);
		mdat.put_u32(1);
		mdat.put_slice(b"mdat");
		mdat.put_u64(end - self.reserved);

		Ok(Finish {
			moov_offset: end,
			tail: tail.freeze(),
			mdat_offset: self.reserved,
			mdat: mdat.freeze(),
			reopen: false,
			kept: false,
		})
	}
}

impl Index {
	/// Each chapter that falls within the movie, sorted, as its start in movie ticks from the
	/// movie's start at `origin` and its title. `starts` is where each track starts, as media
	/// ticks and the track's timescale, which the movie's ticks round.
	fn place<'a>(
		&self,
		chapters: &'a [Chapter],
		starts: &[(u64, u32)],
		origin: u64,
		duration: u64,
	) -> Result<Vec<(u64, &'a str)>> {
		let movie = moq_net::Timescale::new(u64::from(self.timescale))?;
		let mut placed = Vec::new();
		for chapter in chapters {
			// A chapter before the first sample has nothing to start on: compare it where the
			// tracks start, not in movie ticks, where it can round onto the start.
			let mut started = false;
			for &(start, timescale) in starts {
				if u128::from(start) <= chapter.time.as_scale(moq_net::Timescale::new(u64::from(timescale))?)? {
					started = true;
					break;
				}
			}
			// Nor does one after the last, compared before narrowing to a tick count.
			let ticks = chapter.time.as_scale(movie)?;
			let start = ticks.saturating_sub(u128::from(origin));
			if started && start < u128::from(duration) {
				placed.push((snap(start, self.timescale) as u64, chapter.title.as_str()));
			}
		}
		// A list a player reads in order, equal starts keeping the order given.
		placed.sort_by_key(|&(start, _)| start);
		if placed.len() > CHPL_LIMIT {
			return Err(Error::TooManyChapters(placed.len()).into());
		}
		if let Some((_, title)) = placed.iter().find(|(_, title)| title.len() > CHPL_LIMIT) {
			return Err(Error::ChapterTitleTooLong(title.len()).into());
		}
		Ok(placed)
	}
}

/// A chapter's start in movie ticks moved back to the first tick in the same 100 ns, the
/// resolution of a `chpl` entry, which a repair reads back to place the chapter. A movie
/// timescale of 10 MHz or less has no two ticks in one 100 ns, so nothing moves.
fn snap(ticks: u128, timescale: u32) -> u128 {
	let movie = u128::from(timescale);
	(ticks * 10_000_000 / movie * movie).div_ceil(10_000_000)
}

/// The QuickTime chapter track, whose samples are the titles in the order given, and the
/// text they lie in, which starts at `offset` in the file after the `mdat` header.
///
/// A chapter lasts until the next starts, the last until the movie ends, and the first
/// starts after an empty edit if it does not start the movie. Each start is in movie ticks.
fn chapter_track(
	id: u32,
	movie: u32,
	duration: u64,
	chapters: &[(u64, &str)],
	offset: u64,
) -> Result<(mp4_atom::Trak, Vec<u8>)> {
	let first = chapters.first().map_or(0, |&(start, _)| start);
	let mut text = Vec::new();
	let mut sizes = Vec::new();
	for &(_, title) in chapters {
		let before = text.len();
		// A 16-bit length and the title, then `encd` declaring it UTF-8 as ffmpeg writes it.
		text.extend_from_slice(&(title.len() as u16).to_be_bytes());
		text.extend_from_slice(title.as_bytes());
		text.extend_from_slice(&[0, 0, 0, 12, b'e', b'n', b'c', b'd', 0, 0, 1, 0]);
		sizes.push((text.len() - before) as u32);
	}

	// The movie's own ticks while every duration fits 31 bits, which keeps the
	// samples exact against the leading edit. A longer recording, hours at 90 kHz, counts
	// milliseconds, and one with a chapter over 24 days the next coarser timescale, rounding
	// each chapter's start. The edits stay in the movie's ticks.
	let (timescale, deltas, length) = std::iter::once(movie)
		.chain(TEXT_TIMESCALES)
		.find_map(|timescale| {
			let scale = |ticks: u64| u64::try_from(u128::from(ticks) * u128::from(timescale) / u128::from(movie)).ok();
			let deltas = chapters
				.iter()
				.enumerate()
				.map(|(i, &(start, _))| {
					let end = chapters.get(i + 1).map_or(duration, |&(next, _)| next);
					// A delta past 31 bits is clipped by readers such as FFmpeg's.
					i32::try_from(scale(end)?.checked_sub(scale(start)?)?)
						.ok()
						.map(i32::unsigned_abs)
				})
				.collect::<Option<Vec<u32>>>()?;
			Some((timescale, deltas, scale(duration)?.checked_sub(scale(first)?)?))
		})
		.ok_or(Error::PtsOverflow)?;

	let mut stts: Vec<mp4_atom::SttsEntry> = Vec::new();
	for delta in deltas {
		match stts.last_mut() {
			Some(last) if last.sample_delta == delta => last.sample_count += 1,
			_ => stts.push(mp4_atom::SttsEntry {
				sample_count: 1,
				sample_delta: delta,
			}),
		}
	}

	let trak = mp4_atom::Trak {
		tkhd: mp4_atom::Tkhd {
			track_id: id,
			// Text a player lists rather than shows, so it is not enabled.
			enabled: false,
			in_movie: true,
			duration,
			..Default::default()
		},
		edts: edits(first, 0, duration - first),
		mdia: mp4_atom::Mdia {
			mdhd: mp4_atom::Mdhd {
				timescale,
				duration: length,
				// Undetermined, which a zero decodes as three backticks instead.
				language: "und".into(),
				..Default::default()
			},
			hdlr: mp4_atom::Hdlr {
				handler: b"text".into(),
				name: String::new(),
			},
			minf: mp4_atom::Minf {
				nmhd: Some(mp4_atom::Nmhd {}),
				dinf: mp4_atom::Dinf {
					dref: mp4_atom::Dref {
						entries: vec![mp4_atom::DrefEntry::Url(mp4_atom::Url::default())],
					},
				},
				stbl: mp4_atom::Stbl {
					stsd: mp4_atom::Stsd {
						codecs: vec![
							mp4_atom::Tx3g {
								data_reference_index: 1,
								..Default::default()
							}
							.into(),
						],
					},
					stts: mp4_atom::Stts { entries: stts },
					stsc: mp4_atom::Stsc {
						entries: vec![mp4_atom::StscEntry {
							first_chunk: 1,
							samples_per_chunk: chapters.len() as u32,
							sample_description_index: 1,
						}],
					},
					stsz: mp4_atom::Stsz {
						samples: mp4_atom::StszSamples::Different { sizes },
					},
					// The mdat's header is 8 bytes.
					co64: Some(mp4_atom::Co64 {
						entries: vec![offset + 8],
					}),
					..Default::default()
				},
				..Default::default()
			},
		},
		..Default::default()
	};
	Ok((trak, text))
}

fn unindexable(track: u32, reason: &'static str) -> Error {
	Error::Unindexable { track, reason }
}

/// A track's samples laid on one media timeline.
struct Timeline {
	/// Each sample's `stts` delta: up to the next sample's decode time, which stretches a
	/// sample over any gap after it, and the last sample's own duration.
	deltas: Vec<u32>,
	/// Each sample's composition offset from its decode time on this timeline.
	offsets: Vec<i32>,
	/// Where presentation starts on the media timeline, past the first decode time.
	media_time: u64,
	/// The Hang time presentation starts at, in media ticks, which places the track in the
	/// movie. An Opus track's media runs one pre-skip ahead of it.
	start: u64,
	/// How long the track presents, in media ticks.
	duration: u64,
}

impl Timeline {
	fn new(id: u32, trak: &mp4_atom::Trak, track: &Track) -> Result<Self> {
		// Each fragment's decode time is its first sample's presentation time, so fragments
		// shorter than a GOP of reordered video step back and forth. Decode on one clock that
		// a sync sample moves ahead to its fragment's time, which keeps a real gap, and keep
		// every presentation time by recomputing the composition offsets against it.
		let mut decode_times: Vec<u64> = Vec::with_capacity(track.samples.len());
		let mut presentations: Vec<i128> = Vec::with_capacity(track.samples.len());
		let mut clock: Option<u64> = None;
		let mut samples = track.samples.iter();
		for run in &track.chunks {
			let mut written = run.decode_time;
			for sample in samples.by_ref().take(run.count as usize) {
				presentations.push(i128::from(written) + i128::from(sample.cts));
				let time = match clock {
					Some(clock) if sample.sync => clock.max(written),
					Some(clock) => clock,
					None => written,
				};
				decode_times.push(time);
				clock = Some(time.checked_add(u64::from(sample.duration)).ok_or(Error::PtsOverflow)?);
				written += u64::from(sample.duration);
			}
		}

		let mut deltas = Vec::with_capacity(decode_times.len());
		let mut offsets = Vec::with_capacity(decode_times.len());
		for (i, &time) in decode_times.iter().enumerate() {
			let delta = match decode_times.get(i + 1) {
				Some(&next) => {
					u32::try_from(next - time).map_err(|_| unindexable(id, "a gap overflows a sample delta"))?
				}
				None => track.samples[i].duration,
			};
			deltas.push(delta);
			let offset = presentations[i] - i128::from(time);
			offsets.push(i32::try_from(offset).map_err(|_| unindexable(id, "a composition offset overflows 32 bits"))?);
		}

		let Some(&first) = decode_times.first() else {
			return Ok(Self {
				deltas,
				offsets,
				media_time: 0,
				start: 0,
				duration: 0,
			});
		};
		let mut presented = i128::MAX;
		let mut end = i128::MIN;
		for (&pts, sample) in presentations.iter().zip(&track.samples) {
			presented = presented.min(pts);
			end = end.max(pts + i128::from(sample.duration));
		}
		// An Opus track presents one pre-skip into its first packet, and nothing is presented
		// before the first decode time.
		let priming = fmp4::priming(trak);
		let media = (presented + i128::from(priming)).max(i128::from(first));
		let media = u64::try_from(media).map_err(|_| Error::PtsOverflow)?;
		let duration = u64::try_from((end - i128::from(media)).max(0)).map_err(|_| Error::PtsOverflow)?;
		Ok(Self {
			deltas,
			offsets,
			media_time: media - first,
			start: media.checked_sub(priming).ok_or(Error::PtsOverflow)?,
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
fn sample_table(stsd: &mp4_atom::Stsd, track: &Track, timeline: &Timeline) -> mp4_atom::Stbl {
	let mut stts: Vec<mp4_atom::SttsEntry> = Vec::new();
	for &delta in &timeline.deltas {
		match stts.last_mut() {
			Some(last) if last.sample_delta == delta => last.sample_count += 1,
			_ => stts.push(mp4_atom::SttsEntry {
				sample_count: 1,
				sample_delta: delta,
			}),
		}
	}

	let mut ctts: Vec<mp4_atom::CttsEntry> = Vec::new();
	for &offset in &timeline.offsets {
		let offset = i64::from(offset);
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
	#[tokio::test]
	async fn received_metadata_is_bounded() {
		use crate::container::fmp4::metadata::tests::{attacks, measure};
		use mp4_atom::Encode;
		let live = Live::avc3();
		let mut export = export(&live).await;
		let mut bounded = true;
		for (name, data) in attacks(b"moov") {
			let mut init = Vec::new();
			mp4_atom::Ftyp {
				major_brand: b"isom".into(),
				minor_version: 0,
				compatible_brands: vec![],
			}
			.encode(&mut init)
			.unwrap();
			init.extend_from_slice(&data);
			let init = bytes::Bytes::from(init);
			bounded &= measure(name, || export.init(&init).is_err());
		}
		export.index = Some(super::Index::new(0, &mp4_atom::Moov::default()).unwrap());
		for (name, data) in attacks(b"moof") {
			bounded &= measure(name, || export.fragment(data.clone()).is_err());
		}
		assert!(bounded);
	}

	use std::time::Duration;

	use mp4_atom::DecodeMaybe;

	use crate::container::test_util::{Live, raw_frame, video_frame};

	/// Every retained group, since these tests publish before they record.
	const MAX_DELAY: Duration = Duration::from_secs(30);

	/// A 20 ms Opus packet (TOC config 1, one frame).
	const OPUS: &[u8] = &[0x08, 0xaa, 0xbb, 0xcc];

	type Producer = crate::container::Producer<crate::catalog::hang::Container>;

	/// A 48 kHz Opus rendition whose OpusHead declares `pre_skip`.
	fn add_opus(live: &mut Live, pre_skip: u16) -> Producer {
		use hang::catalog::{AudioCodec, AudioConfig, Container};

		live.add_track(".opus", |catalog, name| {
			let mut config = AudioConfig::new(AudioCodec::Opus, 48_000, 2);
			config.container = Container::Legacy;
			config.description = Some(
				crate::codec::opus::Config::new(48_000, 2)
					.with_pre_skip(pre_skip)
					.encode()
					.unwrap(),
			);
			catalog
				.modify(catalog.next_timestamp())
				.unwrap()
				.audio
				.renditions
				.insert(name, config);
		})
	}

	async fn export(live: &Live) -> super::Export<crate::catalog::Consumer> {
		super::Export::new(live.source().await, live.catalog_stream().await)
			.with_delay(crate::container::Delay::default().with_max(MAX_DELAY))
	}

	/// Append everything the export writes without waiting for more input.
	async fn record(export: &mut super::Export<crate::catalog::Consumer>, file: &mut Vec<u8>) {
		while let Ok(next) = kio::clock::timeout(Duration::from_millis(1), export.next()).await {
			match next.expect("export failed") {
				Some(data) => file.extend_from_slice(&data),
				None => break,
			}
		}
	}

	/// Append everything the export writes like [`record`], keeping the file's length and what
	/// the export indexed after each write.
	async fn record_indexed(
		export: &mut super::Export<crate::catalog::Consumer>,
		file: &mut Vec<u8>,
		indexes: &mut Vec<(usize, super::Index)>,
	) {
		while let Ok(next) = kio::clock::timeout(Duration::from_millis(1), export.next()).await {
			match next.expect("export failed") {
				Some(data) => file.extend_from_slice(&data),
				None => break,
			}
			indexes.push((file.len(), export.index.clone().expect("the header comes first")));
		}
	}

	/// Finish the recording in `file` the way a writer does.
	fn finish(export: super::Export<crate::catalog::Consumer>, file: &mut Vec<u8>) {
		apply(
			export.finish(&[]).expect("finish").expect("something was written"),
			file,
		);
	}

	/// Make the writes of `finish` to `file`.
	fn apply(finish: super::Finish, file: &mut Vec<u8>) {
		finish.write_to(file).unwrap();
	}

	impl super::Disk for Vec<u8> {
		fn set_len(&mut self, len: u64) -> std::io::Result<()> {
			self.resize(len as usize, 0);
			Ok(())
		}

		fn write_at(&mut self, offset: u64, data: &[u8]) -> std::io::Result<()> {
			let offset = offset as usize;
			if self.len() < offset + data.len() {
				self.resize(offset + data.len(), 0);
			}
			self[offset..offset + data.len()].copy_from_slice(data);
			Ok(())
		}

		fn sync(&mut self) -> std::io::Result<()> {
			Ok(())
		}
	}

	/// One call a finish made to its file.
	#[derive(Clone)]
	enum Op {
		SetLen(u64),
		Write(u64, Vec<u8>),
		Sync,
	}

	impl super::Disk for Vec<Op> {
		fn set_len(&mut self, len: u64) -> std::io::Result<()> {
			self.push(Op::SetLen(len));
			Ok(())
		}

		fn write_at(&mut self, offset: u64, data: &[u8]) -> std::io::Result<()> {
			self.push(Op::Write(offset, data.to_vec()));
			Ok(())
		}

		fn sync(&mut self) -> std::io::Result<()> {
			self.push(Op::Sync);
			Ok(())
		}
	}

	/// Every file a crash while `finish` writes to `file` can leave: what was synced, then each
	/// call since either lost or made, a write cut short within the box header it starts with
	/// or its last bytes. A reader sees any other cut as one of those.
	fn crashes(file: &[u8], finish: &super::Finish) -> Vec<Vec<u8>> {
		use super::Disk;

		let mut ops = Vec::new();
		finish.write_to(&mut ops).unwrap();
		let mut synced = file.to_vec();
		let mut pending: Vec<Op> = Vec::new();
		let mut files = vec![synced.clone()];
		for op in ops {
			if let Op::Sync = op {
				for op in pending.drain(..) {
					made(&mut synced, &op);
				}
				continue;
			}
			pending.push(op);
			let mut states = vec![synced.clone()];
			for op in &pending {
				let mut next = Vec::new();
				for state in states {
					next.push(state.clone());
					match op {
						Op::Write(offset, data) => {
							let header = 1..=data.len().min(16);
							for len in header.chain(data.len().saturating_sub(16).max(17)..=data.len()) {
								let mut state = state.clone();
								state.write_at(*offset, &data[..len]).unwrap();
								next.push(state);
							}
						}
						op => {
							let mut state = state;
							made(&mut state, op);
							next.push(state);
						}
					}
				}
				states = next;
			}
			files.extend(states);
		}
		files.push(synced);
		files
	}

	fn made(file: &mut Vec<u8>, op: &Op) {
		use super::Disk;
		match op {
			Op::SetLen(len) => super::Disk::set_len(file, *len).unwrap(),
			Op::Write(offset, data) => file.write_at(*offset, data).unwrap(),
			Op::Sync => {}
		}
	}

	/// The first `moov` a player reading the file's boxes in order reaches, or `None` when a box
	/// it can't read comes first.
	fn played(file: &[u8]) -> Option<&[u8]> {
		let mut position = 0;
		while let Some(atom) = super::Atom::read(&mut std::io::Cursor::new(file), position, file.len() as u64).unwrap()
		{
			if atom.kind == *b"moov" {
				return Some(&file[atom.start as usize..atom.end as usize]);
			}
			position = atom.end;
		}
		None
	}

	/// The `moov` a finish appends, after the chapter text if it wrote any.
	fn final_moov(finish: &super::Finish) -> &[u8] {
		let tail = &finish.tail[..];
		match &tail[4..8] {
			b"mdat" => &tail[u32::from_be_bytes(tail[..4].try_into().unwrap()) as usize..],
			_ => tail,
		}
	}

	fn repair(file: &[u8]) -> crate::Result<Option<super::Finish>> {
		super::repair(&mut std::io::Cursor::new(file), &[])
	}

	fn assert_same(finish: &super::Finish, expected: &super::Finish, cut: usize) {
		assert_eq!(finish.moov_offset, expected.moov_offset, "cut at {cut}");
		assert_eq!(finish.tail, expected.tail, "cut at {cut}");
		assert_eq!(finish.mdat_offset, expected.mdat_offset, "cut at {cut}");
		assert_eq!(finish.mdat, expected.mdat, "cut at {cut}");
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
		let mut audio = add_opus(&mut live, 312);
		let video: Vec<_> = (0..9u64).map(|i| video_frame(i * 33_000, i % 3 == 0)).collect();
		for frame in &video {
			live.track.write(frame.clone()).unwrap();
		}
		for i in 0..12u64 {
			audio.write(raw_frame(i * 20_000, OPUS, i % 5 == 0)).unwrap();
		}
		live.track.end_with(None).unwrap();
		let _finished = audio.finish().unwrap();

		let mut export = export(&live).await;
		let mut file = Vec::new();
		record(&mut export, &mut file).await;

		// Unfinished, it is the fMP4 a crash would leave. The fragmented moov already describes
		// every track as written, Opus pre-skip edit included.
		let unfinished = atoms(&file);
		assert_eq!(kinds(&unfinished[..3]), ["ftyp", "free", "moov"]);
		assert!(
			kinds(&unfinished[3..])
				.iter()
				.all(|kind| kind == "moof" || kind == "mdat"),
			"no description beyond the fragmented moov"
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

		let mut audio = add_opus(&mut live, 0);
		for i in 0..10u64 {
			audio.write(raw_frame(i * 20_000, OPUS, i % 5 == 0)).unwrap();
		}
		let _finished = audio.finish().unwrap();
		live.track.write(video_frame(200_000, true)).unwrap();
		live.track.end_with(None).unwrap();
		record(&mut export, &mut file).await;

		let unfinished = atoms(&file);
		let frees = unfinished
			.iter()
			.filter(|atom| matches!(atom, mp4_atom::Any::Free(_)))
			.count();
		assert_eq!(
			frees, 4,
			"the reserved box, the audio description and both audio fragments"
		);

		finish(export, &mut file);
		let moov = moov(atoms(&file));
		assert_eq!(moov.trak.len(), 2);
		assert_eq!(samples(&file, trak(&moov, b"soun")), vec![OPUS; 10]);
		assert_eq!(samples(&file, trak(&moov, b"vide")).len(), 2);
	}

	/// A late rendition that leaves without writing anything is forgotten, so cycling names
	/// holds nothing; one that wrote stays for the final moov.
	#[tokio::test(start_paused = true)]
	async fn departed_late_renditions_keep_only_what_they_wrote() {
		let mut live = Live::avc3();
		live.track.write(video_frame(0, true)).unwrap();
		let mut export = export(&live).await;
		let mut file = Vec::new();
		record(&mut export, &mut file).await;

		for _ in 0..3 {
			let audio = add_opus(&mut live, 0);
			record(&mut export, &mut file).await;
			let name = audio.name().to_string();
			live.catalog
				.modify(live.catalog.next_timestamp())
				.unwrap()
				.audio
				.renditions
				.remove(&name);
			record(&mut export, &mut file).await;
		}
		assert_eq!(export.fmp4.declarations().count(), 1, "only the video is held");

		let mut audio = add_opus(&mut live, 0);
		record(&mut export, &mut file).await;
		for i in 0..5u64 {
			audio.write(raw_frame(i * 20_000, OPUS, i == 0)).unwrap();
		}
		let audio = audio.finish().unwrap();
		record(&mut export, &mut file).await;
		let name = audio.name().to_string();
		live.catalog
			.modify(live.catalog.next_timestamp())
			.unwrap()
			.audio
			.renditions
			.remove(&name);
		live.track.write(video_frame(200_000, true)).unwrap();
		live.track.end_with(None).unwrap();
		record(&mut export, &mut file).await;
		assert_eq!(export.fmp4.declarations().count(), 2);

		finish(export, &mut file);
		let moov = moov(atoms(&file));
		assert_eq!(samples(&file, trak(&moov, b"soun")), vec![OPUS; 5]);
	}

	/// A track that starts later waits behind an empty edit, and a gap stretches the sample
	/// before it rather than pulling later samples early. Opus starts at its Hang time, its
	/// edit skipping the pre-skip in its media.
	#[tokio::test(start_paused = true)]
	async fn later_starts_and_gaps_keep_their_time() {
		let mut live = Live::avc3();
		let mut audio = add_opus(&mut live, 312);
		for i in 0..10u64 {
			audio.write(raw_frame(i * 20_000, OPUS, i % 5 == 0)).unwrap();
		}
		let _finished = audio.finish().unwrap();
		// Video starts 100 ms in, and its second GOP comes a second after the first.
		for (timestamp, keyframe) in [(100_000, true), (133_000, false), (1_133_000, true), (1_166_000, false)] {
			live.track.write(video_frame(timestamp, keyframe)).unwrap();
		}
		live.track.end_with(None).unwrap();

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

		let audio = trak(&moov, b"soun");
		let skip = 312 * u64::from(audio.mdia.mdhd.timescale) / 48_000;
		let edits = &audio.edts.as_ref().expect("edts").elst.as_ref().expect("elst").entries;
		assert_eq!(edits.len(), 1, "audio starts the movie");
		assert_eq!(edits[0].media_time, Some(skip));

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

	/// Fragments shorter than a GOP of reordered video each start their decode time at their
	/// own presentation time, which steps backwards; the index still decodes in order and
	/// presents every frame at its time.
	#[tokio::test(start_paused = true)]
	async fn reordered_video_in_short_fragments_keeps_its_times() {
		let mut live = Live::avc3();
		let presentations: [i64; 5] = [0, 120_000, 40_000, 80_000, 160_000];
		for (i, &timestamp) in presentations.iter().enumerate() {
			live.track.write(video_frame(timestamp as u64, i % 4 == 0)).unwrap();
		}
		live.track.end_with(None).unwrap();

		let mut export = export(&live).await.with_fragment_duration(Duration::ZERO);
		let mut file = Vec::new();
		record(&mut export, &mut file).await;
		finish(export, &mut file);
		let moov = moov(atoms(&file));
		let video = trak(&moov, b"vide");
		let stbl = &video.mdia.minf.stbl;

		let deltas = stbl
			.stts
			.entries
			.iter()
			.flat_map(|entry| std::iter::repeat_n(i64::from(entry.sample_delta), entry.sample_count as usize));
		let offsets: Vec<i64> = match &stbl.ctts {
			Some(ctts) => ctts
				.entries
				.iter()
				.flat_map(|entry| std::iter::repeat_n(entry.sample_offset, entry.sample_count as usize))
				.collect(),
			None => vec![0; presentations.len()],
		};
		let mut decode = 0;
		let mut presented = Vec::new();
		for (delta, offset) in deltas.zip(offsets) {
			assert!(delta > 0, "decode time advances");
			presented.push(decode + offset);
			decode += delta;
		}
		let timescale = i64::from(video.mdia.mdhd.timescale);
		let expected: Vec<i64> = presentations.iter().map(|&us| us * timescale / 1_000_000).collect();
		assert_eq!(presented, expected);
	}

	/// Stopping writes out the fragment each track was still buffering, so the finished file
	/// holds every frame read, and ends the export without waiting for more.
	#[tokio::test(start_paused = true)]
	async fn stopping_writes_the_buffered_fragments() {
		let mut live = Live::avc3();
		let mut audio = add_opus(&mut live, 0);
		let video: Vec<_> = (0..5u64).map(|i| video_frame(i * 33_000, i % 3 == 0)).collect();
		for frame in &video {
			live.track.write(frame.clone()).unwrap();
		}
		for i in 0..8u64 {
			audio.write(raw_frame(i * 20_000, OPUS, i % 5 == 0)).unwrap();
		}

		let mut export = export(&live).await;
		let mut file = Vec::new();
		record(&mut export, &mut file).await;
		let written = |export: &super::Export<_>| {
			export
				.index
				.as_ref()
				.unwrap()
				.tracks
				.values()
				.map(|track| track.samples.len())
				.sum::<usize>()
		};
		assert_eq!(written(&export), 3 + 5, "only the closed groups");

		export.stop();
		while let Some(data) = export.next().await.expect("export failed") {
			file.extend_from_slice(&data);
		}
		assert_eq!(written(&export), 5 + 8);

		finish(export, &mut file);
		let moov = moov(atoms(&file));
		let expected: Vec<Vec<u8>> = video.iter().map(length_prefixed).collect();
		assert_eq!(samples(&file, trak(&moov, b"vide")), expected);
		assert_eq!(samples(&file, trak(&moov, b"soun")), vec![OPUS; 8]);
	}

	/// A frame read but not yet placed in a fragment, as each track's first is when the header
	/// goes out, is written too.
	#[tokio::test(start_paused = true)]
	async fn stopping_writes_the_frames_read() {
		let mut live = Live::avc3();
		let mut audio = add_opus(&mut live, 0);
		let frame = video_frame(0, true);
		live.track.write(frame.clone()).unwrap();
		audio.write(raw_frame(0, OPUS, true)).unwrap();

		let mut export = export(&live).await;
		let mut file = export.next().await.unwrap().expect("the header").to_vec();
		export.stop();
		while let Some(data) = export.next().await.expect("export failed") {
			file.extend_from_slice(&data);
		}

		finish(export, &mut file);
		let moov = moov(atoms(&file));
		assert_eq!(samples(&file, trak(&moov, b"vide")), [length_prefixed(&frame)]);
		assert_eq!(samples(&file, trak(&moov, b"soun")), [OPUS]);
	}

	/// Stopping before every track can be described ends the export without a header, rather
	/// than failing on the media it could not write.
	#[tokio::test(start_paused = true)]
	async fn stopping_before_the_header_writes_nothing() {
		/// An IDR without the SPS a High profile track waits for.
		const IDR_ONLY: &[u8] = &[0, 0, 0, 1, 0x65, 0x88, 0x84, 0x21];

		let mut live = Live::avc3();
		let mut audio = add_opus(&mut live, 0);
		if let Some(config) = live
			.catalog
			.modify(live.catalog.next_timestamp())
			.unwrap()
			.video
			.renditions
			.values_mut()
			.next() && let hang::catalog::VideoCodec::H264(h264) = &mut config.codec
		{
			h264.profile = 110;
		}
		live.track.write(raw_frame(0, IDR_ONLY, true)).unwrap();
		for i in 0..10u64 {
			audio.write(raw_frame(i * 20_000, OPUS, i % 5 == 0)).unwrap();
		}

		let mut export = export(&live).await;
		let mut file = Vec::new();
		record(&mut export, &mut file).await;
		assert!(file.is_empty(), "the video holds the header back");

		export.stop();
		assert!(export.next().await.expect("stopping is not a failure").is_none());
		assert!(export.finish(&[]).unwrap().is_none());
	}

	/// Nothing to finish before the header is written.
	#[tokio::test(start_paused = true)]
	async fn nothing_written_needs_no_finish() {
		let live = Live::avc3();
		let export = export(&live).await;
		assert!(export.finish(&[]).unwrap().is_none());
	}

	/// A recording cut anywhere, mid-fragment or right after a late rendition joins, repairs
	/// to exactly what finishing it after its last complete write would have produced.
	#[tokio::test(start_paused = true)]
	async fn a_cut_recording_repairs_like_a_finish_there() {
		let mut live = Live::avc3();
		for i in 0..4u64 {
			live.track.write(video_frame(i * 33_000, i % 3 == 0)).unwrap();
		}
		let mut export = export(&live).await;
		let mut file = Vec::new();
		let mut indexes = Vec::new();
		record_indexed(&mut export, &mut file, &mut indexes).await;

		let mut audio = add_opus(&mut live, 312);
		for i in 0..10u64 {
			audio.write(raw_frame(100_000 + i * 20_000, OPUS, i % 5 == 0)).unwrap();
		}
		let _finished = audio.finish().unwrap();
		for i in 4..9u64 {
			live.track.write(video_frame(i * 33_000, i % 3 == 0)).unwrap();
		}
		live.track.end_with(None).unwrap();
		record_indexed(&mut export, &mut file, &mut indexes).await;
		assert!(indexes.len() > 4, "several writes to cut between");

		for cut in 0..=file.len() {
			let repaired = repair(&file[..cut]);
			let Some((end, index)) = indexes.iter().rev().find(|(end, _)| *end <= cut) else {
				assert!(repaired.is_err(), "a header cut at {cut} is no recording");
				continue;
			};
			let repaired = repaired.unwrap().expect("unfinished");
			assert_same(&repaired, &index.finish(*end as u64, &[]).unwrap(), cut);

			// A repair cut short after its moov leaves a file that repairs the same.
			let mut half = file[..cut].to_vec();
			half.truncate(repaired.moov_offset as usize);
			half.extend_from_slice(&repaired.tail);
			assert_same(&repair(&half).unwrap().expect("unfinished"), &repaired, cut);
		}

		// Uncut, the repair is the finish byte for byte, and indexes the late rendition.
		let mut repaired = file.clone();
		apply(repair(&file).unwrap().expect("unfinished"), &mut repaired);
		finish(export, &mut file);
		assert_eq!(repaired, file);
		let moov = moov(atoms(&file));
		assert_eq!(samples(&file, trak(&moov, b"soun")), vec![OPUS; 10]);
		assert_eq!(samples(&file, trak(&moov, b"vide")).len(), 9);
	}

	/// A recording with audio and video and nothing past its last fragment, and the bytes that
	/// finish it.
	async fn recorded(chapters: &[super::Chapter]) -> (Vec<u8>, super::Finish) {
		let mut live = Live::avc3();
		let mut audio = add_opus(&mut live, 0);
		for i in 0..6u64 {
			live.track.write(video_frame(i * 33_000, i % 3 == 0)).unwrap();
			audio.write(raw_frame(i * 20_000, OPUS, i == 0)).unwrap();
		}
		live.track.end_with(None).unwrap();
		let _finished = audio.finish().unwrap();
		let mut export = export(&live).await;
		let mut file = Vec::new();
		record(&mut export, &mut file).await;
		let finish = export.finish(chapters).unwrap().expect("something was written");
		(file, finish)
	}

	/// A crash anywhere in a finish, or in the repair of a recording cut mid-fragment or
	/// mid-description, or in the repair of any of those crashes, leaves a file a player still
	/// reads, through the fragmented moov or the final one, and that repairs to the same finish.
	#[tokio::test(start_paused = true)]
	async fn a_finish_interrupted_anywhere_plays_and_repairs() {
		interrupted(&[], ["ftyp", "mdat", "moov"]).await;
	}

	/// The same with chapters, whose text goes down in an `mdat` before the moov.
	#[tokio::test(start_paused = true)]
	async fn a_finish_with_chapters_interrupted_anywhere_plays_and_repairs() {
		interrupted(&chapters(), ["ftyp", "mdat", "mdat", "moov"]).await;
	}

	async fn interrupted<const N: usize>(chapters: &[super::Chapter], layout: [&str; N]) {
		// The repair of a recording lists the chapters its finish would.
		let repair = |file: &[u8]| super::repair(&mut std::io::Cursor::new(file), chapters);
		let (file, finish) = recorded(chapters).await;
		let mut finished = file.clone();
		apply(finish.clone(), &mut finished);
		let fragmented = played(&file).expect("the unfinished file plays").to_vec();

		let mut cut = file.clone();
		cut.truncate(file.len() - 3);
		let partial = repair(&cut).unwrap().expect("unfinished");
		let mut repaired = cut.clone();
		apply(partial.clone(), &mut repaired);
		assert_eq!(kinds(&atoms(&repaired)), layout);

		// Part of a late rendition's description, longer than the final moov.
		let mut described = file.clone();
		described.extend_from_slice(&[0, 0, 0x40, 0, b'f', b'r', b'e', b'e']);
		described.resize(described.len() + 4 * finish.tail.len(), 0);

		for (start, finish, done) in [
			(&file, &finish, &finished),
			(&described, &finish, &finished),
			(&cut, &partial, &repaired),
		] {
			for (i, crashed) in crashes(start, finish).iter().enumerate() {
				let moov = played(crashed).unwrap_or_else(|| panic!("crash {i} left a file no player reads"));
				assert!(
					moov == fragmented || moov == final_moov(finish),
					"crash {i} plays a broken moov"
				);
				let Some(again) = repair(crashed).unwrap_or_else(|err| panic!("crash {i}: {err}")) else {
					assert_eq!(crashed, done, "crash {i} reported finished");
					continue;
				};
				assert_same(&again, finish, i);
				// The repair can crash too, and that file still plays and repairs the same. Only
				// a reserved box no longer `free` makes it write what a finish does not.
				let reserved = finish.mdat_offset as usize;
				let reopened = crashed[reserved..reserved + 8] != super::FREE[..8];
				let twice = if reopened { crashes(crashed, &again) } else { Vec::new() };
				for (j, twice) in twice.iter().enumerate() {
					let moov = played(twice).unwrap_or_else(|| panic!("crash {i}, {j} left a file no player reads"));
					assert!(
						moov == fragmented || moov == final_moov(finish),
						"crash {i}, {j} plays a broken moov"
					);
					match repair(twice).unwrap_or_else(|err| panic!("crash {i}, {j}: {err}")) {
						Some(last) => assert_same(&last, finish, i),
						None => assert_eq!(twice, done, "crash {i}, {j} reported finished"),
					}
				}
				let mut fixed = crashed.clone();
				apply(again, &mut fixed);
				assert_eq!(&fixed, done, "crash {i}");
			}
		}
		assert_eq!(kinds(&atoms(&finished)), layout);
	}

	/// A finished file counts only when its header and trailing moov are exactly what its
	/// fragments index. A header torn in either write order, or a moov removed, cut, extended or
	/// changed, is finished again.
	#[tokio::test(start_paused = true)]
	async fn only_a_verified_index_counts_as_finished() {
		let (file, finish) = recorded(&[]).await;
		let mut finished = file.clone();
		apply(finish.clone(), &mut finished);
		assert!(repair(&finished).unwrap().is_none(), "already a regular MP4");

		let reserved = finish.mdat_offset as usize;
		let header = reserved..reserved + 16;
		let mut damaged = Vec::new();
		for len in 0..16 {
			// The header written first to last, as older recorders did, or last to first.
			let mut first = finished.clone();
			first[header.start + len..header.end].copy_from_slice(&super::FREE[len..]);
			let mut last = finished.clone();
			last[header.start..header.end - len].copy_from_slice(&super::FREE[..16 - len]);
			// Bytes the two headers share are written either way.
			damaged.extend([first, last].into_iter().filter(|torn| *torn != finished));
		}
		let moov = finish.moov_offset as usize;
		for len in [0, 1, 8, finish.tail.len() - 1] {
			let mut cut = finished.clone();
			cut.truncate(moov + len);
			damaged.push(cut);
		}
		let mut extended = finished.clone();
		extended.extend_from_slice(b"junk");
		damaged.push(extended);
		// A chunk offset changed, which still decodes.
		let mut changed = finished.clone();
		let last = changed.len() - 1;
		changed[last] ^= 1;
		damaged.push(changed);

		for (i, file) in damaged.iter().enumerate() {
			assert_same(&repair(file).unwrap().expect("not finished"), &finish, i);
		}

		// A reserved box that is neither is not one a recording wrote.
		let mut garbage = finished.clone();
		garbage[header.start + 5] = b'x';
		assert!(repair(&garbage).is_err());

		// A plain fMP4 has no reserved box for the mdat header.
		let fmp4 = [&file[..reserved], &file[reserved + 16..]].concat();
		assert!(repair(&fmp4).is_err());
	}

	fn chapter(millis: u64, title: &str) -> super::Chapter {
		super::Chapter {
			time: moq_net::Timestamp::from_millis(millis).unwrap(),
			title: title.to_string(),
		}
	}

	/// Chapters in the order a studio might mark them, the last one past the recording.
	fn chapters() -> Vec<super::Chapter> {
		vec![
			chapter(100, "Second"),
			chapter(0, "First \u{1f3ac}"),
			chapter(100, "Also second"),
			chapter(10_000, "Past the end"),
		]
	}

	/// Where a chapter starts in milliseconds, and its title.
	type Listed = Vec<(u64, String)>;

	/// The chapters a file lists in each form: its `chpl` box, then its text track.
	fn listed(file: &[u8], moov: &mp4_atom::Moov) -> (Listed, Listed) {
		let movie = u64::from(moov.mvhd.timescale);
		let chpl = moov.udta.as_ref().and_then(|udta| udta.chpl.as_ref()).expect("a chpl");
		let chpl = chpl
			.entries
			.iter()
			.map(|entry| (entry.start / 10_000, entry.title.clone()))
			.collect();

		let text = trak(moov, b"text");
		// Counted in the movie's ticks: the track's edits are, and its samples in its own.
		let scale = u64::from(text.mdia.mdhd.timescale);
		let mut start = 0;
		let edits = text
			.edts
			.as_ref()
			.and_then(|edts| edts.elst.as_ref())
			.map(|elst| elst.entries.as_slice());
		// An empty edit delays the first chapter.
		if let Some([delay, _]) = edits {
			start = delay.segment_duration;
		}
		let mut starts = Vec::new();
		for entry in &text.mdia.minf.stbl.stts.entries {
			for _ in 0..entry.sample_count {
				starts.push(start * 1000 / movie);
				start += u64::from(entry.sample_delta) * movie / scale;
			}
		}
		let track = samples(file, text)
			.iter()
			.zip(starts)
			.map(|(sample, start)| {
				let len = u16::from_be_bytes([sample[0], sample[1]]) as usize;
				(start, String::from_utf8(sample[2..2 + len].to_vec()).unwrap())
			})
			.collect();
		(chpl, track)
	}

	/// Both forms list the chapters that fall in the recording, in order, and the first track
	/// points at the text.
	#[tokio::test(start_paused = true)]
	async fn a_finish_lists_chapters_both_ways() {
		let (mut file, finish) = recorded(&chapters()).await;
		apply(finish, &mut file);
		let moov = moov(atoms(&file));
		let expected = vec![
			(0, "First \u{1f3ac}".to_string()),
			(100, "Second".to_string()),
			(100, "Also second".to_string()),
		];
		let (chpl, track) = listed(&file, &moov);
		assert_eq!(chpl, expected);
		assert_eq!(track, expected);

		let text = trak(&moov, b"text");
		assert!(!text.tkhd.enabled, "text a player lists is not shown");
		assert_eq!(moov.trak[0].mdia.hdlr.handler, b"vide".into());
		let reference = moov.trak[0].tref.as_ref().expect("the first track references the text");
		assert_eq!(reference.track_reference_type_boxes.len(), 1);
		assert_eq!(reference.track_reference_type_boxes[0].reference_type, b"chap".into());
		assert_eq!(reference.track_reference_type_boxes[0].track_ids, [text.tkhd.track_id]);
		assert_eq!(moov.mvhd.next_track_id, text.tkhd.track_id + 1);
		assert_eq!(text.tkhd.duration, moov.mvhd.duration);
	}

	/// A chapter before the first sample or after the last is dropped, not clamped, and a
	/// movie that starts late lists chapters from its own start.
	#[tokio::test(start_paused = true)]
	async fn chapters_are_placed_on_the_broadcast_timeline() {
		let mut live = Live::avc3();
		for i in 0..6u64 {
			live.track
				.write(video_frame(1_000_000 + i * 33_000, i % 3 == 0))
				.unwrap();
		}
		live.track.end_with(None).unwrap();
		let mut export = export(&live).await;
		let mut file = Vec::new();
		record(&mut export, &mut file).await;

		let chapters = [
			chapter(999, "Before"),
			chapter(1_000, "Start"),
			chapter(1_066, "Middle"),
			chapter(1_198, "After"),
		];
		apply(
			export.finish(&chapters).unwrap().expect("something was written"),
			&mut file,
		);
		let moov = moov(atoms(&file));
		let (chpl, track) = listed(&file, &moov);
		let expected = vec![(0, "Start".to_string()), (66, "Middle".to_string())];
		assert_eq!(chpl, expected);
		assert_eq!(track, expected);
	}

	/// A recording that no chapter falls in is the same file as one given none.
	#[tokio::test(start_paused = true)]
	async fn no_chapter_in_range_adds_nothing() {
		let (_, plain) = recorded(&[]).await;
		let (_, past) = recorded(&[chapter(60_000, "Past the end")]).await;
		assert_same(&past, &plain, 0);
	}

	/// What a chapter list's one-byte counts cannot hold is refused instead of cut.
	#[tokio::test(start_paused = true)]
	async fn a_chapter_list_over_its_limits_is_refused() {
		let mut live = Live::avc3();
		for i in 0..6u64 {
			live.track.write(video_frame(i * 33_000, i % 3 == 0)).unwrap();
		}
		let mut export = export(&live).await;
		let mut file = Vec::new();
		record(&mut export, &mut file).await;
		let index = export.index.expect("a header");
		let end = file.len() as u64;
		assert!(index.finish(end, &vec![chapter(0, "a"); 255]).is_ok());
		assert!(index.finish(end, &vec![chapter(0, "a"); 256]).is_err());
		assert!(index.finish(end, &[chapter(0, &"a".repeat(255))]).is_ok());
		assert!(index.finish(end, &[chapter(0, &"a".repeat(256))]).is_err());

		// Those the recording drops count toward neither, whatever their time.
		let past = chapter(60_000, "a");
		let mut many = vec![past.clone(); 300];
		many.push(chapter(0, "kept"));
		assert!(index.finish(end, &many).is_ok());
		assert!(index.finish(end, &[past.clone(), chapter(0, "kept")]).is_ok());
		let long = super::Chapter {
			title: "a".repeat(256),
			..past
		};
		assert!(index.finish(end, &[long]).is_ok());
		let far = super::Chapter {
			time: moq_net::Timestamp::from_secs(1 << 60).unwrap(),
			title: "far".to_string(),
		};
		assert!(index.finish(end, &[far]).is_ok());
	}

	/// A chapter at a movie timescale above 10 MHz starts on the first tick of its 100 ns,
	/// which is where a repair reads it back, and nothing moves at or below it.
	#[test]
	fn a_chapter_starts_on_the_first_tick_of_its_chpl_unit() {
		for movie in [1_000, 90_000, 10_000_000, 48_000_000, 1_000_000_000] {
			for ticks in [0u128, 1, 7, 99, 100, 12_345, 987_654_321] {
				let snapped = super::snap(ticks, movie);
				assert!(snapped <= ticks);
				assert_eq!(super::snap(snapped, movie), snapped);
				let chpl = |ticks: u128| ticks * 10_000_000 / u128::from(movie);
				assert_eq!(chpl(snapped), chpl(ticks));
				if movie <= 10_000_000 {
					assert_eq!(snapped, ticks);
				}
			}
		}
	}

	/// A chapter's duration in the text track holds for a recording of hours at the finest
	/// movie timescale.
	#[test]
	fn a_long_movie_keeps_its_chapters() {
		let hour = 3600 * 90_000;
		let (trak, _) = super::chapter_track(2, 90_000, 20 * hour, &[(0, "a"), (hour, "b")], 0).unwrap();
		assert_eq!(trak.mdia.mdhd.timescale, 1000);
		assert_eq!(trak.mdia.mdhd.duration, 20 * 3600 * 1000);
	}

	/// A short movie's text samples are in its own ticks, so a chapter that is not a whole
	/// millisecond lands where the leading edit and the previous sample put it.
	#[test]
	fn text_samples_are_exact_against_the_leading_edit() {
		// 0.9 ms and 1.1 ms at 90 kHz.
		let (trak, _) = super::chapter_track(2, 90_000, 9_000, &[(81, "a"), (99, "b")], 0).unwrap();
		assert_eq!(trak.mdia.mdhd.timescale, 90_000);
		let delay = trak.edts.unwrap().elst.unwrap().entries[0].segment_duration;
		assert_eq!(delay, 81);
		let deltas: Vec<_> = trak
			.mdia
			.minf
			.stbl
			.stts
			.entries
			.iter()
			.map(|entry| entry.sample_delta)
			.collect();
		assert_eq!(deltas, [18, 8901]);
	}

	/// A chapter span FFmpeg would clip, past 31 bits, takes the next timescale: 47,720 seconds
	/// is 4,294,800,000 ticks at 90 kHz, which fits 32 bits but not 31.
	#[test]
	fn a_chapter_delta_past_31_bits_takes_a_coarser_text_timescale() {
		let (trak, _) = super::chapter_track(2, 90_000, 47_720 * 90_000, &[(0, "a")], 0).unwrap();
		assert_eq!(trak.mdia.mdhd.timescale, 1000);
		let (trak, _) = super::chapter_track(2, 90_000, 20_000 * 90_000, &[(0, "a")], 0).unwrap();
		assert_eq!(trak.mdia.mdhd.timescale, 90_000);
	}

	/// A chapter over 24 days gets
	#[test]
	fn a_chapter_of_weeks_gets_a_coarser_text_timescale() {
		let day = 24 * 3600 * 90_000;
		let (trak, _) = super::chapter_track(2, 90_000, 50 * day, &[(0, "a")], 0).unwrap();
		assert_eq!(trak.mdia.mdhd.timescale, 100);
		assert_eq!(trak.mdia.mdhd.duration, 50 * 24 * 3600 * 100);
	}

	/// A repair given no chapters leaves a finished file's alone, and finishes one whose
	/// chapters are damaged.
	#[tokio::test(start_paused = true)]
	async fn repairing_without_chapters_keeps_a_finished_files() {
		let (mut file, finish) = recorded(&chapters()).await;
		apply(finish, &mut file);
		assert!(repair(&file).unwrap().is_none());

		// A chunk offset inside the moov changed, which still decodes.
		let mut changed = file.clone();
		let moov = changed.windows(4).rposition(|kind| kind == b"moov").expect("a moov");
		let co64 = moov
			+ changed[moov..]
				.windows(4)
				.position(|kind| kind == b"co64")
				.expect("a co64");
		changed[co64 + 19] ^= 1;
		assert!(repair(&changed).unwrap().is_some());

		// A moov that no longer decodes is finished again too.
		let mut broken = file.clone();
		let tkhd = moov
			+ broken[moov..]
				.windows(4)
				.position(|kind| kind == b"tkhd")
				.expect("a tkhd");
		broken[tkhd + 4] = 9;
		// `moov` is where the box's kind starts, past its 4-byte size.
		let decodes = |file: &[u8]| {
			<mp4_atom::Moov as mp4_atom::Decode>::decode(&mut std::io::Cursor::new(&file[moov - 4..])).is_ok()
		};
		assert!(decodes(&file));
		assert!(!decodes(&broken));
		assert!(repair(&broken).unwrap().is_some());
	}

	/// A trailer that lists chapters is kept only when its chapter text, its references, and
	/// its list agree, and what is not kept is finished again without them.
	#[tokio::test(start_paused = true)]
	async fn repairing_without_chapters_replaces_a_trailer_that_disagrees() {
		let (mut file, finish) = recorded(&chapters()).await;
		let text = finish.moov_offset as usize;
		apply(finish, &mut file);
		assert!(repair(&file).unwrap().is_none());
		let trailer = file.windows(4).rposition(|kind| kind == b"moov").expect("a moov");
		let chapterless = |file: &[u8]| {
			let mut file = file.to_vec();
			apply(repair(&file).unwrap().expect("finished again"), &mut file);
			let moov = super::tests::moov(atoms(&file));
			assert!(moov.udta.and_then(|udta| udta.chpl).is_none());
		};

		// A title in the text that `chpl` does not list.
		let mut changed = file.clone();
		let at = changed[text..trailer]
			.windows(5)
			.position(|title| title == b"First")
			.expect("a title");
		changed[text + at + 1] = b'x';
		chapterless(&changed);

		// A reference to a track that is not the text.
		let mut changed = file.clone();
		let chap = changed[trailer..]
			.windows(4)
			.position(|kind| kind == b"chap")
			.expect("a chap reference");
		changed[trailer + chap + 7] ^= 1;
		chapterless(&changed);

		// A trailer far larger than a finish writes is not read.
		let mut changed = file.clone();
		let size = changed.len() - trailer + 4;
		changed[trailer - 4..trailer].copy_from_slice(&((size + 1_000_000) as u32).to_be_bytes());
		changed.resize(changed.len() + 1_000_000, 0);
		chapterless(&changed);
	}

	/// A box a damaged moof left in the middle is not a chapter text: the fragments after
	/// it are not cut.
	#[tokio::test(start_paused = true)]
	async fn a_damaged_fragment_box_is_not_repaired_away() {
		let (mut file, _) = recorded(&chapters()).await;
		let moof = file.windows(4).position(|kind| kind == b"moof").expect("a moof");
		file[moof..moof + 4].copy_from_slice(b"mdat");
		assert!(repair(&file).is_err());

		// Nor is a box after a finish's moov, which a finish ends with.
		let (mut file, finish) = recorded(&chapters()).await;
		apply(finish, &mut file);
		file.extend_from_slice(&[0, 0, 0, 8, b'f', b'r', b'e', b'e']);
		assert!(repair(&file).is_err());
	}

	/// The text track takes the next id, so a trailer advertising an occupied one is finished
	/// again.
	#[tokio::test(start_paused = true)]
	async fn repairing_without_chapters_checks_the_next_track_id() {
		let (mut file, finish) = recorded(&chapters()).await;
		apply(finish, &mut file);
		let mvhd = file.windows(4).rposition(|kind| kind == b"mvhd").expect("an mvhd");
		// A version 1 mvhd's next track id follows 108 bytes of its body.
		let at = mvhd + 4 + 108;
		assert_eq!(&file[at..at + 4], &4u32.to_be_bytes());
		file[at + 3] = 3;
		assert!(repair(&file).unwrap().is_some());
	}

	/// The chapter reference goes on the first enabled track.
	#[test]
	fn the_chapter_reference_goes_on_an_enabled_track() {
		let track = |enabled| mp4_atom::Trak {
			tkhd: mp4_atom::Tkhd {
				enabled,
				..Default::default()
			},
			..Default::default()
		};
		assert_eq!(super::chap_carrier(&[]), None);
		assert_eq!(super::chap_carrier(&[track(true), track(true)]), Some(0));
		assert_eq!(super::chap_carrier(&[track(false), track(true)]), Some(1));
		assert_eq!(super::chap_carrier(&[track(false), track(false)]), Some(0));
	}

	/// A finish that crashed before its mdat header was complete still lists its chapters
	/// after a repair given none, which only completes the header.
	#[tokio::test(start_paused = true)]
	async fn repairing_without_chapters_keeps_them_when_only_the_header_is_unfinished() {
		let (mut file, finish) = recorded(&chapters()).await;
		let reserved = finish.mdat_offset as usize;
		let text = finish.moov_offset as usize;
		apply(finish, &mut file);
		let finished = file.clone();

		file[reserved..reserved + 8].copy_from_slice(&super::FREE[..8]);
		let repaired = repair(&file).unwrap().expect("unfinished");
		apply(repaired, &mut file);
		assert_eq!(file, finished);
		assert_eq!(&file[text + 4..text + 8], b"mdat");

		// The repair can crash too, and the file still lists its chapters once repaired.
		let mut torn = finished.clone();
		torn[reserved..reserved + 8].copy_from_slice(&super::FREE[..8]);
		let repaired = repair(&torn).unwrap().expect("unfinished");
		for (i, crashed) in crashes(&torn, &repaired).iter().enumerate() {
			let mut fixed = crashed.clone();
			if let Some(again) = repair(crashed).unwrap_or_else(|err| panic!("crash {i}: {err}")) {
				apply(again, &mut fixed);
			}
			assert_eq!(fixed, finished, "crash {i}");
		}
	}

	/// A repair given other chapters than a finished file lists finishes it again.
	#[tokio::test(start_paused = true)]
	async fn repairing_with_other_chapters_replaces_them() {
		let (mut file, finish) = recorded(&[]).await;
		apply(finish, &mut file);
		let chapters = chapters();
		let again = super::repair(&mut std::io::Cursor::new(file.as_slice()), &chapters)
			.unwrap()
			.expect("other chapters");
		apply(again, &mut file);

		let (mut expected, finish) = recorded(&chapters).await;
		apply(finish, &mut expected);
		assert_eq!(file, expected);
		assert!(
			super::repair(&mut std::io::Cursor::new(file.as_slice()), &chapters)
				.unwrap()
				.is_none()
		);
	}
}
