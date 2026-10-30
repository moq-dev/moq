//! Segment conformance: what the HLS playlists and the DASH manifest advertise, against the media
//! served as ffprobe decodes it.
//!
//! ffmpeg encodes a ladder whose renditions disagree on keyframes: `video0` has a 2s GOP with
//! B-frames, `video1` a 3s one that overruns the declared 2s duration and changes resolution
//! halfway (a reconfigure, so a new rendition instance), and AAC audio packs into the grid. The
//! transport stream is imported and served across a moq-net session while every playlist and the
//! manifest are polled. Each segment is fetched while listed, until a short window evicts it, and
//! refetched on later reloads, one right after a catalog estimate update; the segment cursors
//! record it too. Once the broadcast finishes, ffprobe decodes every segment against its init.
//!
//! Ignored by default since it runs ffmpeg and ffprobe; `just rs hls-conformance` runs it nightly.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::http::StatusCode;
use bytes::Bytes;
use moq_hls::Server;
use moq_hls::export::renditions;

use crate::support::*;

/// The media's length, and where `video1` changes resolution.
const SECONDS: u32 = 24;
const RECONFIGURE: u32 = 12;

/// The ladder's renditions: two video, one audio.
const RENDITIONS: usize = 3;

/// The declared segment duration, which `video1`'s 3s GOPs overrun.
const DURATION: Duration = Duration::from_secs(2);

/// The playlist window, short enough to evict most of a run's segments.
const WINDOW: Duration = Duration::from_secs(6);

/// How far a decoded start or duration may sit from what a playlist or manifest advertises: the
/// records carry milliseconds, so one for each end of a span, plus ffprobe's microsecond rounding.
const TOLERANCE: f64 = 0.0025;

/// How many pieces the transport stream is imported in, polling after each.
const CHUNKS: usize = 96;

/// How long a poll waits on a held playlist or manifest request before moving on.
const HOLD: Duration = Duration::from_secs(1);

/// Every how many polls each listed segment is refetched, besides right after the catalog
/// estimate update and once the broadcast ends.
const RELOAD: usize = 8;

#[tokio::test(start_paused = true)]
#[ignore = "runs ffmpeg and ffprobe; `just rs hls-conformance` runs it"]
async fn segments_match_their_manifests() {
	let dir = tempfile::tempdir().expect("tempdir");
	let ts = encode(dir.path()).await;

	let pair = Pair::connect().await;
	let publication = pair.publisher.create_broadcast("live").expect("publish");
	publication.announce(Default::default()).expect("announce");
	let mut broadcast = publication.broadcast().clone();
	let hls = moq_mux::hls::Config::default()
		.with_duration(DURATION)
		.with_duration_hint(DURATION);
	let mut catalog =
		moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default().with_hls(hls))
			.expect("catalog");
	let init = moq_mux::import::ContainerInit::new(moq_mux::import::ContainerFormat::Ts, Bytes::new());
	let mut import = moq_mux::import::Container::new(broadcast, catalog.reserve(), &init).expect("import");

	let mut config = moq_hls::export::Config::default();
	config.window = WINDOW;
	let server = Server::new(pair.subscriber.consume(), config);
	let app = server.router();
	let mut seen = Seen::default();

	// The first chunk publishes the catalog, so the recorder follows every rendition from its
	// first segment.
	let chunk = ts.len().div_ceil(CHUNKS).next_multiple_of(188);
	let mut chunks = ts.chunks(chunk);
	import.decode(chunks.next().expect("media")).expect("decode");
	let recorder = Recorder::start(&server).await;

	for (i, chunk) in chunks.enumerate() {
		import.decode(chunk).expect("decode");
		if i == CHUNKS / 3 {
			// What a publisher's estimator writes back: the rendition keeps its instance and its URLs.
			let mut catalog = catalog.modify().expect("catalog open");
			for config in catalog.video.renditions.values_mut() {
				config.bitrate = Some(1_234_567);
				config.jitter = Some(Duration::from_millis(40));
			}
		}
		settle().await;
		// Every listed segment is fetched as soon as it is seen, and refetched now and then.
		seen.reload = i % RELOAD == 0 || i == CHUNKS / 3 + 1;
		seen.poll(&app).await;
		// Imported faster than real time, the window would evict segments before the recording
		// reads them, so nothing more is imported until it has read what the playlists list.
		recorder.caught_up(&seen.newest()).await;
	}
	seen.reload = true;
	import.finish().expect("finish import");
	catalog.finish().expect("finish catalog");

	let deadline = moq_clock::now() + TIMEOUT;
	while seen.poll(&app).await != Polled::Ended {
		assert!(moq_clock::now() < deadline, "the playlists never ended");
		moq_clock::sleep(Duration::from_millis(50)).await;
	}
	let recorded = recorder.finish().await;

	seen.check(&recorded).await;
}

