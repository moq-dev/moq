//! Live sources: what each [`Source`] of a collection shows and plays right now.
//!
//! [`Sources::sync`] keeps one [`Feed`] per source and hands back a [`Driver`]
//! for each one it opens. The caller runs the drivers wherever suits it (a
//! capture driver is `!Send` on macOS); a driver stops once its feed is
//! dropped, which happens when the source is removed or edited.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use hang::catalog::Reference;
use moq_audio::bus::{Bus, Strip};
use moq_mux::catalog::Stream;

use crate::Error;
use crate::collection::{Collection, Source};

/// A source's newest picture, waiting for the compositor to draw it.
pub(crate) enum Picture {
	/// A captured or decoded video frame.
	Frame(moq_video::Frame),
	/// Tightly packed RGBA with straight alpha, such as a still image.
	Rgba { size: moq_video::Size, pixels: Vec<u8> },
}

/// The one picture a source holds for the compositor. A newer picture replaces
/// an undrawn one, so a slow canvas skips pictures instead of queuing them.
type Mailbox = Arc<Mutex<Option<Picture>>>;

/// One instance of a broadcast being decoded.
type Playing<'a> = Pin<Box<dyn Future<Output = Result<(), Error>> + 'a>>;

/// What a running source is doing.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Status {
	/// Opening its device, file, or subscription.
	Starting,
	/// Producing pictures or sound.
	Live,
	/// Waiting for its broadcast to be announced.
	Offline,
	/// Stopped, with the reason. A broadcast source keeps waiting for the
	/// broadcast to come back; every other source stays stopped until edited.
	Failed(String),
}

/// What every source shares.
#[derive(Clone)]
pub struct Context {
	/// The program audio that microphones, system audio, and broadcasts feed.
	pub bus: Bus,
	/// Where broadcast sources are announced: the studio's relay connection.
	pub origin: moq_net::origin::Consumer,
	/// What a relative image path is relative to: the scene file's directory.
	pub dir: PathBuf,
	/// What cameras, screens, and broadcasts deliver video as:
	/// `moq_video::render::output` of the compositor's device.
	pub output: moq_video::Output,
}

/// The running sources of a collection.
pub struct Sources {
	context: Context,
	feeds: BTreeMap<String, Feed>,
}

impl Sources {
	/// No sources running yet; [`sync`](Self::sync) starts them.
	pub fn new(context: Context) -> Self {
		Self {
			context,
			feeds: BTreeMap::new(),
		}
	}

	/// What every source shares.
	pub fn context(&self) -> &Context {
		&self.context
	}

	/// Run exactly the sources `collection` has: start new and edited ones and
	/// stop removed and edited ones. Returns a driver for each source started,
	/// which does nothing until run.
	///
	/// Also applies the collection's gain and mute to every audio strip, which
	/// never restarts a source. Panics on a gain [`Collection::validate`] refuses.
	pub fn sync(&mut self, collection: &Collection) -> Vec<Driver> {
		self.feeds
			.retain(|name, feed| collection.sources.get(name) == Some(&feed.source));

		let mut drivers = Vec::new();
		for (name, source) in &collection.sources {
			if !self.feeds.contains_key(name) {
				let (feed, driver) = open(name, source, &self.context);
				self.feeds.insert(name.clone(), feed);
				drivers.push(driver);
			}

			if let Some(strip) = self.feeds[name].strip() {
				let audio = collection.audio.get(name).copied().unwrap_or_default();
				strip.set_gain(audio.gain);
				strip.set_muted(audio.muted);
			}
		}
		drivers
	}

	/// The running source called `name`.
	pub fn get(&self, name: &str) -> Option<&Feed> {
		self.feeds.get(name)
	}

	/// Every running source, by name.
	pub fn iter(&self) -> impl Iterator<Item = (&str, &Feed)> {
		self.feeds.iter().map(|(name, feed)| (name.as_str(), feed))
	}
}

/// One running source. Dropping it stops its [`Driver`].
pub struct Feed {
	source: Source,
	picture: Mailbox,
	strip: Option<Strip>,
	status: kio::Consumer<Status>,
}

impl Feed {
	/// What this feed was opened for.
	pub fn source(&self) -> &Source {
		&self.source
	}

	/// What the source is doing.
	pub fn status(&self) -> Status {
		self.status.read().clone()
	}

	/// The source's gain, mute, and meter in the program audio, if it has sound.
	pub fn strip(&self) -> Option<&Strip> {
		self.strip.as_ref()
	}

