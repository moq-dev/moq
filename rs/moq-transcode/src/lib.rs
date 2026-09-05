//! Just-in-time live transcoding for hang broadcasts.
//!
//! [`run`] consumes a source broadcast and fills a derivative broadcast: a
//! catalog advertising lower renditions (rungs) of the source video plus
//! references back to the source renditions, and one output video track per
//! rung. The catalog is published immediately and deterministically (codec
//! strings are computed from the ladder, not the bitstream), but nothing is
//! encoded until a subscriber actually asks:
//!
//! - Subscribing to a rung attaches it to a shared live decode of the source
//!   (one subscription and one decoder per source, no matter how many rungs
//!   are active); each rung resizes and encodes its own copy, group for group,
//!   stopping when the last subscriber leaves.
//! - Fetching a specific group fetches that same group from the source and
//!   transcodes just that group. Output groups mirror source sequence numbers
//!   1:1, so group N of every rung is the same content as source group N.
//!
//! The codec work is `moq-video`: hardware where available (NVDEC + NVENC on
//! Linux, VideoToolbox on macOS, Media Foundation on Windows) with openh264 as
//! the H.264 software fallback. On an NVIDIA GPU the whole pipeline is
//! GPU-resident: NVDEC decodes and scales in hardware and NVENC encodes the
//! CUDA frame in place, with no CPU copies. Other decoders scale on the CPU.

pub mod active;

mod catalog;
mod config;
mod error;
mod feed;
mod ladder;
mod rung;

pub use config::{Config, Rung};

#[allow(deprecated)]
pub use config::source_reference;
pub use error::Error;

/// Transcode `source` into `output` until the source broadcast ends.
///
/// A shorthand for [`Transcoder::new`] followed by [`Transcoder::run`], for a
/// caller with nothing to observe.
pub async fn run(
	source: moq_net::broadcast::Consumer,
	output: moq_net::broadcast::Producer,
	config: Config,
) -> Result<(), Error> {
	Transcoder::new(source, output, config)?.run().await
}

/// A transcoder, split from the future that drives it.
///
/// Reads the source catalog, publishes the derivative catalog (rungs strictly
/// below the source, plus source renditions referenced via [`Config::source`]),
/// and serves each rung just-in-time: a rung track only materializes when a
/// consumer asks for it, and only encodes while consumed. Where `output` is
/// announced (and how its path relates to the source) is the caller's business.
///
/// The split exists so a caller can attach [`active`] before any encoding
/// starts. [`run`](Self::run) consumes the transcoder, so take the cursors you
/// want first.
pub struct Transcoder {
	source: moq_net::broadcast::Consumer,
	output: moq_net::broadcast::Producer,
	config: Config,
	derived: moq_mux::catalog::Producer,
	// Consumers asking for a rung before (or after) it exists queue here.
	dynamic: moq_net::broadcast::Dynamic,
	active: active::Producer,
}

impl Transcoder {
	/// Register the catalog tracks and the on-demand rung handler on `output`.
	///
	/// Synchronous, and everything a consumer can race is in place by the time
	/// it returns, so announce `output` after this rather than before.
	pub fn new(
		source: moq_net::broadcast::Consumer,
		mut output: moq_net::broadcast::Producer,
		config: Config,
	) -> Result<Self, Error> {
		// The catalog starts empty and fills in during `run`, exactly like a
		// media importer that hasn't seen parameter sets yet.
		let derived = moq_mux::catalog::Producer::new(&mut output)?;
		let dynamic = output.dynamic();

		Ok(Self {
			source,
			output,
			config,
			derived,
			dynamic,
			active: active::Producer::default(),
		})
	}

	/// A cursor over the renditions this transcoder produces.
	///
	/// Each call returns an independent cursor, positioned before the ladder so
	/// it reports every rendition once and everything already encoding. See
	/// [`active::Consumer`].
	pub fn active(&self) -> active::Consumer {
		self.active.consume()
	}

	/// Serve the ladder until the source broadcast ends.
	pub async fn run(self) -> Result<(), Error> {
		let Self {
			source,
			mut output,
			config,
			mut derived,
			mut dynamic,
			active,
		} = self;

		// The source catalog drives everything; wait for a snapshot with a usable
		// video rendition (the first may precede the source publishing its video).
		let track = source
			.track(hang::Catalog::DEFAULT_NAME)?
			.subscribe(hang::Catalog::default_subscription())
			.await?;
		let mut catalogs = moq_mux::catalog::hang::Consumer::<()>::new(track);
		let (source_name, source_config, snapshot) = loop {
			let Some(snapshot) = catalogs.next().await? else {
				return Err(Error::NoSource);
			};
			match catalog::choose_source(&snapshot.video) {
				Ok((name, config)) => break (name, config, snapshot),
				Err(_) => tracing::debug!("no transcodable rendition yet; waiting for a catalog update"),
			}
		};
		// The ladder, the shared decode behind it, and the rungs serving off it.
		// Resolved again on every source catalog snapshot, so a source that resizes
		// mid-stream takes the ladder with it.
		let mut ladder =
			ladder::Ladder::new(source.clone(), config.clone(), active, source_name, source_config).await?;

		// Publish the derivative catalog before any encoder exists, so subscribers
		// can pick a rung immediately.
		{
			let mut guard = derived.lock();
			catalog::populate(&mut guard, &snapshot, ladder.rungs(), config.source.as_ref())?;
		}

		// Serve rung requests and follow source catalog updates until the source ends.
		let mut tasks = tokio::task::JoinSet::new();
		loop {
			tokio::select! {
				request = dynamic.requested_track() => {
					// Err means the broadcast closed; nothing left to serve.
					let Ok(request) = request else { break };
					match ladder.rung(request.name())? {
						Some(rung) => { tasks.spawn(rung::serve(rung, request)); }
						None => request.reject(moq_net::Error::NotFound),
					}
				},
				update = catalogs.next() => match update {
					Ok(Some(snapshot)) => {
						ladder.follow(&snapshot.video).await?;
						let mut guard = derived.lock();
						catalog::populate(&mut guard, &snapshot, ladder.rungs(), config.source.as_ref())?;
					}
					// The source ended (or its catalog track died): wind down.
					Ok(None) => break,
					Err(err) => {
						tracing::debug!(%err, "source catalog ended");
						break;
					}
				},
				Some(result) = tasks.join_next() => match result {
					Ok(Ok(())) => {}
					Ok(Err(err)) => tracing::warn!(%err, "rung failed"),
					Err(err) => tracing::warn!(%err, "rung panicked"),
				}
			}
		}

		// Wind the rungs down. On a clean source end they are already finishing on
		// their own (the live path saw the source track end), so `shutdown` just
		// joins them. But `run` also breaks on a catalog-track error while the
		// source media and viewers are still live, and a rung task only self-ends on
		// source-media-end or broadcast-close, not catalog-end. Aborting rather than
		// awaiting keeps that case from hanging forever here.
		tasks.shutdown().await;

		derived.finish()?;
		output.finish();
		Ok(())
	}
}