/// Let everything a chunk produced cross the session: on the paused clock, a sleep ends only once
/// every task is idle.
async fn settle() {
	moq_clock::sleep(Duration::from_millis(10)).await;
}

/// Encode the ladder as one transport stream in `dir`.
async fn encode(dir: &Path) -> Vec<u8> {
	// video1 in two halves, each opening with its own parameter sets, so the stream reconfigures.
	// One level for both keeps the codec string, so the rendition keeps its name and the export
	// replaces its instance rather than adding a rendition.
	let mut halves = Vec::new();
	for (start, size) in [(0, "320x180"), (RECONFIGURE, "480x270")] {
		let path = dir.join(format!("video1.{start}.h264"));
		ffmpeg(&[
			"-f",
			"lavfi",
			"-i",
			&format!("testsrc2=size={size}:rate=30"),
			"-t",
			&(SECONDS - RECONFIGURE).to_string(),
			"-c:v",
			"libx264",
			"-preset",
			"ultrafast",
			"-x264-params",
			"keyint=90:min-keyint=90:scenecut=0:bframes=0:level=3.0",
			"-f",
			"h264",
			path.to_str().expect("utf-8"),
		])
		.await;
		halves.extend(std::fs::read(&path).expect("read half"));
	}
	let video1 = dir.join("video1.h264");
	std::fs::write(&video1, halves).expect("write video1");

	let ts = dir.join("ladder.ts");
	ffmpeg(&[
		"-f",
		"lavfi",
		"-i",
		"testsrc2=size=640x360:rate=30",
		"-f",
		"lavfi",
		"-i",
		"sine=frequency=440:sample_rate=48000",
		"-framerate",
		"30",
		"-i",
		video1.to_str().expect("utf-8"),
		"-t",
		&SECONDS.to_string(),
		"-map",
		"0:v",
		"-map",
		"2:v",
		"-map",
		"1:a",
		"-c:v:0",
		"libx264",
		"-preset:v:0",
		"veryfast",
		"-x264-params:v:0",
		"keyint=60:min-keyint=60:scenecut=0:bframes=2",
		"-c:v:1",
		"copy",
		"-c:a",
		"aac",
		"-b:a",
		"96k",
		"-f",
		"mpegts",
		ts.to_str().expect("utf-8"),
	])
	.await;
	std::fs::read(&ts).expect("read ladder")
}

async fn ffmpeg(args: &[&str]) {
	let output = tokio::process::Command::new("ffmpeg")
		.args(["-hide_banner", "-loglevel", "error", "-y"])
		.args(args)
		.output()
		.await
		.expect("run ffmpeg");
	assert!(
		output.status.success(),
		"ffmpeg {args:?}: {}",
		String::from_utf8_lossy(&output.stderr)
	);
}

/// Segment cursors over every rendition instance, recording each segment's bytes by rendition
/// name and number.
struct Recorder {
	recorded: Arc<Mutex<Recorded>>,
	task: tokio::task::JoinHandle<()>,
}

