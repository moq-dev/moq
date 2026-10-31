//! Record a broadcast's HLS playlists and segments into a directory as they complete.
//!
//! The layout mirrors the served one without the instance: `master.m3u8`, then
//! `{kind}/{rendition}/media.m3u8` with its `init.{n}.mp4` and `{segment}.m4s` files. Each
//! rendition keeps one continuous media playlist across decoder reconfigurations, so the whole
//! recording stays playable from its master playlist. Its segments are numbered from the first
//! one recorded, so a replacement reading another record track continues the numbering.

use std::collections::{BTreeMap, VecDeque, btree_map};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use bytes::Bytes;
use futures::StreamExt;
use futures::stream::FuturesUnordered;
use moq_hls::export::{Broadcaster, Kind, Rendition, master, renditions, segments};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};

/// RFC 3986 unreserved characters, safe as both a file name and a URL path segment. Catalog
/// admission refuses the names `.` and `..`, which this leaves as they are.
const PATH_SEGMENT: &AsciiSet = &NON_ALPHANUMERIC.remove(b'-').remove(b'.').remove(b'_').remove(b'~');

/// fMP4 segments via `EXT-X-MAP` require protocol version 6, and `EXT-X-GAP` version 8.
const VERSION: u32 = 6;
const VERSION_GAP: u32 = 8;

type Key = (Kind, String);

/// A recording in progress, written into its directory as segments complete.
pub struct Recording {
	dir: PathBuf,
	slots: BTreeMap<Key, Slot>,
	/// The master playlist as last written.
	master: Option<String>,
}

/// One rendition's playlist, recorded from each catalog instance of it in turn.
struct Slot {
	/// The directory holding the playlist, init, and segment files.
	dir: PathBuf,
	/// The playlist's URI relative to the master playlist.
	uri: String,
	/// The newest instance while one is recording or queued. Released once every instance has
	/// drained, so a removed rendition holds no subscriptions for the rest of the recording.
	rendition: Option<Arc<Rendition>>,
	/// What the master playlist lists, as the newest instance last described it.
	variant: Variant,
	playlist: Playlist,
	/// The instance being recorded, if its cursor is in flight.
	recording: Option<Instance>,
	/// Replacements whose cursors wait for the current instance to drain, oldest first, each
	/// with the record track it reads.
	queued: VecDeque<(Arc<str>, segments::Consumer)>,
	/// How many instances started, which numbers their init files.
	instances: u64,
	/// The last segment recorded, by any instance.
	last: Option<Last>,
}

/// A rendition's master playlist attributes.
struct Variant {
	bandwidth: u64,
	width: Option<u32>,
	height: Option<u32>,
	codec: String,
}

impl Variant {
	fn of(rendition: &Rendition) -> Self {
		Self {
			bandwidth: rendition.bandwidth(),
			width: rendition.width,
			height: rendition.height,
			codec: rendition.codec.clone(),
		}
	}
}

/// The progress of one instance's cursor.
struct Instance {
	/// Numbers its init file, `init.{index}.mp4`.
	index: u64,
	/// The init file name, once written.
	init: Option<String>,
	/// The record track its segment numbers come from.
	track: Arc<str>,
	/// The last segment the previous instance recorded, which this one must continue.
	after: Option<Last>,
	/// It yielded the segment `after` names, so its next one directly follows.
	overlapped: bool,
	/// It recorded a segment.
	started: bool,
}

/// The source of the last segment recorded.
#[derive(Clone)]
struct Last {
	track: Arc<str>,
	segment: u64,
	discontinuity: u64,
}

/// What a recording does with an instance's next segment.
#[derive(Debug, PartialEq, Eq)]
enum Admit {
	/// The previous instance recorded it already.
	Discard,
	/// Record it, switching from the previous instance first when `transition`.
	Record { transition: bool },
}

impl Instance {
	/// Admit the next segment. A replacement reading its predecessor's record track may repeat
	/// the rows its predecessor recorded, since its cursor can start at the oldest row retained,
	/// so those are discarded. Those numbers name the same span in both and only increase within
	/// a broadcaster run, so a repeated number with a newer discontinuity is a source reset, and
	/// a jump past the next number is a gap nothing recorded. Another record track's numbers name
	/// other content, so all of it is recorded.
	fn admit(&mut self, segment: u64, discontinuity: u64) -> anyhow::Result<Admit> {
		let Some(after) = self.after.as_ref().filter(|_| !self.started) else {
			self.started = true;
			return Ok(Admit::Record { transition: false });
		};
		if after.track == self.track {
			if segment <= after.segment {
				anyhow::ensure!(
					discontinuity <= after.discontinuity,
					"the source restarted its timeline at segment {segment}"
				);
				self.overlapped |= segment == after.segment;
				return Ok(Admit::Discard);
			}
			anyhow::ensure!(
				self.overlapped || segment == after.segment + 1,
				"missed segments {} to {}",
				after.segment + 1,
				segment - 1
			);
		}
		self.started = true;
		Ok(Admit::Record { transition: true })
	}
}

/// A cursor read to make: the next segment of the rendition `key`, and its init with it while
/// the instance has none written yet.
struct Read {
	key: Key,
	cursor: segments::Consumer,
	init: bool,
}