	/// The newest picture not yet drawn.
	pub(crate) fn take(&self) -> Option<Picture> {
		self.picture.lock().unwrap().take()
	}
}

/// The work behind one [`Feed`]: opening the source and delivering what it
/// produces. Built by [`Sources::sync`].
pub struct Driver {
	name: String,
	source: Source,
	context: Context,
	picture: Mailbox,
	strip: Option<Strip>,
	status: kio::Producer<Status>,
}

impl Driver {
	/// The source's name in the collection.
	pub fn name(&self) -> &str {
		&self.name
	}

	/// Run the source until its [`Feed`] is dropped.
	///
	/// A failure is reported through [`Feed::status`] rather than returned. On
	/// macOS a capture source's future is `!Send`, so run it on a local task.
	pub async fn run(self) {
		let status = self.status.clone();
		let result = tokio::select! {
			_ = status.wanted_changed(true) => return,
			result = self.produce() => result,
		};
		let status = match result {
			Ok(()) => Status::Failed("the source ended".into()),
			Err(err) => Status::Failed(err.to_string()),
		};
		tracing::warn!(source = %self.name, ?status, "source stopped");
		set(&self.status, status);
	}

	async fn produce(&self) -> Result<(), Error> {
		match &self.source {
			Source::Color { color } => {
				self.show(Picture::Rgba {
					size: moq_video::Size::new(1, 1),
					pixels: color.0.to_vec(),
				});
				std::future::pending().await
			}
			Source::Image { path } => {
				let picture = image(&self.context.dir.join(path))?;
				self.show(picture);
				std::future::pending().await
			}
			Source::Broadcast { path } => self.broadcast(path).await,
			device => self.device(device).await,
		}
	}

	fn show(&self, picture: Picture) {
		*self.picture.lock().unwrap() = Some(picture);
		set(&self.status, Status::Live);
	}

	/// Capture a camera, display, window, or audio input.
	#[cfg(feature = "capture")]
	async fn device(&self, source: &Source) -> Result<(), Error> {
		use moq_audio::capture::Source as Audio;
		use moq_video::capture::Source as Video;

		let video = match source {
			Source::Camera { id } => Video::Camera(id.clone()),
			Source::Display { id } => Video::Display(id.clone()),
			Source::Window { id } => Video::Window(id.clone()),
			Source::Microphone { id } => return self.listen(Audio::Microphone(id.clone())).await,
			Source::SystemAudio => return self.listen(Audio::System).await,
			Source::Color { .. } | Source::Image { .. } | Source::Broadcast { .. } => {
				unreachable!("not a device source")
			}
		};

		let mut config = moq_video::capture::Config::default();
		config.source = video;
		config.output = self.context.output;
		let mut stream = moq_video::capture::open(&config).await?;
		while let Some(frame) = stream.read().await? {
			self.show(Picture::Frame(frame));
		}
		Ok(())
	}

	#[cfg(feature = "capture")]
	async fn listen(&self, source: moq_audio::capture::Source) -> Result<(), Error> {
		let strip = self.strip.clone().expect("audio sources have a strip");
		let mut config = moq_audio::capture::Config::default();
		config.source = source;
		let capture = self.context.bus.capture(config, strip);
		set(&self.status, Status::Live);
		Ok(capture.run().await?)
	}

	#[cfg(not(feature = "capture"))]
	async fn device(&self, _: &Source) -> Result<(), Error> {
		Err(Error::Unsupported(
			"this build cannot capture devices (enable the `capture` feature)".into(),
		))
	}

	/// Follow the broadcast at `path` for as long as the feed lives, playing
	/// each instance of it that comes online.
	async fn broadcast(&self, path: &str) -> Result<(), Error> {
		let mut instances = moq_mux::follow::Instances::new(&self.context.origin, path, None)?;
		set(&self.status, Status::Offline);

		let mut playing: Option<Playing<'_>> = None;
		loop {
			tokio::select! {
				event = instances.next() => match event {
					Ok(moq_mux::follow::Event::Start(broadcast)) => {
						set(&self.status, Status::Live);
						playing = Some(Box::pin(self.play(broadcast, path)));
					}
					Ok(moq_mux::follow::Event::End) => {
						playing = None;
						set(&self.status, Status::Offline);
					}
					Err(err) => return Err(err.into()),
				},
				result = async { playing.as_mut().expect("guarded").await }, if playing.is_some() => {
					playing = None;
					set(&self.status, match result {
						Ok(()) => Status::Offline,
						Err(err) => Status::Failed(err.to_string()),
					});
				}
			}
		}
	}