impl Recorder {
	async fn start(server: &Server) -> Self {
		let broadcaster = moq_clock::timeout(TIMEOUT, server.broadcaster("live"))
			.await
			.expect("resolve timed out")
			.expect("announced");
		let recorded = Arc::new(Mutex::new(Recorded::default()));
		let task = tokio::spawn({
			let recorded = recorded.clone();
			async move {
				let mut renditions = broadcaster.renditions();
				let mut cursors = Vec::new();
				while let Some(event) = renditions.next().await.expect("renditions") {
					let renditions::Event::Added(rendition) = event else {
						continue;
					};
					let recorded = recorded.clone();
					cursors.push(tokio::spawn(async move {
						let mut cursor = rendition.segments();
						let name = &rendition.name;
						loop {
							let segment = match cursor.next().await {
								Ok(Some(segment)) => segment,
								Ok(None) => break,
								Err(err) => {
									let mut recorded = recorded.lock().unwrap();
									recorded.failures.push(format!("recording {name}: {err}"));
									recorded.failed.insert(name.clone());
									break;
								}
							};
							let mut recorded = recorded.lock().unwrap();
							// Where content stops, a zero-duration gap ends the segment before it.
							let Some(media) = segment.media else {
								if !segment.duration.is_zero() {
									recorded
										.failures
										.push(format!("{name} recorded {} as a gap", segment.segment));
								}
								continue;
							};
							let segments = recorded.segments.entry(name.clone()).or_default();
							if segments.insert(segment.segment, media).is_some() {
								recorded
									.failures
									.push(format!("{name} recorded {} twice", segment.segment));
							}
						}
					}));
				}
				for cursor in cursors {
					cursor.await.expect("cursor");
				}
			}
		});
		Self { recorded, task }
	}

	/// Wait until each rendition's cursor has recorded the `newest` segment its playlist listed,
	/// or failed.
	async fn caught_up(&self, newest: &BTreeMap<&str, u64>) {
		let deadline = moq_clock::now() + TIMEOUT;
		loop {
			let behind = {
				let recorded = self.recorded.lock().unwrap();
				newest
					.iter()
					.find(|(name, number)| {
						let failed = recorded.failed.contains(**name);
						let read = recorded
							.segments
							.get(**name)
							.is_some_and(|read| read.contains_key(number));
						!failed && !read
					})
					.map(|(name, number)| format!("{name} {number}"))
			};
			let Some(behind) = behind else {
				return;
			};
			assert!(moq_clock::now() < deadline, "the recording never reached {behind}");
			moq_clock::sleep(Duration::from_millis(10)).await;
		}
	}

	async fn finish(self) -> Recorded {
		moq_clock::timeout(TIMEOUT, self.task)
			.await
			.expect("the recording never ended")
			.expect("recorder");
		std::mem::take(&mut *self.recorded.lock().unwrap())
	}
}

/// What the segment cursors read.
#[derive(Default)]
struct Recorded {
	/// Each rendition's segments by number, across its instances.
	segments: BTreeMap<String, BTreeMap<u64, Bytes>>,
	/// Cursors that failed or read something unexpected.
	failures: Vec<String>,
	/// The renditions whose cursor failed.
	failed: BTreeSet<String>,
}

/// One segment as a media playlist lists it.
#[derive(Clone, Debug, PartialEq)]
struct Listed {
	/// The segment's URL, under the broadcast.
	uri: String,
	/// Its init's URL, from the `EXT-X-MAP` before it.
	init: String,
	/// Its `EXTINF`, in seconds.
	duration: f64,
	/// Whether an `EXT-X-DISCONTINUITY` precedes it.
	discontinuity: bool,
	/// Whether it is an `EXT-X-GAP`: nothing to fetch.
	gap: bool,
}

/// One segment as the DASH manifest lists it.
#[derive(Clone, Debug, PartialEq)]
struct Timed {
	uri: String,
	init: String,
	/// `S@t` and `S@d`, in seconds.
	start: f64,
	duration: f64,
}

/// How far one poll got, least first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Polled {
	/// A playlist or the manifest was held waiting for media.
	Held,
	/// Everything rendered, and the broadcast is live.
	Live,
	/// Everything rendered, and every playlist and the manifest ended.
	Ended,
}