/// A cursor read, handed back with the cursor that made it.
struct Step {
	key: Key,
	cursor: segments::Consumer,
	result: moq_hls::Result<Option<(segments::Segment, Option<Bytes>)>>,
}

async fn advance(read: Read) -> Step {
	let Read { key, mut cursor, init } = read;
	let result: moq_hls::Result<_> = async {
		let Some(segment) = cursor.next().await? else {
			return Ok(None);
		};
		let init = if init && segment.media.is_some() {
			cursor.init().await?
		} else {
			None
		};
		Ok(Some((segment, init)))
	}
	.await;
	Step { key, cursor, result }
}

impl Recording {
	/// Record into `dir`, creating it, and refusing one that holds anything already.
	pub fn create(dir: PathBuf) -> anyhow::Result<Self> {
		std::fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
		let mut entries = std::fs::read_dir(&dir).with_context(|| format!("failed to read {}", dir.display()))?;
		anyhow::ensure!(
			entries.next().is_none(),
			"refusing to record into {}: the directory is not empty",
			dir.display()
		);
		Ok(Self {
			dir,
			slots: BTreeMap::new(),
			master: None,
		})
	}

	/// Record `broadcaster` until it finishes, failing if the recording ends abruptly or misses
	/// a segment. Only a complete recording ends its playlists with `EXT-X-ENDLIST`.
	pub async fn run(mut self, broadcaster: &Broadcaster) -> anyhow::Result<()> {
		let mut changes = broadcaster.renditions();
		let mut steps = FuturesUnordered::new();
		let mut listing = true;
		loop {
			tokio::select! {
				change = changes.next(), if listing => match change.context("recording failed")? {
					Some(renditions::Event::Added(rendition)) => steps.extend(self.add(rendition)?.map(advance)),
					// A removed instance's cursor drains and ends on its own.
					Some(_) => {}
					None => listing = false,
				},
				Some(step) = steps.next() => steps.extend(self.step(step).await?.map(advance)),
				else => break,
			}
		}
		self.finish().await
	}

	/// Start recording a new instance of a rendition, or queue it behind the one recording.
	fn add(&mut self, rendition: Arc<Rendition>) -> anyhow::Result<Option<Read>> {
		let key = (rendition.kind, rendition.name.clone());
		// Subscribe now: the cursor starts at the oldest row retained when it is created.
		let cursor = rendition.segments();
		let track = Arc::from(rendition.hls().track.as_str());
		let slot = match self.slots.entry(key.clone()) {
			btree_map::Entry::Occupied(entry) => entry.into_mut(),
			btree_map::Entry::Vacant(entry) => {
				let name = utf8_percent_encode(&rendition.name, PATH_SEGMENT).to_string();
				let dir = self.dir.join(rendition.kind.as_str()).join(&name);
				create_slot_dir(&dir)?;
				entry.insert(Slot {
					dir,
					uri: format!(
						"{}/{}/media.m3u8",
						rendition.kind.as_str(),
						utf8_percent_encode(&name, PATH_SEGMENT)
					),
					rendition: None,
					variant: Variant::of(&rendition),
					playlist: Playlist::new(rendition.target_duration()),
					recording: None,
					queued: VecDeque::new(),
					instances: 0,
					last: None,
				})
			}
		};
		// A reconfigure keeps the exporter's target, but a rendition removed and re-added derives
		// its own, while the recording's header can't change: the exporter judges overruns
		// against a target this playlist doesn't carry.
		if rendition.target_duration() != slot.playlist.target {
			tracing::warn!(
				kind = rendition.kind.as_str(),
				rendition = %rendition.name,
				served = rendition.target_duration(),
				recorded = slot.playlist.target,
				"rendition returned with another HLS target duration; the recording keeps its own"
			);
		}
		slot.rendition = Some(rendition);
		slot.queued.push_back((track, cursor));
		Ok(match slot.recording {
			Some(_) => None,
			None => slot.begin(key),
		})
	}

	/// Record what one cursor read, returning its next read.
	async fn step(&mut self, step: Step) -> anyhow::Result<Option<Read>> {
		let Step { key, cursor, result } = step;
		let (kind, name) = (key.0.as_str(), &key.1);
		let failed = || format!("failed to record {kind} rendition {name}");
		let read = result.with_context(failed)?;
		let slot = self.slots.get_mut(&key).expect("a cursor's slot outlives it");

		let Some((segment, init)) = read else {
			// This instance drained: its replacement, if any, continues the playlist.
			slot.recording = None;
			let next = slot.begin(key);
			if next.is_none()
				&& let Some(rendition) = slot.rendition.take()
			{
				slot.variant = Variant::of(&rendition);
			}
			return Ok(next);
		};

		let instance = slot.recording.as_mut().expect("a cursor in flight is recording");
		match instance
			.admit(segment.segment, segment.discontinuity)
			.with_context(failed)?
		{
			Admit::Discard => {
				let init = instance.init.is_none();
				return Ok(Some(Read { key, cursor, init }));
			}
			Admit::Record { transition: false } => {}
			Admit::Record { transition: true } => slot.playlist.transition(),
		}

		let instance = slot.recording.as_mut().expect("a cursor in flight is recording");
		let number = slot.playlist.next(segment.segment);
		let mut map = None;
		if let Some(media) = &segment.media {
			if instance.init.is_none() {
				let init = init.with_context(|| format!("{kind} rendition {name} has no init segment"))?;
				let file = format!("init.{}.mp4", instance.index);
				write(&slot.dir.join(&file), &init).await?;
				instance.init = Some(file.clone());
				map = Some(file);
			}
			write(&slot.dir.join(format!("{number}.m4s")), media).await?;
		}
		let init = instance.init.is_none();
		slot.last = Some(Last {
			track: instance.track.clone(),
			segment: segment.segment,
			discontinuity: segment.discontinuity,
		});
		slot.playlist.push(Entry {
			segment: number,
			duration: segment.duration,
			gap: segment.media.is_none(),
			discontinuity: segment.discontinuity,
			program_date_time: segment.program_date_time,
			map,
		});

		self.flush().await?;
		Ok(Some(Read { key, cursor, init }))
	}

