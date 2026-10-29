//! Import media into a moq broadcast.
//!
//! The importers split by multiplicity: [`Track`] / [`TrackStream`] publish a
//! single codec onto one MoQ track, while [`Container`] decodes a container
//! that may publish more than one track. A single codec splits again by frame
//! boundaries: [`Track`] takes whole frames (the typical case for files and
//! reassembled network input), while [`TrackStream`] infers them from a raw
//! byte stream (piped Annex-B H.264, ...). A container recovers its own
//! framing, so [`Container`] takes chunks split anywhere.
//!
//! Each importer takes a typed format (parsed from e.g. `"avc3"` or `"fmp4"`)
//! and errors on a format it doesn't handle; `TrackStream` accepts only the
//! self-delimiting formats. The concrete importers live with their format
//! under [`crate::container`] or [`crate::codec`] and own their
//! catalog rendition through their container producer.
//!
//! A single-codec importer takes a track the caller mints, typically
//! `broadcast.unique_track(suffix, catalog.track_info())`, so the retention the catalog
//! declares reaches the track.

mod container;
mod format;
mod init;
mod track;

pub use container::*;
pub use format::{AudioFormat, ContainerFormat, Format, Kind, VideoFormat};
pub use init::{AudioInit, ContainerInit, VideoInit};
pub use track::*;

/// A finished importer keeps its tracks and renditions published, but not its input storage.
///
/// Bindings keep finished importers until their broadcast ends, so a long-lived broadcast that
/// retires many sources would otherwise hold each one's largest input chunk or element. Each source
/// here buffers a [`PAD`]-sized element (an atom, a tag, a block, an access unit) or chunk, then the
/// test measures what dropping the finished importers frees while their tracks are still held.
///
/// The counting allocator below serves this crate's whole unit-test binary, but it counts only on a
/// thread inside [`released`], so other tests are unaffected.
#[cfg(test)]
mod tests {
	use std::alloc::{GlobalAlloc, Layout, System};
	use std::cell::Cell;

	use super::*;
	use bytes::Bytes;

	/// The element or chunk each source buffers, far above the bookkeeping a finished importer keeps.
	const PAD: usize = 1 << 20;

	/// Small enough that an element spans many writes, so the importer accumulates it.
	const CHUNK: usize = 64 * 1024;

	thread_local! {
		/// Bytes this thread freed minus those it allocated while measuring. Per thread so concurrent
		/// tests don't mix, and `const` so the allocator can read it without allocating.
		static RELEASED: Cell<Option<isize>> = const { Cell::new(None) };
	}

	fn count(bytes: isize) {
		// `try_with` because TLS is unavailable while a thread is being torn down.
		let _ = RELEASED.try_with(|released| released.set(released.get().map(|n| n + bytes)));
	}

	/// Counts on the measuring thread; every call forwards to the system allocator unchanged.
	struct Counting;

	unsafe impl GlobalAlloc for Counting {
		unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
			count(-(layout.size() as isize));
			unsafe { System.alloc(layout) }
		}

		unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
			count(layout.size() as isize);
			unsafe { System.dealloc(ptr, layout) }
		}

		unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
			count(layout.size() as isize - new_size as isize);
			unsafe { System.realloc(ptr, layout, new_size) }
		}
	}

	#[global_allocator]
	static ALLOCATOR: Counting = Counting;

	/// The heap `f` gives back, net of what it allocates on the way.
	fn released(f: impl FnOnce()) -> isize {
		RELEASED.with(|released| released.set(Some(0)));
		f();
		RELEASED.with(|released| released.replace(None).unwrap())
	}

	/// One source's importer, finished and kept the way a binding keeps it.
	type Finished = Box<dyn std::any::Any>;

	type Import<'a> = &'a dyn Fn(&moq_net::broadcast::Producer, &crate::catalog::Producer) -> Finished;

	/// Retire `sources` sources on one broadcast and return what dropping their finished importers
	/// frees while every published track is still held.
	fn retire(sources: usize, import: Import) -> isize {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		// No HLS records: retiring them frees their compressed groups' encoders, which the
		// catalog owns rather than the importer.
		let config = crate::catalog::Config::default().with_hls(None);
		let catalog = crate::catalog::Producer::new(&mut broadcast, config).unwrap();
		let consumer = broadcast.consume();

		let finished: Vec<Finished> = (0..sources).map(|_| import(&broadcast, &catalog)).collect();

		// Late readers keep the published media, so only what the importers own is measured.
		let snapshot = catalog.snapshot();
		let names: Vec<&String> = snapshot
			.video
			.renditions
			.keys()
			.chain(snapshot.audio.renditions.keys())
			.collect();
		assert!(names.len() >= sources, "a source published nothing: {names:?}");
		let _tracks: Vec<_> = names.iter().map(|name| consumer.track(name).unwrap()).collect();

		released(|| drop(finished))
	}

	#[track_caller]
	fn assert_released(name: &str, import: Import) {
		for sources in [1, 4] {
			let released = retire(sources, import);
			assert!(
				released < (sources * PAD / 4) as isize,
				"{name}: {sources} finished sources still owned {released} bytes"
			);
		}
	}

	/// An IDR access unit carrying its SPS and PPS, padded to [`PAD`] with filler data, Annex-B.
	fn h264_keyframe() -> Vec<u8> {
		let mut au = vec![0, 0, 0, 1];
		au.extend_from_slice(&[
			0x67, 0x64, 0x00, 0x1f, 0xac, 0x24, 0x84, 0x01, 0x40, 0x16, 0xec, 0x04, 0x40, 0x00, 0x00, 0x03, 0x00, 0x40,
			0x00, 0x00, 0x0c, 0x23, 0xc6, 0x0c, 0x92,
		]);
		au.extend_from_slice(&[0, 0, 0, 1, 0x68, 0xee, 0x32, 0xc8, 0xb0]);
		au.extend_from_slice(&[0, 0, 0, 1, 0x65, 0x88, 0x84, 0x21]);
		au.extend_from_slice(&[0, 0, 0, 1, 0x0c]);
		au.resize(au.len() + PAD, 0xff);
		au.push(0x80);
		au
	}

	fn avc3(broadcast: &moq_net::broadcast::Producer) -> moq_net::track::Request {
		broadcast.reserve_track(broadcast.unique_name(".avc3")).unwrap()
	}

	#[tokio::test(start_paused = true)]
	async fn finished_track_releases_its_splitter() {
		let keyframe = h264_keyframe();
		assert_released("avc3 track", &|broadcast, catalog| {
			let init = VideoInit::new(VideoFormat::Avc3, Bytes::new());
			let mut track = Track::video(avc3(broadcast), catalog.reserve(), init).unwrap();
			for pts in [0, 33_333] {
				let pts = moq_net::Timestamp::from_micros(pts).unwrap();
				track.decode(keyframe.clone(), Some(pts)).unwrap();
			}
			track.finish().unwrap();
			Box::new(track)
		});
	}

	#[tokio::test(start_paused = true)]
	async fn finished_track_stream_releases_its_splitter() {
		// Two access units in one write, so the splitter holds the whole write.
		let stream = [h264_keyframe(), h264_keyframe()].concat();
		assert_released("avc3 stream", &|broadcast, catalog| {
			let init = VideoInit::new(VideoFormat::Avc3, Bytes::new());
			let mut track = TrackStream::video(avc3(broadcast), catalog.reserve(), init).unwrap();
			track.decode(&stream).unwrap();
			track.finish().unwrap();
			Box::new(track)
		});
	}

	/// Import `data` as `format`, `chunk` bytes per write.
	fn container(
		format: ContainerFormat,
		data: &[u8],
		chunk: usize,
	) -> impl Fn(&moq_net::broadcast::Producer, &crate::catalog::Producer) -> Finished {
		move |broadcast, catalog| {
			let init = ContainerInit::new(format, Bytes::new());
			let mut import = Container::new(broadcast.clone(), catalog.reserve(), &init).unwrap();
			for chunk in data.chunks(chunk) {
				import.decode(chunk).unwrap();
			}
			import.finish().unwrap();
			Box::new(import)
		}
	}

	#[tokio::test(start_paused = true)]
	async fn finished_fmp4_releases_its_atom_buffer() {
		// The fixture ends on a moof without its mdat, so keep its first four whole fragments.
		let mut data = include_bytes!("../container/fmp4/test_data/bbb.mp4")[..2035].to_vec();
		// A free atom accumulates across writes before the parser skips it.
		data.extend_from_slice(&u32::try_from(8 + PAD).unwrap().to_be_bytes());
		data.extend_from_slice(b"free");
		data.resize(data.len() + PAD, 0);

		assert_released("fmp4", &container(ContainerFormat::Fmp4, &data, CHUNK));
		assert_released("fmp4 passthrough", &|broadcast, catalog| {
			let mut import =
				crate::container::fmp4::Import::new(broadcast.clone(), catalog.reserve()).with_passthrough(true);
			for chunk in data.chunks(CHUNK) {
				import.decode(chunk).unwrap();
			}
			import.finish().unwrap();
			Box::new(import)
		});
	}

	#[tokio::test(start_paused = true)]
	async fn finished_flv_releases_its_tag_buffer() {
		let mut data = include_bytes!("../container/ts/test_data/he_aac.flv").to_vec();
		// A script tag accumulates across writes before the parser discards it: type, 24-bit size,
		// timestamp and stream ID, body, then the trailing previous-tag size.
		let size = u32::try_from(PAD).unwrap();
		data.push(18);
		data.extend_from_slice(&size.to_be_bytes()[1..]);
		data.extend_from_slice(&[0; 7]);
		data.resize(data.len() + PAD, 0);
		data.extend_from_slice(&(size + 11).to_be_bytes());

		assert_released("flv", &container(ContainerFormat::Flv, &data, CHUNK));
	}

	#[tokio::test(start_paused = true)]
	async fn finished_ts_releases_its_packet_scratch() {
		let mut data = include_bytes!("../container/ts/test_data/bbb.ts").to_vec();
		// Null packets in one write, so the packet scratch holds the whole write.
		let mut null = [0xff; 188];
		null[..4].copy_from_slice(&[0x47, 0x1f, 0xff, 0x10]);
		data.extend(null.iter().cycle().take(PAD / 188 * 188));

		assert_released("ts", &container(ContainerFormat::Ts, &data, data.len()));
	}

	#[tokio::test(start_paused = true)]
	async fn finished_mkv_releases_its_element_buffer() {
		use webm_iterable::WebmWriter;
		use webm_iterable::matroska_spec::{Master, MatroskaSpec, SimpleBlock};

		// The parser retains the last element it emitted, here a block of `PAD` bytes.
		let payload = vec![0; PAD];
		let tags = [
			MatroskaSpec::Ebml(Master::Full(vec![MatroskaSpec::DocType("webm".into())])),
			MatroskaSpec::Segment(Master::Start),
			MatroskaSpec::Info(Master::Full(vec![MatroskaSpec::TimestampScale(1_000_000)])),
			MatroskaSpec::Tracks(Master::Full(vec![MatroskaSpec::TrackEntry(Master::Full(vec![
				MatroskaSpec::TrackNumber(1),
				MatroskaSpec::TrackUID(1),
				MatroskaSpec::TrackType(1),
				MatroskaSpec::CodecID("V_VP9".into()),
				MatroskaSpec::Video(Master::Full(vec![
					MatroskaSpec::PixelWidth(320),
					MatroskaSpec::PixelHeight(240),
				])),
			]))])),
			MatroskaSpec::Cluster(Master::Start),
			MatroskaSpec::Timestamp(0),
			SimpleBlock::new_uncheked(&payload, 1, 0, false, None, false, true).into(),
			MatroskaSpec::Cluster(Master::End),
			MatroskaSpec::Segment(Master::End),
		];
		let mut data = std::io::Cursor::new(Vec::new());
		let mut writer = WebmWriter::new(&mut data);
		for tag in &tags {
			writer.write(tag).unwrap();
		}
		drop(writer);

		assert_released("mkv", &container(ContainerFormat::Mkv, &data.into_inner(), CHUNK));
	}
}