#[cfg(test)]
mod tests {

	use super::*;

	/// A live source broadcast; the producers are kept so the tracks stay open
	/// for the duration of the test.
	struct Source {
		broadcast: moq_net::broadcast::Producer,
		catalog: moq_mux::catalog::Producer,
		_track: moq_net::track::Producer,
		/// The picture the catalog currently advertises, so a republish that only
		/// changes the description keeps it.
		size: (u32, u32),
	}

	impl Source {
		/// Publish the source video rendition at `width`x`height`, replacing
		/// whatever the catalog said before. An importer does exactly this when
		/// the picture changes size: the next keyframe's SPS is republished.
		fn resize(&mut self, width: u32, height: u32) {
			self.publish(width, height, None);
		}

		/// Republish the current rendition with new out-of-band parameter sets,
		/// which is a new decode stream at the same picture.
		fn describe(&mut self, description: Option<bytes::Bytes>) {
			let (width, height) = self.size;
			self.publish(width, height, description);
		}

		fn publish(&mut self, width: u32, height: u32, description: Option<bytes::Bytes>) {
			let mut video = hang::catalog::VideoConfig::new(hang::catalog::H264 {
				inline: true,
				profile: 0x42,
				constraints: 0,
				level: 30,
			});
			video.coded_width = Some(width);
			video.coded_height = Some(height);
			video.bitrate = Some(1_000_000);
			video.framerate = Some(30.0);
			video.description = description;
			self.size = (width, height);

			let mut guard = self.catalog.lock();
			guard.video = hang::catalog::Video::default();
			guard.video.insert("video", video).unwrap();
		}
	}

	/// A source broadcast carrying a catalog and an empty video track: enough to
	/// resolve a ladder, since no rung encodes until someone asks.
	fn source_catalog(width: u32, height: u32) -> Source {
		let mut broadcast = moq_net::broadcast::Info::default().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast).unwrap();
		let track = broadcast.create_track("video", hang::container::track_info()).unwrap();