	/// Write every playlist that changed, then the master playlist if its listing did.
	async fn flush(&mut self) -> anyhow::Result<()> {
		for slot in self.slots.values_mut() {
			slot.playlist.flush(&slot.dir).await?;
		}
		let master = self.render_master();
		if self.master.as_ref() != Some(&master) {
			write(&self.dir.join("master.m3u8"), master.as_bytes()).await?;
			self.master = Some(master);
		}
		Ok(())
	}

	/// The master playlist over every rendition with a media playlist written.
	fn render_master(&self) -> String {
		let mut video = Vec::new();
		let mut audio = Vec::new();
		for ((kind, name), slot) in self.slots.iter().filter(|(_, slot)| slot.playlist.written.is_some()) {
			let refreshed = slot.rendition.as_deref().map(Variant::of);
			let variant = refreshed.as_ref().unwrap_or(&slot.variant);
			match kind {
				Kind::Video => video.push(master::VideoVariant {
					uri: slot.uri.clone(),
					bandwidth: variant.bandwidth,
					width: variant.width,
					height: variant.height,
					codec: variant.codec.clone(),
				}),
				Kind::Audio => audio.push(master::AudioVariant {
					name: name.clone(),
					uri: slot.uri.clone(),
					bandwidth: variant.bandwidth,
					codec: variant.codec.clone(),
				}),
			}
		}
		master::render(&video, &audio)
	}

	/// End every playlist with `EXT-X-ENDLIST`: the broadcast finished and every cursor drained.
	async fn finish(mut self) -> anyhow::Result<()> {
		self.flush().await?;
		anyhow::ensure!(
			self.slots.values().any(|slot| slot.playlist.written.is_some()),
			"the broadcast finished before recording a segment"
		);
		for slot in self.slots.values() {
			if slot.playlist.written.is_some() {
				append(&slot.dir.join("media.m3u8"), "#EXT-X-ENDLIST\n").await?;
			}
		}
		tracing::info!(dir = %self.dir.display(), "HLS recording complete");
		Ok(())
	}
}

impl Slot {
	/// Start recording the next queued instance of `key`, continuing after the last segment
	/// recorded.
	fn begin(&mut self, key: Key) -> Option<Read> {
		let (track, cursor) = self.queued.pop_front()?;
		self.recording = Some(Instance {
			index: self.instances,
			init: None,
			track,
			after: self.last.clone(),
			overlapped: false,
			started: false,
		});
		self.instances += 1;
		Some(Read {
			key,
			cursor,
			init: true,
		})
	}
}

/// A media playlist, appended to as segments are recorded and rewritten whole only when a line
/// already written changes.
#[derive(Default)]
struct Playlist {
	entries: Vec<Entry>,
	/// The highest `EXT-X-VERSION` the entries need.
	version: u32,
	/// `EXT-X-TARGETDURATION`, in whole seconds: the first instance's, which the exporter serves
	/// too, kept through reconfigurations so the header never changes (RFC 8216 6.2.1).
	target: u64,
	/// What the file holds: its header and how many entries.
	written: Option<(Header, usize)>,
	/// Added to each entry's source sequence, so a switch to another instance is a discontinuity
	/// past the last entry whatever that instance's own sequence starts at.
	offset: u64,
	/// The next entry comes from another instance.
	switching: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Header {
	version: u32,
	sequence: u64,
	discontinuity: u64,
}

struct Entry {
	/// The recording's number for the segment, which names its file.
	segment: u64,
	duration: Duration,
	gap: bool,
	/// The rendition's discontinuity sequence for the segment; the playlist adds its offset.
	discontinuity: u64,
	/// Written on the first entry and after each discontinuity.
	program_date_time: Option<kio::clock::Wall>,
	/// The init file the instance's first media segment maps.
	map: Option<String>,
}

impl Playlist {
	/// A playlist whose `EXT-X-TARGETDURATION` is `target` seconds.
	fn new(target: u64) -> Self {
		Self {
			target,
			..Default::default()
		}
	}

	/// The number of the next entry: the source's for the first, then consecutive.
	fn next(&self, source: u64) -> u64 {
		self.entries.last().map_or(source, |last| last.segment + 1)
	}

