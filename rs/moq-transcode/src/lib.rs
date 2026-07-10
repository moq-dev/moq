//! Just-in-time live transcoding for hang broadcasts.
//!
//! [`run`] consumes a source broadcast and fills a derivative broadcast: a
//! catalog advertising lower renditions (rungs) of the source video plus
//! references back to the source renditions, and one output video track per
//! rung. The catalog is published immediately and deterministically (codec
//! strings are computed from the ladder, not the bitstream), but nothing is
//! encoded until a subscriber actually asks:
//!
//! - Subscribing to a rung subscribes to the source track and transcodes live,
//!   group for group, stopping when the last subscriber leaves.
//! - Fetching a specific group fetches that same group from the source and
//!   transcodes just that group. Output groups mirror source sequence numbers
//!   1:1, so group N of every rung is the same content as source group N.
//!
//! The codec work is `moq-video`: hardware where available (NVENC on Linux,
//! VideoToolbox on macOS, Media Foundation on Windows) with openh264 as the
//! H.264 software fallback. Scaling runs on the CPU; the GPU-resident
//! NVDEC -> scale -> NVENC pipeline is tracked in moq-dev/moq#1837.

mod catalog;
mod config;
mod error;
mod rung;
mod scale;

pub use config::{Config, Rung};
pub use error::Error;

/// Transcode `source` into `output` until the source broadcast ends.
///
/// Reads the source catalog, publishes the derivative catalog (rungs strictly
/// below the source, plus source renditions referenced via [`Config::source`]),
/// and serves each rung just-in-time: a rung track only materializes when a
/// consumer asks for it, and only encodes while consumed. Where `output` is
/// announced (and how its path relates to the source) is the caller's business.
///
/// The catalog tracks and the on-demand rung handler are registered
/// synchronously, before the first `await`, so a consumer may race the rest of
/// the setup safely: call `run` before announcing `output`.
pub async fn run(
	source: moq_net::broadcast::Consumer,
	mut output: moq_net::broadcast::Producer,
	config: Config,
) -> Result<(), Error> {
	// The catalog starts empty and fills in below, exactly like a media
	// importer that hasn't seen parameter sets yet.
	let mut derived = moq_mux::catalog::Producer::new(&mut output)?;
	// Consumers asking for a rung before (or after) it exists queue here.
	let mut dynamic = output.dynamic();

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
	let rungs = catalog::resolve_rungs(&config.rungs, &source_name, &source_config)?;
	tracing::info!(source = %source_name, rungs = rungs.len(), "transcoding");

	// Publish the derivative catalog before any encoder exists, so subscribers
	// can pick a rung immediately.
	let entries: Vec<_> = rungs
		.iter()
		.map(|rung| (rung.name.clone(), catalog::rung_entry(rung, &source_config)))
		.collect();
	{
		let mut guard = derived.lock();
		catalog::populate(&mut guard, &snapshot, &entries, config.source.as_ref())?;
	}

	// Serve rung requests and follow source catalog updates until the source
	// ends. The rung set is fixed at startup: a source that changes resolution
	// mid-stream keeps the ladder it started with, but the passthrough entries
	// track the source.
	let mut tasks = tokio::task::JoinSet::new();
	loop {
		tokio::select! {
			request = dynamic.requested_track() => {
				// Err means the broadcast closed; nothing left to serve.
				let Ok(request) = request else { break };
				match rungs.iter().find(|rung| rung.name == request.name()) {
					Some(info) => {
						let rung = rung::Rung {
							source: source.track(&source_name)?,
							broadcast: source.clone(),
							config: source_config.clone(),
							encoder: config.encoder.clone(),
							decoder: config.decoder.clone(),
							info: info.clone(),
						};
						tasks.spawn(rung::serve(rung, request));
					}
					None => request.reject(moq_net::Error::NotFound),
				}
			},
			update = catalogs.next() => match update {
				Ok(Some(snapshot)) => {
					let mut guard = derived.lock();
					catalog::populate(&mut guard, &snapshot, &entries, config.source.as_ref())?;
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
	output.close();
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	/// A live source broadcast; the producers are kept so the tracks stay open
	/// for the duration of the test.
	struct Source {
		broadcast: moq_net::broadcast::Producer,
		_catalog: moq_mux::catalog::Producer,
		_track: moq_net::track::Producer,
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

		let info = moq_net::track::Info::default().with_timescale(hang::container::TIMESCALE);
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
				for payload in encoder.encode_rgba(&gray, 320, 240, index == 0).unwrap() {
					let frame = hang::container::Frame {
						timestamp: moq_net::Timestamp::from_micros(timestamp).unwrap(),
						payload,
					};
					frame.encode(&mut group).unwrap();
				}
			}
			group.finish().unwrap();
		}

		Source {
			broadcast,
			_catalog: catalog,
			_track: track,
		}
	}

	#[tokio::test]
	async fn end_to_end() {
		let source = source_broadcast(2, 5);

		let config = Config {
			rungs: vec![Rung::new(120, 100_000)],
			encoder: moq_video::encode::Kind::Software,
			decoder: moq_video::decode::Kind::Software,
			source: Some(moq_net::PathRelativeOwned::from("..".to_string())),
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
		assert_eq!(passthrough.broadcast.as_ref().map(|b| b.as_ref()), Some(".."));

		// Subscribing to the rung starts the live loop, which mirrors source
		// group sequences 1:1.
		let mut subscriber = consumer.track("video/120p").unwrap().subscribe(None).await.unwrap();
		let mut group = subscriber.next_group().await.unwrap().unwrap();
		assert!(group.sequence <= 1, "unexpected sequence {}", group.sequence);
		let payload = group.read_frame().await.unwrap().unwrap();
		let frame = hang::container::Frame::decode(payload).unwrap();
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
		let payload = fetched.read_frame().await.unwrap().unwrap();
		let frame = hang::container::Frame::decode(payload).unwrap();
		assert!(!frame.payload.is_empty());
		// The fetched group is complete: the source group had 5 frames, and a
		// finished transcode carries them all through.
		let total = fetched.finished().await.unwrap();
		assert_eq!(total, 5);

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