/// Everything the polls saw.
#[derive(Default)]
struct Seen {
	/// Each media playlist's segments by number, as first listed. A segment's listing never
	/// changes while it stays listed.
	playlists: BTreeMap<String, BTreeMap<u64, Listed>>,
	/// Each media playlist's `EXT-X-TARGETDURATION`, which holds for the run.
	targets: BTreeMap<String, u64>,
	/// The rendition name each media playlist serves.
	names: BTreeMap<String, String>,
	/// Each DASH representation's segments by `S@t`, as first listed.
	timelines: BTreeMap<String, BTreeMap<u64, Timed>>,
	/// Every segment and init URL fetched, with its bytes from the first fetch.
	bodies: BTreeMap<String, Bytes>,
	/// Whether any playlist evicted a segment.
	evicted: bool,
	/// Whether this poll refetches what earlier ones already fetched.
	reload: bool,
	/// Fetches that failed while their segment was listed.
	failures: Vec<String>,
	/// Whether a playlist advertised `EXT-X-INDEPENDENT-SEGMENTS`.
	independent: bool,
	/// Whether each DASH adaptation set advertised `startWithSAP="1"`, by representation id.
	sap: BTreeMap<String, bool>,
}

impl Seen {
	/// Reload the master playlist, every media playlist, and the manifest, fetching every listed
	/// segment: new ones for the first time, the rest again on a [`reload`](Self::reload).
	async fn poll(&mut self, app: &axum::Router) -> Polled {
		// The master holds a request until a rendition is playable, which needs more media.
		let Some(master) = render(app, "/live/master.m3u8").await else {
			return Polled::Held;
		};
		self.independent |= master.contains("#EXT-X-INDEPENDENT-SEGMENTS");
		let mut uris: Vec<String> = master
			.lines()
			.filter(|line| !line.starts_with('#') && line.ends_with(".m3u8"))
			.map(str::to_string)
			.collect();
		uris.extend(
			master
				.lines()
				.filter_map(|line| Some(attribute(line, "URI")?.to_string())),
		);

		let mut polled = Polled::Ended;
		for uri in uris {
			polled = polled.min(self.media(app, &uri).await);
		}
		polled.min(self.manifest(app).await)
	}