	/// Append `entry`.
	///
	/// RFC 8216 4.3.3.1 requires every EXTINF, rounded to the nearest second, to fit the target
	/// duration. An entry past it is listed anyway, as the exporter lists it (and warns): refusing
	/// it loses content, and raising the target would rewrite a header players already read.
	fn push(&mut self, mut entry: Entry) {
		self.version = self.version.max(if entry.gap { VERSION_GAP } else { VERSION });
		if std::mem::take(&mut self.switching)
			&& let Some(last) = self.entries.last()
		{
			self.offset = (last.discontinuity + 1).saturating_sub(entry.discontinuity);
		}
		entry.discontinuity += self.offset;
		self.entries.push(entry);
	}

	/// The next entry comes from a reconfigured instance, under a new init: a discontinuity.
	fn transition(&mut self) {
		self.switching = true;
	}

	async fn flush(&mut self, dir: &Path) -> anyhow::Result<()> {
		let Some(first) = self.entries.first() else {
			return Ok(());
		};
		let header = Header {
			version: self.version,
			sequence: first.segment,
			discontinuity: first.discontinuity,
		};
		let path = dir.join("media.m3u8");
		match self.written {
			Some((written, count)) if written == header => {
				if count < self.entries.len() {
					append(&path, &self.render(count)).await?;
				}
			}
			_ => {
				let mut out = format!(
					"#EXTM3U\n#EXT-X-VERSION:{}\n#EXT-X-TARGETDURATION:{}\n#EXT-X-MEDIA-SEQUENCE:{}\n",
					header.version, self.target, header.sequence
				);
				if header.discontinuity > 0 {
					out += &format!("#EXT-X-DISCONTINUITY-SEQUENCE:{}\n", header.discontinuity);
				}
				out += &self.render(0);
				write(&path, out.as_bytes()).await?;
			}
		}
		self.written = Some((header, self.entries.len()));
		Ok(())
	}

	/// The lines of the entries from `from` on.
	fn render(&self, from: usize) -> String {
		let mut out = String::new();
		let mut previous = from.checked_sub(1).map(|index| self.entries[index].discontinuity);
		for entry in &self.entries[from..] {
			let discontinuity = entry.discontinuity;
			// A skipped discontinuity still counts, so playlists starting anywhere agree.
			for _ in previous.unwrap_or(discontinuity)..discontinuity {
				out += "#EXT-X-DISCONTINUITY\n";
			}
			let restart = previous.is_none_or(|previous| previous != discontinuity);
			previous = Some(discontinuity);
			if let Some(map) = &entry.map {
				out += &format!("#EXT-X-MAP:URI=\"{map}\"\n");
			}
			if restart && let Some(pdt) = entry.program_date_time {
				out += &format!(
					"#EXT-X-PROGRAM-DATE-TIME:{}\n",
					humantime::format_rfc3339_millis(pdt.into())
				);
			}
			if entry.gap {
				out += "#EXT-X-GAP\n";
			}
			out += &format!("#EXTINF:{:.5},\n{}.m4s\n", entry.duration.as_secs_f64(), entry.segment);
		}
		out
	}
}

/// Replace `path` with `contents` in one step, so a reader never sees a partial file.
/// Create a rendition's directory, refusing one that already exists.
///
/// Rendition names are case-sensitive, but a filesystem may fold case or trailing dots, so two
/// renditions can name one directory. The recording starts in an empty directory, so an
/// existing one is another rendition's.
fn create_slot_dir(dir: &Path) -> anyhow::Result<()> {
	let parent = dir.parent().expect("a rendition directory has a kind directory");
	std::fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
	match std::fs::create_dir(dir) {
		Ok(()) => Ok(()),
		Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => anyhow::bail!(
			"refusing to record into {}: another rendition already records there",
			dir.display()
		),
		Err(err) => Err(err).with_context(|| format!("failed to create {}", dir.display())),
	}
}

async fn write(path: &Path, contents: &[u8]) -> anyhow::Result<()> {
	if let Some(parent) = path.parent() {
		tokio::fs::create_dir_all(parent)
			.await
			.with_context(|| format!("failed to create {}", parent.display()))?;
	}
	let mut partial = path.as_os_str().to_owned();
	partial.push(".partial");
	tokio::fs::write(&partial, contents)
		.await
		.with_context(|| format!("failed to write {}", path.display()))?;
	tokio::fs::rename(&partial, path)
		.await
		.with_context(|| format!("failed to write {}", path.display()))
}

async fn append(path: &Path, contents: &str) -> anyhow::Result<()> {
	use tokio::io::AsyncWriteExt;
	let mut file = tokio::fs::OpenOptions::new()
		.append(true)
		.open(path)
		.await
		.with_context(|| format!("failed to open {}", path.display()))?;
	// A tokio file writes in the background until flushed, which would race the next rewrite.
	let written = async {
		file.write_all(contents.as_bytes()).await?;
		file.flush().await
	};
	written
		.await
		.with_context(|| format!("failed to write {}", path.display()))
}

#[cfg(test)]
mod tests {
	use super::*;
	use hang::moq_net;