	/// Decode one instance of a broadcast: the first video and audio renditions
	/// each catalog offers, switching when a catalog update changes them, until
	/// the catalog ends.
	async fn play(&self, broadcast: moq_net::broadcast::Consumer, path: &str) -> Result<(), Error> {
		let format = moq_mux::catalog::CatalogFormat::detect(path).unwrap_or_default();
		let mut catalogs = moq_mux::catalog::Consumer::<()>::new(&broadcast, format).await?;

		// A track name always means the same content, so only a new track
		// restarts a decoder. That is the track an entry resolves to, not its
		// catalog ID: a publisher may keep the ID and point it at a replacement
		// track, as the JS publisher does on a codec change. A metadata-only
		// update, such as a jitter estimate, leaves playback alone.
		let mut video: Option<String> = None;
		let mut audio: Option<String> = None;
		let mut video_playing: Playing<'_> = Box::pin(std::future::pending());
		let mut audio_playing: Playing<'_> = Box::pin(std::future::pending());

		loop {
			tokio::select! {
				catalog = catalogs.next() => {
					let Some(catalog) = catalog? else {
						return Ok(());
					};
					let next = pick(catalog.video.enabled())?;
					let track = next.as_ref().map(|(id, config)| Reference::track(config, id).to_string());
					if track != video {
						video = track;
						video_playing = match next {
							Some((id, config)) => Box::pin(self.play_video(&broadcast, id, config)),
							None => Box::pin(std::future::pending()),
						};
					}
					let next = pick(catalog.audio.enabled())?;
					let track = next.as_ref().map(|(id, config)| Reference::track(config, id).to_string());
					if track != audio {
						audio = track;
						audio_playing = match next {
							Some((id, config)) => Box::pin(self.play_audio(&broadcast, id, config)),
							None => Box::pin(std::future::pending()),
						};
					}
				}
				// A rendition that ends waits for a catalog update to replace it.
				result = &mut video_playing => {
					result?;
					video_playing = Box::pin(std::future::pending());
				}
				result = &mut audio_playing => {
					result?;
					audio_playing = Box::pin(std::future::pending());
				}
			}
		}
	}

	async fn play_video(
		&self,
		broadcast: &moq_net::broadcast::Consumer,
		name: String,
		config: hang::catalog::VideoConfig,
	) -> Result<(), Error> {
		let mut options = moq_video::decode::Options::new();
		options.start = moq_video::decode::Start::Latest;
		options.decoder.output = self.context.output;
		let mut consumer = moq_video::decode::Consumer::new(broadcast, &config, name, options).await?;
		while let Some(read) = consumer.read().await? {
			if let moq_video::decode::Read::Frame(frame) = read {
				self.show(Picture::Frame(frame));
			}
		}
		Ok(())
	}

	async fn play_audio(
		&self,
		broadcast: &moq_net::broadcast::Consumer,
		name: String,
		config: hang::catalog::AudioConfig,
	) -> Result<(), Error> {
		let Some(strip) = self.strip.clone() else {
			return Ok(());
		};
		let bus = self.context.bus.config().clone();
		let mut options = moq_audio::decode::Options::new();
		options.start = moq_audio::decode::Start::Latest;
		options.output.format = moq_audio::Format::F32;
		options.output.sample_rate = Some(bus.sample_rate);
		options.output.layout = Some(bus.layout);
		let mut consumer = moq_audio::decode::Consumer::new(broadcast, &config, name, options).await?;

		let mut input = moq_audio::encode::Input::new(bus.sample_rate, bus.layout);
		input.format = moq_audio::Format::F32;
		let mut channel = self.context.bus.channel(input, strip)?;
		// Gaps and encoder boundaries need nothing. Like a picture, which shows
		// on arrival, the bus plays a source at its live edge on the program
		// clock: a stall plays as silence while it lasts, and a burst after it
		// is bounded by the bus's buffer. Replaying the publisher's
		// timeline, holes included, would put the audio behind the live edge
		// and its own video.
		while let Some(read) = consumer.read().await? {
			if let moq_audio::decode::Read::Frame(frame) = read {
				channel.write(&frame.data)?;
			}
		}
		Ok(())
	}
}