	async fn media(&mut self, app: &axum::Router, uri: &str) -> Polled {
		let base = format!("/live/{}", uri.trim_end_matches("media.m3u8"));
		let Some(body) = render(app, &format!("/live/{uri}")).await else {
			return Polled::Held;
		};
		self.independent |= body.contains("#EXT-X-INDEPENDENT-SEGMENTS");
		let name = uri.rsplit('/').nth(1).expect("a rendition");
		let name = percent_encoding::percent_decode_str(name)
			.decode_utf8()
			.expect("utf-8")
			.into_owned();
		self.names.insert(uri.to_string(), name);

		let tag = |prefix: &str| body.lines().find_map(|line| line.strip_prefix(prefix));
		let target: u64 = tag("#EXT-X-TARGETDURATION:")
			.expect("a target")
			.parse()
			.expect("integer");
		let held = *self.targets.entry(uri.to_string()).or_insert(target);
		assert_eq!(held, target, "{uri} changed its EXT-X-TARGETDURATION");
		let mut number: u64 = tag("#EXT-X-MEDIA-SEQUENCE:")
			.expect("a sequence")
			.parse()
			.expect("integer");

		let mut init = None;
		let mut duration = None;
		let mut discontinuity = false;
		let mut gap = false;
		let mut listed = Vec::new();
		for line in body.lines() {
			if let Some(map) = line.strip_prefix("#EXT-X-MAP:") {
				init = Some(format!("{base}{}", attribute(map, "URI").expect("a map URI")));
			} else if let Some(extinf) = line.strip_prefix("#EXTINF:") {
				duration = Some(extinf.trim_end_matches(',').parse::<f64>().expect("a duration"));
			} else if line == "#EXT-X-DISCONTINUITY" {
				discontinuity = true;
			} else if line == "#EXT-X-GAP" {
				gap = true;
			} else if !line.starts_with('#') && !line.is_empty() {
				let suffix = format!(".{number}.m4s");
				assert!(line.ends_with(&suffix), "{uri}: segment {number} listed as {line}");
				let segment = Listed {
					uri: format!("{base}{line}"),
					init: init.clone().expect("an EXT-X-MAP before the segment"),
					duration: duration.take().expect("an EXTINF before the segment"),
					discontinuity: std::mem::take(&mut discontinuity),
					gap: std::mem::take(&mut gap),
				};
				// Every segment of an unaligned ladder has media. Only where content stops does a
				// zero-duration gap end the segment before it.
				assert!(
					!segment.gap || segment.duration == 0.0,
					"{uri} lists segment {number} as a gap:\n{body}"
				);
				listed.push((number, segment));
				number += 1;
			}
		}

		let playlist = self.playlists.entry(uri.to_string()).or_default();
		if let (Some((first, _)), Some((&last, _))) = (listed.first(), playlist.last_key_value()) {
			assert!(
				*first <= last + 1,
				"{uri} evicted segments {} to {first} unseen",
				last + 1
			);
			if *first > *playlist.first_key_value().unwrap().0 {
				self.evicted = true;
			}
		}
		for (number, segment) in listed {
			let first = playlist.entry(number).or_insert_with(|| segment.clone());
			// The discontinuity tag moves to the first listed segment's sequence header once
			// the segment before it is evicted, so only the rest of the listing must hold.
			assert_eq!(
				(&first.uri, &first.init, first.duration, first.gap),
				(&segment.uri, &segment.init, segment.duration, segment.gap),
				"{uri} relisted segment {number} differently"
			);
			if !segment.gap {
				for resource in [&segment.init, &segment.uri] {
					if let Err(err) = fetch(app, &mut self.bodies, resource, self.reload).await {
						self.failures
							.push(format!("{uri} segment {number}: {err} while listed:\n{body}"));
					}
				}
			}
		}
		match body.contains("#EXT-X-ENDLIST") {
			true => Polled::Ended,
			false => Polled::Live,
		}
	}

	async fn manifest(&mut self, app: &axum::Router) -> Polled {
		let Some(body) = render(app, "/live/manifest.mpd").await else {
			return Polled::Held;
		};
		assert!(
			!body.contains("segmentAlignment"),
			"the manifest claims alignment for an unaligned ladder:\n{body}"
		);

		let mut sap = None;
		let mut id = None;
		let mut template = None;
		let mut entries = Vec::new();
		for element in body.split('<').map(|element| element.trim_end()) {
			if element.starts_with("AdaptationSet ") {
				sap = Some(attribute(element, "startWithSAP") == Some("1"));
			} else if element.starts_with("Representation ") {
				let representation = attribute(element, "id").expect("an id").to_string();
				self.sap
					.insert(representation.clone(), sap.expect("inside an adaptation set"));
				id = Some(representation);
			} else if element.starts_with("SegmentTemplate ") {
				let timescale: f64 = attribute(element, "timescale").expect("a timescale").parse().unwrap();
				let init = unescape(attribute(element, "initialization").expect("an init"));
				let media = unescape(attribute(element, "media").expect("a media template"));
				template = Some((timescale, format!("/live/{init}"), format!("/live/{media}")));
			} else if element.starts_with("S ") {
				let (timescale, init, media) = template.clone().expect("inside a template");
				let t: u64 = attribute(element, "t").expect("an explicit t").parse().unwrap();
				let d: u64 = attribute(element, "d").expect("a d").parse().unwrap();
				assert!(attribute(element, "r").is_none(), "an S repeats: {element}");
				let segment = Timed {
					uri: media.replace("$Time$", &t.to_string()),
					init,
					start: t as f64 / timescale,
					duration: d as f64 / timescale,
				};
				entries.push((id.clone().expect("inside a representation"), t, segment));
			}
		}

		for (id, t, segment) in entries {
			let first = self
				.timelines
				.entry(id.clone())
				.or_default()
				.entry(t)
				.or_insert_with(|| segment.clone());
			assert_eq!(*first, segment, "{id} relisted t={t} differently");
			for resource in [&segment.init, &segment.uri] {
				if let Err(err) = fetch(app, &mut self.bodies, resource, self.reload).await {
					self.failures.push(format!("{id} t={t}: {err} while listed"));
				}
			}
		}
		match body.contains(r#"type="static""#) {
			true => Polled::Ended,
			false => Polled::Live,
		}
	}