	type Video = moq_mux::container::Producer<moq_mux::catalog::hang::Container, hang::catalog::VideoConfig>;

	/// A broadcast publishing VP8 renditions with aligned two-second GOPs.
	struct Publisher {
		origin: moq_net::origin::Producer,
		broadcast: moq_net::origin::Publication,
		catalog: moq_mux::catalog::Producer,
		video: Vec<Video>,
	}

	impl Publisher {
		fn new(names: &[&str]) -> Self {
			let origin = moq_tokio::origin::spawn();
			let broadcast = origin.create_broadcast("live").unwrap();
			// Hinted records are advertised before any media, so every rendition is recorded
			// from its first segment.
			let hinted = moq_mux::hls::Config::default().with_duration_hint(Duration::from_secs(2));
			let config = moq_mux::catalog::Config::default().with_hls(hinted);
			let catalog = moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), config).unwrap();
			let reserved = catalog.reserve();
			let video = names
				.iter()
				.map(|name| {
					let info = moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI);
					let track = broadcast.create_track(*name, info).unwrap();
					let container = moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video);
					reserved.video(track, container, config_of(320)).unwrap()
				})
				.collect();
			drop(reserved);
			Self {
				origin,
				broadcast,
				catalog,
				video,
			}
		}

		fn broadcaster(&self) -> Arc<Broadcaster> {
			let source = moq_mux::Source::new(self.origin.consume(), self.broadcast.consume());
			Broadcaster::new(source, moq_hls::export::Config::default())
		}

		/// Write GOP `n`, two seconds from `2n`, to every rendition.
		fn gop(&mut self, n: u64) {
			for video in &mut self.video {
				video.write(frame(n * 2_000_000, true)).unwrap();
				video.write(frame(n * 2_000_000 + 1_000_000, false)).unwrap();
			}
		}

		fn finish(&mut self) {
			for video in &mut self.video {
				video.finish().unwrap();
			}
			self.catalog.finish().unwrap();
		}
	}

	fn config_of(width: u32) -> hang::catalog::VideoConfig {
		let mut config = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8);
		config.coded_width = Some(width);
		config.coded_height = Some(240);
		config.framerate = Some(30.0);
		config
	}

	fn frame(micros: u64, keyframe: bool) -> moq_mux::container::Frame {
		let payload = if keyframe {
			&[0x10, 0x00, 0x00, 0x9d, 0x01, 0x2a, 0x40, 0x01, 0xf0, 0x00][..]
		} else {
			&[0x31, 0x00, 0x00][..]
		};
		moq_mux::container::Frame {
			timestamp: moq_net::Timestamp::from_micros(micros).unwrap(),
			payload: Bytes::copy_from_slice(payload),
			keyframe,
			duration: None,
		}
	}

	const TIMEOUT: Duration = Duration::from_secs(10);

	async fn record(dir: &Path, broadcaster: &Broadcaster) -> anyhow::Result<()> {
		let recording = Recording::create(dir.to_path_buf())?;
		kio::clock::timeout(TIMEOUT, recording.run(broadcaster))
			.await
			.expect("the recording ends")
	}

	fn read(path: PathBuf) -> String {
		std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
	}

	/// The segment numbers a media playlist lists, in order.
	fn listed(playlist: &str) -> Vec<u64> {
		playlist
			.lines()
			.filter_map(|line| line.strip_suffix(".m4s"))
			.map(|segment| segment.parse().unwrap())
			.collect()
	}

	fn spawn(dir: &Path, broadcaster: &Arc<Broadcaster>) -> tokio::task::JoinHandle<anyhow::Result<()>> {
		let recording = Recording::create(dir.to_path_buf()).unwrap();
		let broadcaster = broadcaster.clone();
		tokio::spawn(async move { recording.run(&broadcaster).await })
	}

	/// Wait until the video rendition `name` recorded its first segment.
	async fn recorded(dir: &Path, name: &str) {
		let playlist = dir.join("video").join(name).join("media.m3u8");
		// The recording writes from blocking threads, which wake nothing here.
		kio::clock::timeout(TIMEOUT, async {
			while !std::fs::read_to_string(&playlist).is_ok_and(|text| text.contains("\n0.m4s\n")) {
				tokio::task::yield_now().await;
			}
		})
		.await
		.unwrap_or_else(|_| panic!("{name} never recorded a segment: {:?}", walk(dir)));
	}

	/// The segment numbers a media playlist lists as `EXT-X-GAP`.
	fn gaps(playlist: &str) -> Vec<u64> {
		let mut gap = false;
		let mut gaps = Vec::new();
		for line in playlist.lines() {
			if line == "#EXT-X-GAP" {
				gap = true;
			} else if let Some(segment) = line.strip_suffix(".m4s")
				&& std::mem::take(&mut gap)
			{
				gaps.push(segment.parse().unwrap());
			}
		}
		gaps
	}

	fn consecutive(segments: &[u64], first: u64) -> bool {
		segments.iter().copied().eq(first..first + segments.len() as u64)
	}

	/// The sum of a media playlist's `EXTINF` durations.
	fn duration(playlist: &str) -> Duration {
		playlist
			.lines()
			.filter_map(|line| line.strip_prefix("#EXTINF:")?.strip_suffix(','))
			.map(|secs| Duration::from_secs_f64(secs.parse().unwrap()))
			.sum()
	}

	fn walk(dir: &Path) -> Vec<String> {
		let mut found = Vec::new();
		for entry in std::fs::read_dir(dir).unwrap() {
			let path = entry.unwrap().path();
			if path.is_dir() {
				found.extend(walk(&path));
			} else {
				found.push(path.display().to_string());
			}
		}
		found
	}

	#[tokio::test]
	async fn a_finished_broadcast_records_every_segment() {
		let mut publisher = Publisher::new(&["video0", "video1"]);
		let broadcaster = publisher.broadcaster();
		for n in 0..3 {
			publisher.gop(n);
		}
		publisher.finish();

		let dir = tempfile::tempdir().unwrap();
		record(dir.path(), &broadcaster).await.unwrap();

		let master = read(dir.path().join("master.m3u8"));
		for name in ["video0", "video1"] {
			assert!(master.contains(&format!("\nvideo/{name}/media.m3u8\n")), "{master}");
			let rendition = dir.path().join("video").join(name);
			let playlist = read(rendition.join("media.m3u8"));
			let segments = listed(&playlist);
			assert!(segments.len() > 1 && consecutive(&segments, 0), "{playlist}");
			assert_eq!(duration(&playlist), Duration::from_secs(6), "{playlist}");
			// The exporter's target: the declared duration, the 2s hint's multiple at the 4s grid.
			assert!(playlist.contains("\n#EXT-X-TARGETDURATION:4\n"), "{playlist}");
			assert!(playlist.contains("\n#EXT-X-MEDIA-SEQUENCE:0\n"), "{playlist}");
			assert!(
				playlist.contains("#EXT-X-MAP:URI=\"init.0.mp4\"\n#EXT-X-PROGRAM-DATE-TIME:"),
				"{playlist}"
			);
			assert!(playlist.ends_with(".m4s\n#EXT-X-ENDLIST\n"), "{playlist}");
			assert!(!playlist.contains("DISCONTINUITY"), "{playlist}");
			// The finish's stop is listed as a gap, with no file.
			let gaps = gaps(&playlist);
			assert_eq!(
				gaps,
				segments.last().copied().into_iter().collect::<Vec<_>>(),
				"{playlist}"
			);
			let files = segments
				.iter()
				.filter(|segment| !gaps.contains(segment))
				.map(|segment| format!("{segment}.m4s"));
			for file in files.chain(["init.0.mp4".to_string()]) {
				assert!(
					!std::fs::read(rendition.join(&file)).unwrap().is_empty(),
					"{name}/{file}"
				);
			}
		}
		let partial = walk(dir.path()).into_iter().filter(|path| path.ends_with(".partial"));
		assert_eq!(partial.count(), 0, "every write landed whole");
	}

	/// A reconfigured rendition keeps one playlist: the replacement continues after the last
	/// segment its predecessor recorded, under its own init, behind a discontinuity only its own
	/// playlist marks.
	#[tokio::test]
	async fn a_reconfigured_rendition_continues_its_playlist() {
		let mut publisher = Publisher::new(&["video0", "video1"]);
		let broadcaster = publisher.broadcaster();
		let dir = tempfile::tempdir().unwrap();
		let recording = spawn(dir.path(), &broadcaster);

		// A sibling resolves a segment once its own records pass the segment's end.
		for n in 0..5 {
			publisher.gop(n);
		}
		recorded(dir.path(), "video1").await;
		publisher.video[1].set(config_of(640)).unwrap();
		for n in 5..8 {
			publisher.gop(n);
		}
		publisher.finish();
		kio::clock::timeout(TIMEOUT, recording)
			.await
			.expect("the recording ends")
			.unwrap()
			.unwrap();

		let reconfigured = read(dir.path().join("video/video1/media.m3u8"));
		let sibling = read(dir.path().join("video/video0/media.m3u8"));
		let segments = listed(&reconfigured);
		assert!(consecutive(&segments, 0), "{reconfigured}");
		assert_eq!(duration(&reconfigured), Duration::from_secs(16), "{reconfigured}");
		assert_eq!(listed(&sibling), segments, "{sibling}");
		let switched = reconfigured
			.split("#EXT-X-DISCONTINUITY\n")
			.nth(1)
			.expect("the switch is a discontinuity");
		assert!(
			switched.starts_with("#EXT-X-MAP:URI=\"init.1.mp4\"\n"),
			"{reconfigured}"
		);
		assert!(!sibling.contains("#EXT-X-DISCONTINUITY"), "{sibling}");
		assert!(dir.path().join("video/video1/init.1.mp4").exists());
		let master = read(dir.path().join("master.m3u8"));
		assert!(master.contains("RESOLUTION=640x240"), "{master}");
	}

	/// A rendition removed mid-recording is released once its cursor drains, while the master
	/// playlist keeps listing what it recorded.
	#[tokio::test]
	async fn a_removed_rendition_is_released_once_drained() {
		let mut publisher = Publisher::new(&["video0", "video1"]);
		let broadcaster = publisher.broadcaster();
		let dir = tempfile::tempdir().unwrap();
		let recording = spawn(dir.path(), &broadcaster);
		// A sibling resolves a segment once its own records pass the segment's end.
		for n in 0..5 {
			publisher.gop(n);
		}
		recorded(dir.path(), "video1").await;
		let removed = Arc::downgrade(&broadcaster.rendition(Kind::Video, "video1").unwrap());
		// Finishing bounds its last segment, so the removed instance's cursor drains.
		publisher.video.remove(1).finish().unwrap();
		publisher
			.catalog
			.mutate(|catalog| {
				catalog.video.renditions.remove("video1");
			})
			.unwrap();
		publisher.gop(5);
		publisher.gop(6);
		kio::clock::timeout(TIMEOUT, async {
			while removed.strong_count() > 0 {
				tokio::task::yield_now().await;
			}
		})
		.await
		.expect("the drained rendition was released");

		publisher.gop(7);
		publisher.finish();
		kio::clock::timeout(TIMEOUT, recording)
			.await
			.expect("the recording ends")
			.unwrap()
			.unwrap();
		let master = read(dir.path().join("master.m3u8"));
		assert!(master.contains("\nvideo/video1/media.m3u8\n"), "{master}");
	}

	/// A rendition whose directory another rendition already holds, as two names differing only
	/// in case do on a case-insensitive filesystem, fails the recording instead of mixing files.
	#[tokio::test]
	async fn renditions_sharing_a_directory_are_refused() {
		let mut publisher = Publisher::new(&["video0"]);
		let broadcaster = publisher.broadcaster();
		publisher.gop(0);
		publisher.finish();

		let dir = tempfile::tempdir().unwrap();
		let recording = Recording::create(dir.path().to_path_buf()).unwrap();
		// Stands in for the colliding sibling, which a case-sensitive filesystem cannot produce.
		std::fs::create_dir_all(dir.path().join("video").join("video0")).unwrap();
		let err = kio::clock::timeout(TIMEOUT, recording.run(&broadcaster))
			.await
			.expect("the recording ends")
			.unwrap_err();
		assert!(err.to_string().contains("another rendition"), "{err:#}");
	}

	/// A publisher that drops its session fails the recording, leaving playlists unended.
	#[tokio::test]
	async fn an_abrupt_end_fails_the_recording() {
		let mut publisher = Publisher::new(&["video0"]);
		let broadcaster = publisher.broadcaster();
		let dir = tempfile::tempdir().unwrap();
		let recording = spawn(dir.path(), &broadcaster);
		for n in 0..3 {
			publisher.gop(n);
		}
		recorded(dir.path(), "video0").await;

		let Publisher { catalog, video, .. } = publisher;
		let lost = moq_net::Error::Transport("connection lost".into());
		for video in video {
			video.abort(lost.clone());
		}
		catalog.abort(lost);

		let ended = kio::clock::timeout(TIMEOUT, recording)
			.await
			.expect("the recording ends");
		let err = ended.unwrap().expect_err("an abrupt end fails");
		let playlist = read(dir.path().join("video/video0/media.m3u8"));
		assert!(!playlist.contains("ENDLIST"), "{err:#}: {playlist}");
	}

	#[test]
	fn a_used_directory_is_refused() {
		let dir = tempfile::tempdir().unwrap();
		std::fs::write(dir.path().join("master.m3u8"), "").unwrap();
		let err = Recording::create(dir.path().to_path_buf()).err().expect("refused");
		assert!(format!("{err:#}").contains("not empty"), "{err:#}");
		assert!(
			Recording::create(dir.path().join("fresh")).is_ok(),
			"an absent one is created"
		);
	}

	fn instance(after: Option<(u64, u64)>) -> Instance {
		Instance {
			index: 1,
			init: None,
			track: "video.hls".into(),
			after: after.map(|(segment, discontinuity)| Last {
				track: "video.hls".into(),
				segment,
				discontinuity,
			}),
			overlapped: false,
			started: false,
		}
	}

	#[test]
	fn a_replacement_discards_what_its_predecessor_recorded() {
		let mut replacement = instance(Some((5, 0)));
		for segment in 3..=5 {
			assert_eq!(replacement.admit(segment, 0).unwrap(), Admit::Discard);
		}
		assert_eq!(replacement.admit(6, 1).unwrap(), Admit::Record { transition: true });
		assert_eq!(replacement.admit(7, 1).unwrap(), Admit::Record { transition: false });

		// One whose oldest row is the very next segment overlaps nothing, and needs not.
		let mut adjacent = instance(Some((5, 0)));
		assert_eq!(adjacent.admit(6, 0).unwrap(), Admit::Record { transition: true });

		let mut first = instance(None);
		assert_eq!(first.admit(40, 3).unwrap(), Admit::Record { transition: false });
	}

	/// Another record track numbers other content, so its numbers repeat nothing recorded.
	#[test]
	fn a_replacement_on_another_record_track_records_everything() {
		let mut replacement = Instance {
			track: "video-640.hls".into(),
			..instance(Some((5, 0)))
		};
		assert_eq!(replacement.admit(0, 0).unwrap(), Admit::Record { transition: true });
		assert_eq!(replacement.admit(1, 0).unwrap(), Admit::Record { transition: false });

		// The recording numbers its segments on from the predecessor's.
		let mut playlist = Playlist::new(2);
		for source in 3..=5 {
			let number = playlist.next(source);
			playlist.push(entry(number, 0));
		}
		assert_eq!(playlist.next(0), 6);
	}

	#[test]
	fn a_replacement_refuses_a_gap_or_a_reset() {
		// Rows evicted before the replacement subscribed are missing from both cursors.
		let err = instance(Some((5, 0))).admit(8, 0).unwrap_err();
		assert!(format!("{err}").contains("missed segments 6 to 7"), "{err}");

		// A reused number from a newer discontinuity is another run's content.
		let mut reset = instance(Some((5, 0)));
		assert_eq!(reset.admit(4, 0).unwrap(), Admit::Discard);
		let err = reset.admit(5, 1).unwrap_err();
		assert!(format!("{err}").contains("restarted"), "{err}");
	}

	fn entry(segment: u64, discontinuity: u64) -> Entry {
		Entry {
			segment,
			duration: Duration::from_secs(2),
			gap: false,
			discontinuity,
			program_date_time: None,
			map: (segment == 0).then(|| "init.0.mp4".to_string()),
		}
	}

	/// The wall clock is written on the first entry and after each discontinuity, where the
	/// running sum of durations no longer gives it.
	#[test]
	fn a_discontinuity_restates_the_program_date_time() {
		let at = |secs| Some(kio::clock::Wall::UNIX_EPOCH + Duration::from_secs(secs));
		let mut playlist = Playlist::new(2);
		for (segment, discontinuity, secs) in [(0, 0, 0), (1, 0, 2), (2, 1, 10), (3, 1, 12)] {
			playlist.push(Entry {
				program_date_time: at(secs),
				..entry(segment, discontinuity)
			});
		}
		let text = playlist.render(0);
		assert!(
			text.starts_with("#EXT-X-MAP:URI=\"init.0.mp4\"\n#EXT-X-PROGRAM-DATE-TIME:1970-01-01T00:00:00.000Z\n"),
			"{text}"
		);
		assert!(
			text.contains("1.m4s\n#EXT-X-DISCONTINUITY\n#EXT-X-PROGRAM-DATE-TIME:1970-01-01T00:00:10.000Z\n#EXTINF:"),
			"{text}"
		);
		assert_eq!(text.matches("PROGRAM-DATE-TIME").count(), 2, "{text}");
		// An append continuing the same run restates nothing.
		assert!(!playlist.render(3).contains("PROGRAM-DATE-TIME"));
	}

	/// A switch follows the last entry with a discontinuity even when the new instance's own
	/// sequence starts below the recording's.
	#[test]
	fn a_switch_to_a_lower_sequence_is_a_discontinuity() {
		let mut playlist = Playlist::new(2);
		playlist.push(entry(0, 0));
		playlist.push(entry(1, 2));
		playlist.transition();
		playlist.push(entry(2, 0));
		playlist.push(entry(3, 0));
		let sequences: Vec<_> = playlist.entries.iter().map(|entry| entry.discontinuity).collect();
		assert_eq!(sequences, [0, 2, 3, 3]);
		let text = playlist.render(0);
		assert!(
			text.contains("1.m4s\n#EXT-X-DISCONTINUITY\n#EXTINF:2.00000,\n2.m4s\n#EXTINF:"),
			"{text}"
		);
	}

	/// A switch to a reconfigured instance and the rendition's own break are each a
	/// discontinuity, appended; only a raised header rewrites the playlist.
	#[tokio::test]
	async fn a_transition_appends_a_discontinuity() {
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("media.m3u8");
		let mut playlist = Playlist::new(2);
		for segment in 0..3 {
			playlist.push(entry(segment, 0));
			playlist.flush(dir.path()).await.unwrap();
		}
		assert_eq!(listed(&read(path.clone())), [0, 1, 2]);

		playlist.transition();
		playlist.push(entry(3, 0));
		playlist.push(entry(4, 1));
		playlist.flush(dir.path()).await.unwrap();
		let text = read(path.clone());
		assert_eq!(listed(&text), [0, 1, 2, 3, 4]);
		// The switch at 3, then the rendition's own discontinuity at 4.
		assert!(
			text.contains("2.m4s\n#EXT-X-DISCONTINUITY\n#EXTINF:2.00000,\n3.m4s\n#EXT-X-DISCONTINUITY\n"),
			"{text}"
		);

		// A gap raises the version, which rewrites the header; one past the target duration
		// leaves the target as declared.
		playlist.push(Entry {
			duration: Duration::from_millis(4_500),
			gap: true,
			..entry(5, 1)
		});
		playlist.flush(dir.path()).await.unwrap();
		let text = read(path);
		assert!(
			text.starts_with("#EXTM3U\n#EXT-X-VERSION:8\n#EXT-X-TARGETDURATION:2\n"),
			"{text}"
		);
		assert!(text.ends_with("#EXT-X-GAP\n#EXTINF:4.50000,\n5.m4s\n"), "{text}");
		assert_eq!(text.matches("#EXT-X-DISCONTINUITY\n").count(), 2, "{text}");
	}
}