/// The best enabled rendition the broadcast serves itself, of `renditions`
/// ranked best first.
///
/// One served from another broadcast would need that broadcast's own
/// subscription, which a source does not open, so a catalog offering only
/// those is refused rather than shown as black or played as silence.
fn pick<'a, C: Reference + Clone + 'a>(
	renditions: impl Iterator<Item = (&'a String, &'a C)>,
) -> Result<Option<(String, C)>, Error> {
	let mut foreign = None;
	for (id, config) in renditions {
		if config.broadcast().is_none() {
			return Ok(Some((id.clone(), config.clone())));
		}
		foreign.get_or_insert(id);
	}
	match foreign {
		Some(id) => Err(Error::Unsupported(format!(
			"rendition {id} is served from another broadcast"
		))),
		None => Ok(None),
	}
}

/// Start the feed for `source`, and the driver that fills it.
fn open(name: &str, source: &Source, context: &Context) -> (Feed, Driver) {
	let picture = Mailbox::default();
	let strip = source.has_audio().then(Strip::default);
	let status = kio::Producer::new(Status::Starting);

	let feed = Feed {
		source: source.clone(),
		picture: picture.clone(),
		strip: strip.clone(),
		status: status.consume(),
	};
	let driver = Driver {
		name: name.to_string(),
		source: source.clone(),
		context: context.clone(),
		picture,
		strip,
		status,
	};
	(feed, driver)
}

fn set(status: &kio::Producer<Status>, value: Status) {
	if let Ok(mut status) = status.write() {
		*status = value;
	}
}

/// Decode a still image to straight-alpha RGBA.
fn image(path: &Path) -> Result<Picture, Error> {
	let bytes = std::fs::read(path).map_err(|err| Error::Image(format!("{}: {err}", path.display())))?;
	let image = image::load_from_memory(&bytes)
		.map_err(|err| Error::Image(format!("{}: {err}", path.display())))?
		.into_rgba8();
	Ok(Picture::Rgba {
		size: moq_video::Size::new(image.width(), image.height()),
		pixels: image.into_raw(),
	})
}

/// Helpers for tests across the crate.
#[cfg(test)]
pub(crate) mod test {
	use super::*;

	/// A context with no relay or device behind it, so broadcasts stay offline
	/// while the returned origin lives. Call from a [`tokio::task::LocalSet`].
	pub(crate) fn context(dir: &Path) -> (Context, moq_net::origin::Producer) {
		let (origin, driver) = moq_net::origin::Producer::new(Default::default());
		tokio::task::spawn_local(moq_net::time::run(driver));
		let context = Context {
			bus: Bus::new(Default::default()).unwrap(),
			origin: origin.consume(),
			dir: dir.to_path_buf(),
			output: moq_video::Output::Cpu,
		};
		(context, origin)
	}

	/// Run `drivers` on the local task set, and wait until every source in
	/// `sources` has left [`Status::Starting`].
	pub(crate) async fn start(sources: &Sources, drivers: Vec<Driver>) {
		for driver in drivers {
			tokio::task::spawn_local(driver.run());
		}
		for _ in 0..1000 {
			if sources.iter().all(|(_, feed)| feed.status() != Status::Starting) {
				return;
			}
			tokio::task::yield_now().await;
		}
		panic!("sources never started");
	}

	/// Run `f` on a local task set, as drivers need.
	pub(crate) async fn local<F: Future>(f: F) -> F::Output {
		tokio::task::LocalSet::new().run_until(f).await
	}
}

#[cfg(test)]
mod tests {
	use super::test::{context, local, start};
	use super::*;
	use crate::collection::{Audio, Rgba};

	fn collection() -> Collection {
		let mut collection = Collection::new();
		collection.sources.insert(
			"red".into(),
			Source::Color {
				color: Rgba([255, 0, 0, 255]),
			},
		);
		collection.sources.insert(
			"guest".into(),
			Source::Broadcast {
				path: "room/guest".into(),
			},
		);
		collection
	}

	#[tokio::test]
	async fn a_color_shows_one_pixel() {
		local(async {
			let dir = tempfile::tempdir().unwrap();
			let (context, _origin) = context(dir.path());
			let mut sources = Sources::new(context);
			let drivers = sources.sync(&collection());
			assert_eq!(drivers.len(), 2);
			start(&sources, drivers).await;

			let red = sources.get("red").unwrap();
			assert_eq!(red.status(), Status::Live);
			assert!(red.strip().is_none(), "a color is silent");
			let Some(Picture::Rgba { size, pixels }) = red.take() else {
				panic!("a color shows a picture");
			};
			assert_eq!(size, moq_video::Size::new(1, 1));
			assert_eq!(pixels, [255, 0, 0, 255]);
			assert!(red.take().is_none(), "taken once");

			let guest = sources.get("guest").unwrap();
			assert_eq!(guest.status(), Status::Offline, "nothing is announced");
			assert!(guest.strip().is_some(), "a broadcast feeds the bus");
		})
		.await;
	}