	/// The newest segment each rendition's playlist listed with media, by rendition name.
	fn newest(&self) -> BTreeMap<&str, u64> {
		let mut newest = BTreeMap::new();
		for (uri, playlist) in &self.playlists {
			if let Some((number, _)) = playlist.iter().rev().find(|(_, segment)| !segment.gap) {
				let entry = newest.entry(self.names[uri].as_str()).or_insert(*number);
				*entry = (*entry).max(*number);
			}
		}
		newest
	}

	/// Probe everything fetched, and compare it with what was advertised and recorded.
	async fn check(&self, recorded: &Recorded) {
		assert!(self.evicted, "no playlist evicted a segment");
		assert_eq!(self.playlists.len(), RENDITIONS, "{:?}", self.playlists.keys());
		assert_eq!(self.timelines.len(), RENDITIONS, "{:?}", self.timelines.keys());

		// Every disagreement, so one run reports them all.
		let mut failures = [self.failures.clone(), recorded.failures.clone()].concat();
		let mut fail = |failure: String| failures.push(failure);
		let mut probes = Probes::default();
		let mut overran = false;
		let mut reconfigured = false;
		for (uri, playlist) in &self.playlists {
			let name = &self.names[uri];
			let target = self.targets[uri] as f64;
			let mut previous: Option<(f64, &Listed)> = None;
			let numbers: Vec<u64> = playlist.keys().copied().collect();
			if numbers.windows(2).any(|pair| pair[1] != pair[0] + 1) {
				fail(format!("{uri} skipped a number: {numbers:?}"));
			}
			for (number, segment) in playlist.iter().filter(|(_, segment)| !segment.gap) {
				let Some(probe) = probes.get(&self.bodies, &segment.init, &segment.uri, &mut fail).await else {
					continue;
				};
				let at = format!("{uri} segment {number}");
				if self.independent {
					probe.check_sync(&at, &mut fail);
				}
				if (probe.duration() - segment.duration).abs() > TOLERANCE {
					fail(format!(
						"{at}: EXTINF {} but decodes {:.6}",
						segment.duration,
						probe.duration()
					));
				}
				overran |= segment.duration > target;
				if let Some((start, previous)) = previous
					&& !segment.discontinuity
				{
					let expected = start + previous.duration;
					if (probe.start - expected).abs() > TOLERANCE {
						fail(format!(
							"{at}: decodes from {:.6} but the segment before it ends at {expected:.6}",
							probe.start
						));
					}
				}
				reconfigured |= previous.is_some_and(|(_, previous)| previous.init != segment.init);
				previous = Some((probe.start, segment));

				let recording = recorded.segments.get(name).and_then(|segments| segments.get(number));
				if recording != self.bodies.get(&segment.uri) {
					fail(format!("{at}: the recording differs from what was served"));
				}
			}
			let listed: Vec<u64> = playlist.iter().filter(|(_, s)| !s.gap).map(|(n, _)| *n).collect();
			let recording: Vec<u64> = recorded
				.segments
				.get(name)
				.into_iter()
				.flat_map(|r| r.keys().copied())
				.collect();
			if recording != listed {
				fail(format!("{name}: recorded {recording:?} but listed {listed:?}"));
			}
		}
		if !overran {
			fail("no segment overran its target duration".to_string());
		}
		if !reconfigured {
			fail("no rendition was reconfigured".to_string());
		}

		for (id, timeline) in &self.timelines {
			for (t, segment) in timeline {
				let Some(probe) = probes.get(&self.bodies, &segment.init, &segment.uri, &mut fail).await else {
					continue;
				};
				let at = format!("{id} t={t}");
				if self.sap[id] {
					probe.check_sync(&at, &mut fail);
				}
				if (probe.start - segment.start).abs() > TOLERANCE {
					fail(format!(
						"{at}: S@t is {} but decodes from {:.6}",
						segment.start, probe.start
					));
				}
				if (probe.duration() - segment.duration).abs() > TOLERANCE {
					fail(format!(
						"{at}: S@d is {} but decodes {:.6}",
						segment.duration,
						probe.duration()
					));
				}
			}
		}
		assert!(
			failures.is_empty(),
			"{} failures:\n{}",
			failures.len(),
			failures.join("\n")
		);
	}
}