		let mut source = Source {
			broadcast,
			catalog,
			_track: track,
			size: (width, height),
		};
		source.resize(width, height);
		source
	}

	/// Read derived catalog snapshots until one satisfies `ready`, so a test
	/// doesn't race the transcoder's own catalog writes.
	async fn await_catalog(
		catalogs: &mut moq_mux::catalog::hang::Consumer<()>,
		ready: impl Fn(&moq_mux::catalog::hang::Catalog) -> bool,
	) -> moq_mux::catalog::hang::Catalog {
		loop {
			let snapshot = catalogs.next().await.unwrap().unwrap();
			if ready(&snapshot) {
				return snapshot;
			}
		}
	}

	/// Subscribe to a derived track, waiting for the transcoder to register it.
	async fn subscribe(consumer: &moq_net::broadcast::Consumer, name: &str) -> moq_net::track::Subscriber {
		let track = loop {
			match consumer.track(name) {
				Ok(track) => break track,
				Err(moq_net::Error::NotFound) => tokio::task::yield_now().await,
				Err(err) => panic!("track {name}: {err}"),
			}
		};
		track.subscribe(None).await.unwrap()
	}

	/// H.264 NAL unit types in an Annex-B buffer, found via 3-byte start codes (a
	/// 4-byte `00 00 00 01` code contains `00 00 01` too, so this catches both).
	fn nal_types(annexb: &[u8]) -> Vec<u8> {
		let mut types = Vec::new();
		let mut i = 0;
		while i + 3 < annexb.len() {
			if annexb[i..i + 3] == [0, 0, 1] {
				types.push(annexb[i + 3] & 0x1f);
				i += 3;
			} else {
				i += 1;
			}
		}
		types
	}

	/// Write one gray 320x240 keyframe into `group`, so a fetch of it has something
	/// to decode while the group is still open.
	fn write_keyframe(group: &mut moq_net::group::Producer) {
		let mut encoder = moq_video::encode::Encoder::new(&{
			let mut config = moq_video::encode::Config::new(320, 240, 30);
			config.kind = moq_video::encode::Kind::Software;
			config
		})
		.unwrap();
		encoder.keyframe();
		let gray = vec![0x80u8; 320 * 240 * 4];
		for encoded in encoder.encode(&gray_frame(&gray, 0)).unwrap() {
			hang::container::Frame {
				timestamp: encoded.timestamp,
				payload: encoded.payload,
			}
			.write_to(group)
			.unwrap();
		}
	}

	/// Wrap a gray 320x240 RGBA buffer as a raw frame at `timestamp` microseconds.
	fn gray_frame(rgba: &[u8], timestamp: u64) -> moq_video::Frame {
		let surface = moq_video::Surface::rgba(rgba, moq_video::Size::new(320, 240)).unwrap();
		moq_video::Frame::new(surface, moq_net::Timestamp::from_micros(timestamp).unwrap())
	}

	/// Build a 320x240 avc3 source broadcast: a catalog plus a video track with
	/// `groups` groups of `frames` gray frames each, encoded with openh264.
	fn source_broadcast(groups: u64, frames: u64) -> Source {
		let mut broadcast = moq_net::broadcast::Info::default().produce();
		let mut catalog = moq_mux::catalog::Producer::new(&mut broadcast).unwrap();

		let mut video = hang::catalog::VideoConfig::new(hang::catalog::H264 {
			inline: true,
			profile: 0x42,
			constraints: 0,
			level: 30,
		});
		video.coded_width = Some(320);
		video.coded_height = Some(240);
		video.bitrate = Some(1_000_000);
		video.framerate = Some(30.0);
		catalog.lock().video.insert("video", video).unwrap();

		let info = hang::container::track_info();
		let mut track = broadcast.create_track("video", info).unwrap();

		let mut encoder = moq_video::encode::Encoder::new(&{
			let mut config = moq_video::encode::Config::new(320, 240, 30);
			config.kind = moq_video::encode::Kind::Software;
			config
		})
		.unwrap();
		let gray = vec![0x80u8; 320 * 240 * 4];

		for sequence in 0..groups {
			let mut group = track.create_group(sequence.into()).unwrap();
			for index in 0..frames {
				let timestamp = (sequence * frames + index) * 33_333;
				if index == 0 {
					encoder.keyframe();
				}
				for encoded in encoder.encode(&gray_frame(&gray, timestamp)).unwrap() {
					let frame = hang::container::Frame {
						timestamp: encoded.timestamp,
						payload: encoded.payload,
					};
					frame.write_to(&mut group).unwrap();
				}
			}
			group.finish().unwrap();
		}

		Source {
			broadcast,
			catalog,
			_track: track,
			size: (320, 240),
		}
	}

	/// A source like [`source_broadcast`], but the groups arrive over (paused)
	/// time instead of all at once, so several rungs can attach to the shared
	/// live feed before the first group exists. Returns the broadcast plus the
	/// producing task's handle (the track producer lives inside it).
	fn source_broadcast_live(groups: u64, frames: u64) -> (Source, tokio::task::JoinHandle<()>) {
		let mut broadcast = moq_net::broadcast::Info::default().produce();
		let mut catalog = moq_mux::catalog::Producer::new(&mut broadcast).unwrap();

		let mut video = hang::catalog::VideoConfig::new(hang::catalog::H264 {
			inline: true,
			profile: 0x42,
			constraints: 0,
			level: 30,
		});
		video.coded_width = Some(320);
		video.coded_height = Some(240);
		video.bitrate = Some(1_000_000);
		video.framerate = Some(30.0);
		catalog.lock().video.insert("video", video).unwrap();

		let info = hang::container::track_info();
		let mut track = broadcast.create_track("video", info).unwrap();

		let source = Source {
			broadcast,
			catalog,
			// The producing task owns the real track producer; park a clone so
			// the struct shape matches `source_broadcast`.
			_track: track.clone(),
			size: (320, 240),
		};

		let task = tokio::spawn(async move {
			let mut encoder = moq_video::encode::Encoder::new(&{
				let mut config = moq_video::encode::Config::new(320, 240, 30);
				config.kind = moq_video::encode::Kind::Software;
				config
			})
			.unwrap();
			let gray = vec![0x80u8; 320 * 240 * 4];

			for sequence in 0..groups {
				// Paces the source: a real sleep, since the rungs encode off the
				// executor and cannot be sequenced by paused-time idle detection.
				// Also the window the subscribers attach in, before group 0.
				tokio::time::sleep(std::time::Duration::from_millis(100)).await;
				let mut group = track.create_group(sequence.into()).unwrap();
				for index in 0..frames {
					let timestamp = (sequence * frames + index) * 33_333;
					if index == 0 {
						encoder.keyframe();
					}
					for encoded in encoder.encode(&gray_frame(&gray, timestamp)).unwrap() {
						let frame = hang::container::Frame {
							timestamp: encoded.timestamp,
							payload: encoded.payload,
						};
						frame.write_to(&mut group).unwrap();
					}
				}
				group.finish().unwrap();
			}
			// Keep the track open until aborted, like a live source.
			std::future::pending::<()>().await;
		});

		(source, task)
	}

	/// Two rungs subscribed at once ride one shared live decode (the feed):
	/// both must produce complete groups mirroring the source sequences.
	#[tokio::test]
	async fn live_multi_rung() {
		// Real time on purpose, unlike most timed tests here. The rungs encode on
		// their own threads (`encode::Sink`), so a rung waiting on one looks idle
		// to tokio and `pause()` auto-advances the source's sleep while the encode
		// is still in flight. The source then outruns the feed's bounded broadcast
		// and every rung sees `Lagged` instead of its frames. Real sleeps pace the
		// source against the encoders the way a live source does.
		let (source, producer_task) = source_broadcast_live(3, 5);
		let config = Config {
			rungs: vec![Rung::new(120, 100_000), Rung::new(60, 50_000)],
			encoder: moq_video::encode::Kind::Software,
			decoder: moq_video::decode::Kind::Software,
			source: None,
			..Default::default()
		};

		let output = moq_net::broadcast::Info::default().produce();
		let consumer = output.consume();
		let transcoder = tokio::spawn(run(source.broadcast.consume(), output, config));

		// Attach both rungs before the first source group exists (paused time:
		// the producer's sleep only fires once every rung is parked on the feed).
		let mut subscribers = Vec::new();
		for name in ["video/120p", "video/60p"] {
			let track = loop {
				match consumer.track(name) {
					Ok(track) => break track,
					Err(moq_net::Error::NotFound) => tokio::task::yield_now().await,
					Err(err) => panic!("rung track {name}: {err}"),
				}
			};
			subscribers.push((name, track.subscribe(None).await.unwrap()));
		}

		// Every rung receives a complete group with all 5 source frames.
		for (name, subscriber) in &mut subscribers {
			let mut group = subscriber.next_group().await.unwrap().unwrap();
			let payload = group.read_frame().await.unwrap().unwrap();
			let frame = hang::container::Frame::decode(payload.payload).unwrap();
			assert!(
				frame.payload.starts_with(&[0, 0, 0, 1]) || frame.payload.starts_with(&[0, 0, 1]),
				"{name} output is not Annex-B"
			);
			let total = group.finished().await.unwrap();
			assert_eq!(total, 5, "{name} dropped frames");
		}

		producer_task.abort();
		transcoder.abort();
	}

	/// The multi-rung live path on real hardware: one shared NVDEC session
	/// decodes the source, the GPU box filter resizes per rung, and each rung's
	/// NVENC session encodes the CUDA frame in place. Skips without a GPU.
	#[cfg_attr(
		target_os = "windows",
		ignore = "explicit live-DXVA GPU probe; VideoProcessorBlt can hang on affected drivers"
	)]
	#[tokio::test]
	async fn live_multi_rung_hardware() {
		if !hardware_available() {
			eprintln!("skipping: no hardware decoder + encoder available");
			return;
		}
		// Real time on purpose, unlike most timed tests here. The rungs encode on
		// their own threads (`encode::Sink`), so a rung waiting on one looks idle
		// to tokio and `pause()` auto-advances the source's sleep while the encode
		// is still in flight. The source then outruns the feed's bounded broadcast
		// and every rung sees `Lagged` instead of its frames. Real sleeps pace the
		// source against the encoders the way a live source does.
		let (source, producer_task) = source_broadcast_live(3, 5);
		// 180p and 120p: NVENC rejects tiny frames (80x60 is below its minimum
		// encode resolution), so the hardware ladder stays a bit larger than the
		// software test's.
		let mut config = Config {
			rungs: vec![Rung::new(180, 200_000), Rung::new(120, 100_000)],
			encoder: moq_video::encode::Kind::Hardware,
			decoder: moq_video::decode::Kind::Hardware,
			source: None,
			..Default::default()
		};
		config.resize.acceleration = moq_video::resize::Acceleration::Gpu;

		let output = moq_net::broadcast::Info::default().produce();
		let consumer = output.consume();
		let transcoder = tokio::spawn(run(source.broadcast.consume(), output, config));

		let mut subscribers = Vec::new();
		for name in ["video/180p", "video/120p"] {
			let track = loop {
				match consumer.track(name) {
					Ok(track) => break track,
					Err(moq_net::Error::NotFound) => tokio::task::yield_now().await,
					Err(err) => panic!("rung track {name}: {err}"),
				}
			};
			subscribers.push((name, track.subscribe(None).await.unwrap()));
		}

		for (name, subscriber) in &mut subscribers {
			let mut group = subscriber.next_group().await.unwrap().unwrap();
			let payload = group.read_frame().await.unwrap().unwrap();
			let frame = hang::container::Frame::decode(payload.payload).unwrap();
			assert!(
				frame.payload.starts_with(&[0, 0, 0, 1]) || frame.payload.starts_with(&[0, 0, 1]),
				"{name} output is not Annex-B"
			);
			let total = group.finished().await.unwrap();
			assert_eq!(total, 5, "{name} dropped frames");
		}

		producer_task.abort();
		transcoder.abort();
	}

	/// Whether a hardware decoder AND encoder are usable here (e.g. a Linux box
	/// with the NVIDIA driver). Probed through the public API so the hardware
	/// test skips cleanly on GPU-less CI.
	fn hardware_available() -> bool {
		let mut encode = moq_video::encode::Config::new(160, 120, 30);
		encode.kind = moq_video::encode::Kind::Hardware;
		if moq_video::encode::Encoder::new(&encode).is_err() {
			return false;
		}

		let video = hang::catalog::VideoConfig::new(hang::catalog::H264 {
			inline: true,
			profile: 0x42,
			constraints: 0,
			level: 30,
		});
		let mut decode = moq_video::decode::Config::new();
		decode.kind = moq_video::decode::Kind::Hardware;
		moq_video::decode::Decoder::new(&video, &decode).is_ok()
	}

	/// The GPU pipeline end to end: hardware decode (NVDEC, scaling in the
	/// decoder) into hardware encode (NVENC, consuming the CUDA frame in place).
	/// Skips on machines without both; on a Linux + NVIDIA box this is the
	/// zero-copy transcode path under the real broadcast plumbing.
	#[cfg_attr(
		target_os = "windows",
		ignore = "explicit live-DXVA GPU probe; VideoProcessorBlt can hang on affected drivers"
	)]
	#[tokio::test]
	async fn end_to_end_hardware() {
		if !hardware_available() {
			eprintln!("skipping: no hardware decoder + encoder available");
			return;
		}

		let source = source_broadcast(2, 5);
		let mut config = Config {
			rungs: vec![Rung::new(120, 100_000)],
			encoder: moq_video::encode::Kind::Hardware,
			decoder: moq_video::decode::Kind::Hardware,
			source: None,
			..Default::default()
		};
		config.resize.acceleration = moq_video::resize::Acceleration::Gpu;

		let output = moq_net::broadcast::Info::default().produce();
		let consumer = output.consume();
		let transcoder = tokio::spawn(run(source.broadcast.consume(), output, config));

		// Fetch a specific group: runs a one-shot pipeline to completion, so all
		// 5 source frames must come through the GPU path.
		let track = loop {
			match consumer.track("video/120p") {
				Ok(track) => break track,
				Err(moq_net::Error::NotFound) => tokio::task::yield_now().await,
				Err(err) => panic!("rung track: {err}"),
			}
		};
		let mut fetched = track.fetch_group(0, None).await.unwrap();
		let payload = fetched.read_frame().await.unwrap().unwrap();
		let frame = hang::container::Frame::decode(payload.payload).unwrap();
		assert!(
			frame.payload.starts_with(&[0, 0, 0, 1]) || frame.payload.starts_with(&[0, 0, 1]),
			"hardware rung output is not Annex-B"
		);
		let total = fetched.finished().await.unwrap();
		assert_eq!(total, 5, "hardware transcode dropped frames");

		transcoder.abort();
	}

	#[tokio::test]
	async fn end_to_end() {
		let source = source_broadcast(2, 5);

		let config = Config {
			rungs: vec![Rung::new(120, 100_000)],
			encoder: moq_video::encode::Kind::Software,
			decoder: moq_video::decode::Kind::Software,
			source: Some(moq_net::PathRelativeOwned::from(".".to_string())),
			..Default::default()
		};

		let output = moq_net::broadcast::Info::default().produce();
		let consumer = output.consume();
		let transcoder = tokio::spawn(run(source.broadcast.consume(), output, config));

		// The derivative catalog appears before anything is encoded, with the
		// rung sized against the source and the passthrough reference. Yield
		// until the spawned transcoder has run its synchronous prologue (the
		// catalog tracks and dynamic handler register before its first await).
		let track = loop {
			match consumer.track(hang::Catalog::DEFAULT_NAME) {
				Ok(track) => break track,
				Err(moq_net::Error::NotFound) => tokio::task::yield_now().await,
				Err(err) => panic!("catalog track: {err}"),
			}
		};
		let track = track.subscribe(None).await.unwrap();
		let mut catalogs = moq_mux::catalog::hang::Consumer::<()>::new(track);
		// The catalog track exists from the start but may open empty; the rung
		// appears once the transcoder has read the source catalog.
		let derived = loop {
			let snapshot = catalogs.next().await.unwrap().unwrap();
			if snapshot.video.renditions.contains_key("video/120p") {
				break snapshot;
			}
		};

		let rung = derived.video.renditions.get("video/120p").expect("rung missing");
		assert_eq!(rung.coded_width, Some(160));
		assert_eq!(rung.coded_height, Some(120));
		assert_eq!(rung.bitrate, Some(100_000));
		assert!(rung.codec.to_string().starts_with("avc3."));

		let passthrough = derived.video.renditions.get("video").expect("passthrough missing");
		assert_eq!(passthrough.broadcast.as_ref().map(|b| b.as_ref()), Some("."));

		// Subscribing to the rung starts the live loop, which mirrors source
		// group sequences 1:1.
		let mut subscriber = consumer.track("video/120p").unwrap().subscribe(None).await.unwrap();
		let mut group = subscriber.next_group().await.unwrap().unwrap();
		assert!(group.sequence <= 1, "unexpected sequence {}", group.sequence);
		let payload = group.read_frame().await.unwrap().unwrap();
		let frame = hang::container::Frame::decode(payload.payload).unwrap();
		assert!(
			frame.payload.starts_with(&[0, 0, 0, 1]) || frame.payload.starts_with(&[0, 0, 1]),
			"rung output is not Annex-B"
		);

		// Fetching a specific past group transcodes source group 0 on demand.
		let mut fetched = consumer
			.track("video/120p")
			.unwrap()
			.fetch_group(0, None)
			.await
			.unwrap();
		let mut timestamps = Vec::new();
		let mut first_payload = None;
		while let Some(payload) = fetched.read_frame().await.unwrap() {
			let frame = hang::container::Frame::decode(payload.payload).unwrap();
			assert!(!frame.payload.is_empty());
			timestamps.push(frame.timestamp.as_micros());
			first_payload = first_payload.or(Some(frame.payload));
		}

		// The group has to open on an IDR, or a subscriber starting here decodes
		// nothing: the rung asks its encoder for one at every group boundary. An
		// Annex-B start code alone doesn't prove it, since a delta frame has one too,
		// so check the NAL types: SPS (7) and PPS (8) inline ahead of an IDR (5),
		// which is what avc3 promises.
		let types = nal_types(&first_payload.expect("the group had no frames"));
		assert!(types.contains(&7), "group does not open with an SPS: {types:?}");
		assert!(types.contains(&8), "group does not open with a PPS: {types:?}");
		assert!(types.contains(&5), "group does not open with an IDR: {types:?}");
		// Each output frame keeps the presentation time of the source frame it was
		// transcoded from, including the tail the encoder drains at the end of the
		// group. Collapsing them onto one instant would stall playback here.
		assert_eq!(timestamps, (0..5).map(|i| i * 33_333).collect::<Vec<u128>>());
		// The fetched group is complete: the source group had 5 frames, and a
		// finished transcode carries them all through.
		let total = fetched.finished().await.unwrap();
		assert_eq!(total, 5);

		transcoder.abort();
	}

	/// The whole point of [`active`]: a caller metering or pricing the work is
	/// handed the ladder, sees each rendition start and stop, and can bill the
	/// seconds in between. Nothing else distinguishes a transcoder publishing a
	/// catalog from one saturating a GPU.
	#[tokio::test]
	async fn reports_active_rungs() {
		let source = source_broadcast(2, 5);

		let config = Config {
			rungs: vec![Rung::new(120, 100_000)],
			encoder: moq_video::encode::Kind::Software,
			decoder: moq_video::decode::Kind::Software,
			source: None,
			..Default::default()
		};

		let output = moq_net::broadcast::Info::default().produce();
		let consumer = output.consume();
		let transcoder = Transcoder::new(source.broadcast.consume(), output, config).unwrap();
		let mut active = transcoder.active();
		let driver = tokio::spawn(transcoder.run());

		// The ladder arrives once resolved, before anyone has asked for a rung.
		let update = active.next().await.unwrap();
		let rendition = update.rendition;
		assert_eq!(rendition.name(), "video/120p");
		assert_eq!(rendition.size().height, 120);
		assert_eq!(rendition.bitrate(), 100_000);
		assert!(!update.encoding, "encoding before anyone asked");
		assert_eq!(rendition.frames(), 0);

		let mut subscriber = consumer.track("video/120p").unwrap().subscribe(None).await.unwrap();
		let update = active.next().await.unwrap();
		assert_eq!(update.rendition.name(), "video/120p");
		assert!(update.encoding);

		// Real frames, so the counters are counting encoding rather than intent.
		let mut group = subscriber.next_group().await.unwrap().unwrap();
		group.read_frame().await.unwrap().unwrap();
		assert!(rendition.frames() > 0);
		assert!(rendition.bytes() > 0);

		// Demand gone: the rung stops encoding and the cursor reports the edge.
		drop(group);
		drop(subscriber);
		let update = active.next().await.unwrap();
		assert_eq!(update.rendition.name(), "video/120p");
		assert!(!update.encoding);

		// The rendition is idle, but the totals survive for the final bill.
		assert!(rendition.frames() > 0);
		assert!(rendition.bytes() > 0);

		driver.abort();
	}

	/// A source that resizes mid-stream takes the ladder with it.
	///
	/// `moq_video::encode::publish_capture` opens its source twice by design (once
	/// to probe the mode, once when the first subscriber arrives), and a window
	/// A source that changes aspect ratio keeps every rung height while moving
	/// every rung width, so a rung retires and its replacement serves the same
	/// height. That replacement must not reuse the retired track's name: a clean
	/// end is terminal, and a relay keeps the finished logical track (only an
	/// *aborted* one is dropped and requested again), so a subscriber asking for
	/// the old name would get its EOF forever and never reach the transcoder.
	#[tokio::test]
	async fn a_resized_rung_takes_a_fresh_name() {
		let mut source = source_catalog(640, 360);

		let config = Config {
			rungs: vec![Rung::new(120, 100_000)],
			encoder: moq_video::encode::Kind::Software,
			decoder: moq_video::decode::Kind::Software,
			source: None,
			..Default::default()
		};

		let output = moq_net::broadcast::Info::default().produce();
		let consumer = output.consume();
		let transcoder = tokio::spawn(run(source.broadcast.consume(), output, config));

		let track = loop {
			match consumer.track(hang::Catalog::DEFAULT_NAME) {
				Ok(track) => break track,
				Err(moq_net::Error::NotFound) => tokio::task::yield_now().await,
				Err(err) => panic!("catalog track: {err}"),
			}
		};
		let mut catalogs = moq_mux::catalog::hang::Consumer::<()>::new(track.subscribe(None).await.unwrap());

		let derived = await_catalog(&mut catalogs, |snapshot| {
			snapshot.video.renditions.contains_key("video/120p")
		})
		.await;
		assert_eq!(
			derived.video.renditions.get("video/120p").and_then(|v| v.coded_width),
			Some(212),
			"640x360 should give 120p a 212 wide picture"
		);
		let mut retired = subscribe(&consumer, "video/120p").await;

		// Same height, wider pixels: 120p stays 120 tall and goes from 212 to 160.
		source.resize(480, 360);

		let derived = await_catalog(&mut catalogs, |snapshot| {
			!snapshot.video.renditions.contains_key("video/120p")
		})
		.await;
		let replacement = derived
			.video
			.renditions
			.get("video/120p.2")
			.expect("the resized rung was not republished under a fresh name");
		assert_eq!(replacement.coded_width, Some(160));
		assert_eq!(replacement.coded_height, Some(120));

		// The retired name ends cleanly, and the replacement is a track the
		// transcoder has never finished, so it serves.
		let ended = tokio::time::timeout(std::time::Duration::from_secs(5), retired.next_group())
			.await
			.expect("the retired rung never ended its track")
			.expect("the retired rung aborted instead of finishing");
		assert!(ended.is_none(), "expected a clean end, got a group");

		// The replacement is a track the transcoder has never finished, so it serves.
		subscribe(&consumer, "video/120p.2").await;

		transcoder.abort();
	}

	/// capture derives its geometry from the window on each open, so the picture a
	/// transcoder advertises a ladder for is routinely not the one it ends up
	/// carrying. The rungs that no longer fit have to retire, the ones that still
	/// do have to keep serving, and the passthrough entry has to follow.
	#[tokio::test]
	async fn ladder_follows_a_source_resize() {
		let mut source = source_catalog(640, 360);

		let config = Config {
			// 360p is admitted at 640x360 only because its bitrate undercuts the
			// source's; 240p and 120p fit outright.
			rungs: vec![
				Rung::new(360, 900_000),
				Rung::new(240, 300_000),
				Rung::new(120, 100_000),
			],
			encoder: moq_video::encode::Kind::Software,
			decoder: moq_video::decode::Kind::Software,
			source: Some(moq_net::PathRelativeOwned::from(".".to_string())),
			..Default::default()
		};

		let output = moq_net::broadcast::Info::default().produce();
		let consumer = output.consume();
		let transcoder = tokio::spawn(run(source.broadcast.consume(), output, config));

		let track = loop {
			match consumer.track(hang::Catalog::DEFAULT_NAME) {
				Ok(track) => break track,
				Err(moq_net::Error::NotFound) => tokio::task::yield_now().await,
				Err(err) => panic!("catalog track: {err}"),
			}
		};
		let mut catalogs = moq_mux::catalog::hang::Consumer::<()>::new(track.subscribe(None).await.unwrap());

		let derived = await_catalog(&mut catalogs, |snapshot| {
			snapshot.video.renditions.contains_key("video/360p")
		})
		.await;
		assert!(derived.video.renditions.contains_key("video/240p"));
		assert!(derived.video.renditions.contains_key("video/120p"));
		assert_eq!(
			derived.video.renditions.get("video").and_then(|v| v.coded_width),
			Some(640),
			"the passthrough entry should describe the source"
		);

		// Two live subscribers: one on a rung the smaller picture has no room for,
		// one on a rung that survives it unchanged.
		let mut retired = subscribe(&consumer, "video/240p").await;
		let mut kept = subscribe(&consumer, "video/120p").await;

		// 320x180 keeps the source aspect ratio, so 120p stays 212x120 while 360p
		// and 240p are now taller than the source.
		source.resize(320, 180);

		let derived = await_catalog(&mut catalogs, |snapshot| {
			!snapshot.video.renditions.contains_key("video/360p")
		})
		.await;
		assert!(
			!derived.video.renditions.contains_key("video/240p"),
			"240p outlived the resize"
		);
		let rung = derived
			.video
			.renditions
			.get("video/120p")
			.expect("120p was retired too");
		assert_eq!(rung.coded_width, Some(212));
		assert_eq!(rung.coded_height, Some(120));
		assert_eq!(
			derived.video.renditions.get("video").and_then(|v| v.coded_width),
			Some(320),
			"the passthrough entry should follow the source"
		);

		// The retired rung ends its track, so a subscriber reselects the way it
		// would on any other rendition going away, rather than stalling or seeing
		// an abort it would read as a failure.
		let ended = tokio::time::timeout(std::time::Duration::from_secs(5), retired.next_group())
			.await
			.expect("the retired rung never ended its track")
			.expect("the retired rung aborted instead of finishing");
		assert!(ended.is_none(), "expected a clean end, got a group");

		// The rung the new picture still fits keeps serving: its subscriber sees
		// nothing at all, since the source has no media.
		assert!(
			tokio::time::timeout(std::time::Duration::from_millis(100), kept.next_group())
				.await
				.is_err(),
			"a rung that still fits was retired anyway"
		);

		transcoder.abort();
	}

	/// Retiring a rung stops taking new fetches, but one already in flight has to
	/// run to a clean end. Two things can cut it short: dropping the handler while
	/// its request is still queued, and finishing the track at a live edge that
	/// sits at or below the group it is about to claim (sequence 0, on a rung that
	/// only ever served fetches).
	///
	/// `Consumer::fetch_group` resolves as soon as the attempt is registered, well
	/// before `GroupRequest::accept` creates the group, so the retirement below
	/// really does land while the fetch is still opening its decoder. Which side
	/// of `accept` it lands on is up to the scheduler, so this catches the second
	/// case some of the time rather than every time. It caught it on a loaded CI
	/// runner; it has never lost that race on a developer machine, which is also
	/// why forcing it deterministically here needs a hook the rung does not have.
	#[tokio::test]
	async fn retirement_finishes_an_in_flight_fetch() {
		let mut source = source_catalog(320, 240);
		let mut group = source._track.create_group(0u64.into()).unwrap();
		write_keyframe(&mut group);

		let config = Config {
			rungs: vec![Rung::new(120, 100_000)],
			encoder: moq_video::encode::Kind::Software,
			decoder: moq_video::decode::Kind::Software,
			source: None,
			..Default::default()
		};
		let output = moq_net::broadcast::Info::default().produce();
		let consumer = output.consume();
		let transcoder = tokio::spawn(run(source.broadcast.consume(), output, config));

		let catalog = loop {
			match consumer.track(hang::Catalog::DEFAULT_NAME) {
				Ok(track) => break track,
				Err(moq_net::Error::NotFound) => tokio::task::yield_now().await,
				Err(err) => panic!("catalog track: {err}"),
			}
		};
		let mut catalogs = moq_mux::catalog::hang::Consumer::<()>::new(catalog.subscribe(None).await.unwrap());
		await_catalog(&mut catalogs, |snapshot| {
			snapshot.video.renditions.contains_key("video/120p")
		})
		.await;

		// Resolving the info waits for the transcoder to accept the track, so the
		// resize below cannot land first and retire the rung out from under a
		// request that was never served. That is correct behavior (the rendition is
		// gone), just not what this test is about.
		let rung = consumer.track("video/120p").unwrap();
		rung.info().await.unwrap();
		let mut fetched = rung.fetch_group(0, None).await.unwrap();

		// The fetch is queued against a source group that is still open, so retiring
		// now has to leave it running until that group ends.
		source.resize(160, 90);
		tokio::time::timeout(
			std::time::Duration::from_secs(5),
			await_catalog(&mut catalogs, |snapshot| {
				!snapshot.video.renditions.contains_key("video/120p")
			}),
		)
		.await
		.expect("the ladder never retired the rung");
		group.finish().unwrap();

		let finished = tokio::time::timeout(std::time::Duration::from_secs(5), async {
			while fetched.read_frame().await?.is_some() {}
			fetched.finished().await
		})
		.await
		.expect("the accepted fetch never finished");
		assert!(finished.is_ok(), "retirement aborted the accepted group: {finished:?}");

		transcoder.abort();
	}

	/// A source whose codec description changes rebuilds the shared decode, so
	/// every rung retires with it. The picture may not have moved at all, so shape
	/// alone would hand the replacements the names that just ended. They have to be
	/// fresh names for the same reason a resized rung's is.
	#[tokio::test]
	async fn a_rebuilt_decode_renames_every_rung() {
		let mut source = source_catalog(320, 240);

		let config = Config {
			rungs: vec![Rung::new(120, 100_000)],
			encoder: moq_video::encode::Kind::Software,
			decoder: moq_video::decode::Kind::Software,
			source: None,
			..Default::default()
		};

		let output = moq_net::broadcast::Info::default().produce();
		let consumer = output.consume();
		let transcoder = tokio::spawn(run(source.broadcast.consume(), output, config));

		let track = loop {
			match consumer.track(hang::Catalog::DEFAULT_NAME) {
				Ok(track) => break track,
				Err(moq_net::Error::NotFound) => tokio::task::yield_now().await,
				Err(err) => panic!("catalog track: {err}"),
			}
		};
		let mut catalogs = moq_mux::catalog::hang::Consumer::<()>::new(track.subscribe(None).await.unwrap());
		await_catalog(&mut catalogs, |snapshot| {
			snapshot.video.renditions.contains_key("video/120p")
		})
		.await;
		let mut retired = subscribe(&consumer, "video/120p").await;

		// Same picture, new out-of-band parameter sets: the rungs still resolve to
		// 160x120, but their decoder is rebuilt so none of them survives.
		source.describe(Some(bytes::Bytes::from_static(&[0x01, 0x42, 0x00, 0x1e])));

		let derived = tokio::time::timeout(
			std::time::Duration::from_secs(5),
			await_catalog(&mut catalogs, |snapshot| {
				snapshot.video.renditions.contains_key("video/120p.2")
			}),
		)
		.await
		.expect("the rebuilt decode kept the retired rung name");
		assert!(
			!derived.video.renditions.contains_key("video/120p"),
			"the retired name is still advertised"
		);
		assert_eq!(
			derived.video.renditions.get("video/120p.2").and_then(|v| v.coded_width),
			Some(160),
			"the replacement should serve the same picture under a new name"
		);

		let ended = tokio::time::timeout(std::time::Duration::from_secs(5), retired.next_group())
			.await
			.expect("the retired rung never ended its track")
			.expect("the retired rung aborted instead of finishing");
		assert!(ended.is_none(), "expected a clean end, got a group");
		subscribe(&consumer, "video/120p.2").await;

		transcoder.abort();
	}

	/// `run` must terminate (not hang in its shutdown drain) when the source
	/// broadcast goes away, even with a rung task that was never subscribed.
	#[tokio::test]
	async fn shuts_down_on_source_end() {
		let source = source_broadcast(1, 3);

		let config = Config {
			rungs: vec![Rung::new(120, 100_000)],
			encoder: moq_video::encode::Kind::Software,
			decoder: moq_video::decode::Kind::Software,
			source: None,
			..Default::default()
		};

		let output = moq_net::broadcast::Info::default().produce();
		let consumer = output.consume();
		let transcoder = tokio::spawn(run(source.broadcast.consume(), output, config));

		// Wait until the derivative catalog is up, so the transcoder is past
		// startup and into its serve loop.
		let track = loop {
			match consumer.track(hang::Catalog::DEFAULT_NAME) {
				Ok(track) => break track,
				Err(moq_net::Error::NotFound) => tokio::task::yield_now().await,
				Err(err) => panic!("catalog track: {err}"),
			}
		};
		let mut catalogs = moq_mux::catalog::hang::Consumer::<()>::new(track.subscribe(None).await.unwrap());
		catalogs.next().await.unwrap().unwrap();

		// Drop the source: the catalog track ends and the broadcast closes, so
		// `run` should observe the end and return rather than block in the drain.
		drop(source);

		let result = tokio::time::timeout(std::time::Duration::from_secs(5), transcoder).await;
		result.expect("run did not shut down within 5s").unwrap().unwrap();
	}
}