	/// Editing a source restarts it, removing one stops it, and an untouched one
	/// keeps running. Gain changes reach the strip without restarting anything.
	#[tokio::test]
	async fn sync_restarts_only_what_changed() {
		local(async {
			let dir = tempfile::tempdir().unwrap();
			let (context, _origin) = context(dir.path());
			let mut sources = Sources::new(context);
			let mut collection = collection();
			let drivers = sources.sync(&collection);
			start(&sources, drivers).await;
			let guest = sources.get("guest").unwrap().strip().unwrap().clone();

			collection.audio.insert(
				"guest".into(),
				Audio {
					gain: 0.25,
					muted: true,
				},
			);
			assert!(sources.sync(&collection).is_empty());
			assert_eq!(guest.gain(), 0.25);
			assert!(guest.muted());

			collection.sources.insert(
				"red".into(),
				Source::Color {
					color: Rgba([0, 0, 255, 255]),
				},
			);
			let drivers = sources.sync(&collection);
			assert_eq!(drivers.iter().map(Driver::name).collect::<Vec<_>>(), ["red"]);
			start(&sources, drivers).await;
			let Some(Picture::Rgba { pixels, .. }) = sources.get("red").unwrap().take() else {
				panic!("the edited color shows");
			};
			assert_eq!(pixels, [0, 0, 255, 255]);

			collection.sources.remove("red");
			assert!(sources.sync(&collection).is_empty());
			assert!(sources.get("red").is_none());
		})
		.await;
	}