/// GET the playlist or manifest at `uri`, or `None` while the export holds the request waiting for
/// media that only later chunks bring.
async fn render(app: &axum::Router, uri: &str) -> Option<String> {
	let response = moq_clock::timeout(HOLD, get(app, uri)).await.ok()?;
	(response.status() == StatusCode::OK).then(|| String::from_utf8(response.body().to_vec()).expect("utf-8"))
}

/// GET `uri` and keep its bytes, or on a `reload`, check they match the first fetch's.
async fn fetch(
	app: &axum::Router,
	bodies: &mut BTreeMap<String, Bytes>,
	uri: &str,
	reload: bool,
) -> Result<(), String> {
	if !reload && bodies.contains_key(uri) {
		return Ok(());
	}
	let response = get(app, uri).await;
	if response.status() != StatusCode::OK {
		return Err(format!("{uri} answered {}", response.status()));
	}
	let body = response.into_body();
	if body.is_empty() {
		return Err(format!("{uri} is empty"));
	}
	let first = bodies.entry(uri.to_string()).or_insert_with(|| body.clone());
	if *first != body {
		return Err(format!("{uri} changed bytes on a reload"));
	}
	Ok(())
}

/// The value of `name="..."` in an XML element or an HLS attribute list.
fn attribute<'a>(element: &'a str, name: &str) -> Option<&'a str> {
	let needle = format!("{name}=\"");
	let start = element
		.match_indices(&needle)
		.map(|(i, _)| i)
		.find(|&i| i == 0 || matches!(element.as_bytes()[i - 1], b' ' | b',' | b':'))?;
	element[start + needle.len()..].split('"').next()
}

fn unescape(value: &str) -> String {
	value
		.replace("&quot;", "\"")
		.replace("&apos;", "'")
		.replace("&lt;", "<")
		.replace("&gt;", ">")
		.replace("&amp;", "&")
}

/// What ffprobe decoded from one init and segment.
#[derive(Clone, Debug)]
struct Probe {
	/// The earliest presentation time, in seconds.
	start: f64,
	/// The latest presentation end, in seconds.
	end: f64,
	/// Whether the first packet in decode order is a sync sample.
	sync: bool,
	/// Whether the first frame presented is a keyframe.
	leads: bool,
}

impl Probe {
	fn duration(&self) -> f64 {
		self.end - self.start
	}

	/// A segment advertised as starting with a SAP opens on a keyframe presented first.
	fn check_sync(&self, at: &str, fail: &mut impl FnMut(String)) {
		if !self.sync {
			fail(format!("{at}: advertised as independent but opens on a non-sync frame"));
		} else if !self.leads {
			fail(format!("{at}: opens on a keyframe that isn't presented first"));
		}
	}
}

/// ffprobe results by the init and segment they decoded.
#[derive(Default)]
struct Probes(HashMap<(String, String), Option<Probe>>);

impl Probes {
	/// Probe `segment` against `init`, unless either failed to fetch or ffprobe failed on it.
	async fn get(
		&mut self,
		bodies: &BTreeMap<String, Bytes>,
		init: &str,
		segment: &str,
		fail: &mut impl FnMut(String),
	) -> Option<Probe> {
		let key = (init.to_string(), segment.to_string());
		if let Some(probe) = self.0.get(&key) {
			return probe.clone();
		}
		let (init, segment) = (bodies.get(init)?, bodies.get(segment)?);
		let mut input = init.to_vec();
		input.extend_from_slice(segment);
		let probe = match probe(&input).await {
			Ok(mut probe) => {
				let shift = reorder(init, segment);
				probe.start -= shift;
				probe.end -= shift;
				Some(probe)
			}
			Err(err) => {
				fail(format!(
					"{} against {}: {}",
					key.1,
					key.0,
					err.lines().next().unwrap_or_default()
				));
				None
			}
		};
		self.0.insert(key, probe.clone());
		probe
	}
}

/// How far ffmpeg delays a fragment's presentation: with no edit list, its mov demuxer shifts
/// every presentation time by the most negative composition offset, so none precedes its
/// decode time. A player presents at `tfdt` plus the offset (ISO/IEC 14496-12 8.8.8), so this is
/// taken back off what ffprobe reports.
fn reorder(init: &[u8], segment: &[u8]) -> f64 {
	use mp4_atom::{Any, DecodeMaybe};

	let mut timescale = None;
	let mut offset = 0;
	let mut cursor = std::io::Cursor::new([init, segment].concat());
	while let Some(atom) = Any::decode_maybe(&mut cursor).expect("an atom") {
		match atom {
			Any::Moov(moov) => timescale = Some(moov.trak[0].mdia.mdhd.timescale),
			Any::Moof(moof) => {
				let offsets = moof
					.traf
					.iter()
					.flat_map(|traf| &traf.trun)
					.flat_map(|trun| &trun.entries);
				offset = offsets.filter_map(|entry| entry.cts).fold(offset, i32::min);
			}
			_ => {}
		}
	}
	-f64::from(offset) / f64::from(timescale.expect("an init"))
}

/// Decode a whole fMP4 with ffprobe.
async fn probe(input: &[u8]) -> Result<Probe, String> {
	use tokio::io::AsyncWriteExt;

	let mut child = tokio::process::Command::new("ffprobe")
		.args([
			"-v",
			"error",
			"-show_entries",
			"packet=flags:frame=key_frame,pts_time,duration_time",
		])
		.args(["-of", "csv", "-i", "pipe:0"])
		.stdin(std::process::Stdio::piped())
		.stdout(std::process::Stdio::piped())
		.stderr(std::process::Stdio::piped())
		.spawn()
		.expect("run ffprobe");
	let mut stdin = child.stdin.take().expect("stdin");
	let input = input.to_vec();
	let writer = tokio::spawn(async move { stdin.write_all(&input).await });
	let output = child.wait_with_output().await.expect("ffprobe");
	writer.await.expect("writer").map_err(|err| err.to_string())?;
	let stderr = String::from_utf8_lossy(&output.stderr);
	if !output.status.success() || !stderr.trim().is_empty() {
		return Err(format!("ffprobe failed: {stderr}"));
	}

	let mut sync = None;
	let mut frames = Vec::new();
	for line in String::from_utf8_lossy(&output.stdout).lines() {
		let fields: Vec<&str> = line.trim_end_matches(',').split(',').collect();
		match fields.as_slice() {
			["packet", flags] => {
				sync.get_or_insert(flags.starts_with('K'));
			}
			["frame", key, pts, duration] => {
				let pts: f64 = pts.parse().map_err(|_| format!("no pts in {line:?}"))?;
				let duration: f64 = duration.parse().map_err(|_| format!("no duration in {line:?}"))?;
				frames.push((*key == "1", pts, duration));
			}
			_ => return Err(format!("unexpected ffprobe line {line:?}")),
		}
	}
	// Frames come out in presentation order, so the first is the earliest presented.
	let (key, first, _) = *frames.first().ok_or("no frames decoded")?;
	let start = frames.iter().map(|f| f.1).fold(f64::INFINITY, f64::min);
	let end = frames.iter().map(|f| f.1 + f.2).fold(f64::NEG_INFINITY, f64::max);
	Ok(Probe {
		start,
		end,
		sync: sync.unwrap_or(false),
		leads: key && first == start,
	})
}