	/// A broadcast source follows catalog updates: when a better rendition
	/// appears, it switches to it without the broadcast restarting.
	#[tokio::test]
	async fn a_broadcast_follows_its_catalog() {
		local(async {
			let dir = tempfile::tempdir().unwrap();
			let (context, origin) = context(dir.path());
			let publication = origin.create_broadcast("room/guest").unwrap();
			let mut broadcast = publication.broadcast().clone();
			let catalog = moq_mux::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
			// A known bitrate ranks a rendition above one without.
			let audio = |name: &str, bitrate: Option<u64>| {
				let track = broadcast
					.create_track(name, catalog.track_info(hang::catalog::PRIORITY.audio))
					.unwrap();
				let mut config = hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, 48_000, 2);
				config.bitrate = bitrate;
				catalog
					.audio(
						track,
						moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Audio),
						config,
					)
					.unwrap()
			};
			let first = audio("first", None);
			publication
				.announce(moq_net::origin::Route::default().with_epoch(moq_net::Epoch::mint()))
				.unwrap();

			let mut sources = Sources::new(context);
			let drivers = sources.sync(&collection());
			start(&sources, drivers).await;

			let mut first = first.demand();
			while first.wanted_changed().await.unwrap().is_none() {}

			let second = audio("second", Some(128_000));
			let mut second = second.demand();
			while second.wanted_changed().await.unwrap().is_none() {}
			while first.wanted_changed().await.unwrap().is_some() {}
			assert_eq!(sources.get("guest").unwrap().status(), Status::Live);
		})
		.await;
	}

	/// A rendition that keeps its catalog ID but points at a replacement track,
	/// as the JS publisher does on a codec change, moves playback to that track.
	#[tokio::test]
	async fn a_broadcast_follows_a_renditions_track() {
		local(async {
			let dir = tempfile::tempdir().unwrap();
			let (context, origin) = context(dir.path());
			let publication = origin.create_broadcast("room/guest").unwrap();
			let mut broadcast = publication.broadcast().clone();
			let mut catalog = moq_mux::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
			let info = catalog.track_info(hang::catalog::PRIORITY.audio);
			let first = broadcast.create_track("first", info.clone()).unwrap();
			let second = broadcast.create_track("second", info).unwrap();
			let point = |catalog: &mut moq_mux::catalog::Producer, track: &str| {
				let mut config = hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, 48_000, 2);
				config.track = Some(track.into());
				catalog
					.mutate(|catalog| {
						catalog.audio.renditions.insert("audio".into(), config);
					})
					.unwrap();
			};
			point(&mut catalog, "first");
			publication
				.announce(moq_net::origin::Route::default().with_epoch(moq_net::Epoch::mint()))
				.unwrap();

			let mut sources = Sources::new(context);
			let drivers = sources.sync(&collection());
			start(&sources, drivers).await;

			let mut first = first.demand();
			while first.wanted_changed().await.unwrap().is_none() {}

			point(&mut catalog, "second");
			let mut second = second.demand();
			while second.wanted_changed().await.unwrap().is_none() {}
			while first.wanted_changed().await.unwrap().is_some() {}
			assert_eq!(sources.get("guest").unwrap().status(), Status::Live);
		})
		.await;
	}

	/// A catalog whose only rendition lives in another broadcast fails the
	/// source rather than leaving it live and silent.
	#[tokio::test]
	async fn a_foreign_rendition_is_refused() {
		local(async {
			let dir = tempfile::tempdir().unwrap();
			let (context, origin) = context(dir.path());
			let publication = origin.create_broadcast("room/guest").unwrap();
			let mut broadcast = publication.broadcast().clone();
			let mut catalog = moq_mux::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
			let mut config = hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, 48_000, 2);
			config.broadcast = Some(moq_net::path::Relative::new("../other").unwrap());
			catalog
				.mutate(|catalog| {
					catalog.audio.renditions.insert("audio".into(), config);
				})
				.unwrap();
			publication
				.announce(moq_net::origin::Route::default().with_epoch(moq_net::Epoch::mint()))
				.unwrap();

			let mut sources = Sources::new(context);
			let drivers = sources.sync(&collection());
			start(&sources, drivers).await;

			let guest = sources.get("guest").unwrap();
			for _ in 0..1000 {
				if let Status::Failed(reason) = guest.status() {
					assert!(reason.contains("another broadcast"), "{reason}");
					return;
				}
				tokio::task::yield_now().await;
			}
			panic!("the source never failed: {:?}", guest.status());
		})
		.await;
	}

	/// Dropping a feed stops its driver, which is what releases a device.
	#[tokio::test]
	async fn a_dropped_feed_stops_its_driver() {
		local(async {
			let dir = tempfile::tempdir().unwrap();
			let (context, _origin) = context(dir.path());
			let mut sources = Sources::new(context);
			let mut drivers = sources.sync(&collection());
			let driver = drivers.pop().unwrap();
			let running = tokio::task::spawn_local(driver.run());
			tokio::task::yield_now().await;
			assert!(!running.is_finished());

			sources.sync(&Collection::new());
			running.await.unwrap();
		})
		.await;
	}

	#[tokio::test]
	async fn an_image_loads_relative_to_the_scene_file() {
		local(async {
			let dir = tempfile::tempdir().unwrap();
			let mut png = image::RgbaImage::new(3, 2);
			png.put_pixel(2, 1, image::Rgba([1, 2, 3, 4]));
			png.save(dir.path().join("logo.png")).unwrap();

			let mut collection = Collection::new();
			collection.sources.insert(
				"logo".into(),
				Source::Image {
					path: "logo.png".into(),
				},
			);
			collection.sources.insert(
				"missing".into(),
				Source::Image {
					path: "missing.png".into(),
				},
			);
			let (context, _origin) = context(dir.path());
			let mut sources = Sources::new(context);
			let drivers = sources.sync(&collection);
			start(&sources, drivers).await;

			let Some(Picture::Rgba { size, pixels }) = sources.get("logo").unwrap().take() else {
				panic!("an image shows a picture");
			};
			assert_eq!(size, moq_video::Size::new(3, 2));
			assert_eq!(pixels[pixels.len() - 4..], [1, 2, 3, 4]);

			let Status::Failed(reason) = sources.get("missing").unwrap().status() else {
				panic!("a missing image fails");
			};
			assert!(reason.contains("missing.png"), "{reason}");
		})
		.await;
	}

	#[cfg(not(feature = "capture"))]
	#[tokio::test]
	async fn a_build_without_capture_says_so() {
		local(async {
			let dir = tempfile::tempdir().unwrap();
			let mut collection = Collection::new();
			collection.sources.insert("cam".into(), Source::Camera { id: None });
			collection.sources.insert("mic".into(), Source::Microphone { id: None });
			let (context, _origin) = context(dir.path());
			let mut sources = Sources::new(context);
			let drivers = sources.sync(&collection);
			start(&sources, drivers).await;
			for name in ["cam", "mic"] {
				let Status::Failed(reason) = sources.get(name).unwrap().status() else {
					panic!("{name} cannot capture");
				};
				assert!(reason.contains("capture"), "{reason}");
			}
		})
		.await;
	}
}
