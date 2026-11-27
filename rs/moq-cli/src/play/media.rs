//! The media task: subscribe, pick renditions, and decode into the window and
//! the speaker.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::Duration;

use anyhow::Context;
use hang::catalog::Orientation;
use moq_mux::catalog::{self, Stream};
// The ambient clock, which follows tokio's paused clock when a test drives the
// playout clock itself.
use kio::clock::Instant;

use super::args::{Args, REACH};
use super::output::{Output, Sink, Speaker};
use super::playback::{Half, Kind, Playback, Running, Switch, Track, joined};
use super::timeline::{AudioTimeline, Playhead};
use super::video::Video;
use super::window::Event;
use moq_mux::sync::Producer as Presentation;

/// What a frame that starts a new decoded timeline crossed.
enum Crossing {
	/// A declared end: the frame starts the utterance after its silence.
	End {
		/// Another end is already declared past this frame, so the utterance it
		/// starts is followed by a silence too.
		ahead: bool,
	},
	/// Lost media: no end was declared.
	Loss,
}

/// The depth a sink opens on, and its floor until the device reports its own
/// latency, which replaces it. A ring with no depth at all can never be read
/// from.
const AUDIO_BUFFER_MIN: Duration = Duration::from_millis(50);

/// How long the device's highest reported output latency holds. A host's report
/// moves with where each callback lands, so the delay the clock holds for it
/// follows the peak rather than every reading, and falls once a lower latency,
/// as after a switch off a Bluetooth headset, has held this long.
const OUTPUT_WINDOW: Duration = Duration::from_secs(5);

/// How far past the latency it opened on the sink may be asked to hold. Its ring
/// reaches three seconds past that latency, less a 30 ms processing window
/// (`Sink::set_latency`), so this leaves margin, and it covers audio's reach past the delay.
const SINK_ROOM: Duration = Duration::from_secs(2);

/// Maximum sample frames per silence write, independent of the catalog rate.
const AUDIO_SILENCE: usize = 960;

/// How much longer than the speaker holds to wait for it to drain. A device that
/// never opens reports its queue as full forever, and a truncated tail beats
/// hanging on the way out.
const AUDIO_DRAIN_GRACE: Duration = Duration::from_secs(1);

/// Everything the media task needs to fill the window and the speaker.
#[derive(Clone)]
pub(super) struct Media<O: Output> {
	pub(super) origin: moq_net::origin::Consumer,
	pub(super) broadcast: String,
	pub(super) args: Args,
	pub(super) video: Arc<Mutex<VecDeque<moq_video::Frame>>>,
	pub(super) presentation: Presentation,
	/// What the speaker is sounding, for `--metrics`.
	pub(super) playhead: Playhead,
	pub(super) drained: Arc<tokio::sync::Notify>,
	pub(super) output: O,
	/// The window's orientation, `None` while square or without a window.
	pub(super) viewport: tokio::sync::watch::Receiver<Option<Orientation>>,
}

impl<O: Output> Media<O> {
	pub(super) async fn run(self) {
		let output = self.output.clone();
		let event = match self.play().await {
			Ok(()) => Event::Ended,
			Err(err) => Event::Failed(format!("{err:#}")),
		};
		output.send(event);
	}

	/// Follow the path for as long as the origin lasts: each identity that comes
	/// online replaces what is playing, and going offline lets what is playing
	/// finish while waiting for the next one.
	async fn play(mut self) -> anyhow::Result<()> {
		let mut follow = self.origin.follow(&self.broadcast, None)?;
		// The device belongs to the player, so replacing a broadcast never waits
		// for it to close and reopen. Each sink adapts its own rate and layout.
		let device = tokio::sync::OnceCell::new();
		let mut playing = None;

		loop {
			tokio::select! {
				event = follow.available() => {
					let broadcast = match event {
						Ok(Some(broadcast)) => broadcast,
						Ok(None) => {
							tracing::info!(broadcast = %self.broadcast, "offline, waiting for it to return");
							continue;
						}
						Err(moq_net::Error::Closed) => return Ok(()),
						Err(err) => return Err(err).context("failed to follow the broadcast"),
					};
					tracing::info!(
						broadcast = %self.broadcast,
						identity = ?broadcast.info().identity,
						"online"
					);

					// Another identity's timeline is its own, so nothing of the old one carries over.
					drop(playing.take());
					self.presentation = self.presentation.reset(self.args.video_delay());
					self.playhead = self.playhead.clone().reset();
					self.video.lock().unwrap().clear();
					self.drained.notify_one();
					self.output.send(Event::Wake);
					let media = self.clone();
					let device = &device;
					playing = Some(Box::pin(async move { media.play_broadcast(broadcast, device).await }));
				}
				result = async { playing.as_mut().expect("guarded").await }, if playing.is_some() => {
					playing = None;
					match result {
						Ok(()) => tracing::info!(broadcast = %self.broadcast, "broadcast ended"),
						Err(err) if err.is::<Unplayable>() => return Err(err),
						Err(err) => tracing::warn!(broadcast = %self.broadcast, err = format!("{err:#}"), "broadcast ended"),
					}
				}
			}
		}
	}

	/// Play the identity handed out by the follower until its catalog and tracks end.
	async fn play_broadcast(
		&self,
		broadcast: moq_net::broadcast::Consumer,
		device: &tokio::sync::OnceCell<O::Speaker>,
	) -> anyhow::Result<()> {
		let source = moq_mux::Source::new(self.origin.clone(), broadcast.clone());
		let mut catalogs = catalog::Consumer::<()>::new(&broadcast, self.args.catalog_format(&self.broadcast))
			.await
			.context("failed to subscribe to the catalog")?;
		// Applied by hand rather than with `select`, which drops disabled renditions: a disabled
		// rendition stops now, while one that leaves the catalog plays out its tail.
		let selection = self.args.select.selection(None);
		let mut tasks = tokio::task::JoinSet::new();
		let mut playback = Playback::default();
		// Each half's lifecycle: what plays, what replaces it, and what is opening. A rendition
		// being opened is raced against everything else here, so neither half waits on the
		// other's broadcast or subscription. A video replacement opens while the rendition it
		// replaces plays on, which stops only once the replacement's track answers, so one that
		// never does (a broadcast nothing announces, a subscription left unanswered) leaves the
		// picture playing rather than frozen.
		let mut video: Half<Showing, Opening<'_, VideoReady>> = Half::default();
		let mut audio: Half<Listening, Opening<'_, AudioReady>> = Half::default();
		// Numbers each task, so the end of one a replacement stopped re-arms nothing.
		let mut spawned = 0u64;
		// Held by the video task playing, and taken by its replacement only once the task it
		// replaced lets go: the presentation clock keeps one track per kind, and the old one's
		// release would clear the new one's.
		let video_clock = Arc::new(tokio::sync::Mutex::new(()));
		// Shared by an audio rendition and the retired tails still playing beside
		// it, so their sinks mix on one stream: a second stream on an exclusive
		// device would fail to open. Released once none of them is left; the
		// player keeps the device.
		let mut speaker = None;
		// Retired audio sinks still playing out what they hold.
		let mut tails = tokio::task::JoinSet::new();
		let mut viewport = self.viewport.clone();
		// Cleared once the window is gone, or there never was one, which leaves the viewport as is.
		let mut viewing = true;
		// Why nothing started, so a catalog this build can't play reports the
		// reason instead of leaving a blank window up forever. The decoders are
		// gated by platform and cargo feature (no AV1 without `nvidia`, say), so
		// this covers gaps the codec flags can't be validated against up front.
		let mut rejected = Vec::new();

		loop {
			// A rendition still opening can play yet, even after the catalog ended.
			if playback.done(&video, &audio) {
				if !playback.played {
					return Err(Unplayable("the catalog contains no playable audio or video renditions".into()).into());
				}
				tails.join_all().await;
				return Ok(());
			}

			// Kept while a rendition opens, so its rejections join those of the other half's.
			if video.opening().is_none() && audio.opening().is_none() {
				rejected.clear();
			}

			// Only wait when there is nothing on hand to act on. The snapshot that
			// retires a rendition arrives while that rendition is still playing, so
			// the half it stops reads it after the fact, by which time the catalog
			// may have ended and the task set emptied: both branches disarmed, with
			// a replacement still on offer.
			if playback.pending(&video, &audio).is_none() {
				// Watched apart from the halves, which the branches below borrow, and only while
				// nothing replaces what plays: a switch under way keeps the watch for once it settles.
				let video_outranked = video
					.running()
					.filter(|_| video.settled())
					.and_then(|running| running.data.outranked.clone());
				let audio_outranked = audio
					.running()
					.filter(|_| audio.settled())
					.and_then(|running| running.data.outranked.clone());
				tokio::select! {
					result = tasks.join_next(), if !tasks.is_empty() => {
						if let Some((kind, task, result)) = joined(result.expect("guarded by is_empty"))? {
							let current = match kind {
								Kind::Video => video.is_current(task),
								Kind::Audio => audio.is_current(task),
							};
							let finished = if current {
								let (finished, sink) = result?;
								if kind == Kind::Audio {
									// A replacement is a track boundary whose timestamps need not
									// continue this one.
									self.playhead.restart();
									// The track released its target, which can lower the delay that
									// video's decoder and window are asleep on.
									self.drained.notify_one();
									self.output.send(Event::Wake);
								}
								// The retired sink still holds a delay of audio, and a replacement
								// holds its own before its first sample sounds. Played one after
								// the other, a rendition switch costs that delay in silence, so the
								// tail plays out while the replacement fills.
								if let Some(sink) = sink {
									tails.spawn(drain(sink));
								}
								finished
							} else {
								// Its replacement already holds the half, so how it ended changes
								// nothing but whether its own track finished first.
								match result {
									Ok((finished, _)) => finished,
									Err(err) => {
										tracing::debug!(err = format!("{err:#}"), "replaced rendition failed");
										false
									}
								}
							};
							// A better rendition still pending was never tried, so a half left with
							// nothing reads its snapshot again and waits on it rather than staying blank.
							match kind {
								Kind::Video => {
									let outranked = video.running().is_some_and(|running| running.task == task && running.data.outranked.is_some());
									playback.ended(&mut video, task, finished);
									if outranked && !video.busy() {
										video.repick();
									}
								}
								Kind::Audio => {
									let outranked = audio.running().is_some_and(|running| running.task == task && running.data.outranked.is_some());
									playback.ended(&mut audio, task, finished);
									if outranked && !audio.busy() {
										audio.repick();
									}
								}
							}
						}
					}
					opened = async { video.opening_mut().expect("guarded").future.as_mut().await }, if video.opening().is_some() => {
						rejected.extend(opened.rejected);
						let outranked = opened.outranked;
						match opened.ready {
							// Its track finished while it opened, leaving only its newest group, which
							// already played, so the half picks again without it.
							Some(ready) if playback.finished(&ready.reference) => {
								video.cancel_opening();
								video.repick();
							}
							Some(ready) => {
								spawned += 1;
								let task = spawned;
								tracing::info!(track = ready.name, decoder = ready.decoder.name(), "playing video rendition");
								let (delay, held) = tokio::sync::watch::channel(None);
								let player = Video {
									presentation: self.presentation.clone(),
									frames: self.video.clone(),
									changed: self.drained.clone(),
									output: self.output.clone(),
									max_delay: self.args.video_delay(),
									min_delay: self.args.video_delay(),
									delay: held,
								};
								let (stop, mut stopped) = tokio::sync::oneshot::channel();
								// Video plays on once moved, until its replacement is ready, so it has
								// nothing to retire.
								let demand = Demand {
									name: ready.name,
									_stop: stop,
									delay,
								};
								demand.hold(&self.args, ready.delay);
								// A snapshot that arrived while it opened kept the opening but may have raised its delay.
								demand.hold(&self.args, playback.latest().and_then(|snapshot| snapshot.video.renditions.get(&demand.name)).and_then(|config| config.delay));
								let (first, track, decoder) = (ready.first, ready.track, ready.decoder);
								let clock = video_clock.clone();
								tasks.spawn(async move {
									// The task it replaces is stopped as this starts, so this waits only
									// for that task to let go of the video clock.
									let clock = tokio::select! {
										clock = clock.lock_owned() => clock,
										_ = &mut stopped => return (Kind::Video, task, Ok((false, None))),
									};
									let result = until_stopped(player.resume(Some(first), track, decoder), stopped).await;
									// Its hold on the video clock went with `resume`.
									drop(clock);
									(Kind::Video, task, result.map(|(finished, _)| (finished, None)))
								});
								// Stops the rendition it replaces, which played until now.
								let showing = Showing { demand, shape: ready.shape, outranked };
								playback.started(&mut video, Running { task, track: ready.reference, data: showing });
							}
							// Nothing to switch to, so what plays keeps playing.
							None => video.give_up(),
						}
					}
					opened = async { audio.opening_mut().expect("guarded").future.as_mut().await }, if audio.opening().is_some() => {
						rejected.extend(opened.rejected);
						let outranked = opened.outranked;
						match opened.ready {
							// Its track finished while it opened, as video's can.
							Some(ready) if playback.finished(&ready.reference) => {
								audio.cancel_opening();
								audio.repick();
							}
							// An upgrade's replacement plays once the rendition it replaces has played
							// out what its sink holds, which it fills behind, as after a move. Played
							// side by side, both would drive the speaker's playhead.
							Some(ready) if audio.switching() == Some(Switch::Upgrade) => {
								// Only the rendition that opened, so a snapshot arriving while what
								// plays retires is checked against it alone.
								let candidates: Vec<AudioCandidate> = audio
									.opening()
									.map(|opening| {
										opening
											.candidates
											.lock()
											.unwrap()
											.iter()
											.filter(|(name, ..)| *name == ready.name)
											.cloned()
											.collect()
									})
									.unwrap_or_default();
								let next = Opened {
									ready: Some(ready),
									rejected: Vec::new(),
									outranked,
								};
								let next = Opening {
									candidates: Arc::new(Mutex::new(candidates)),
									future: Box::pin(std::future::ready(next)),
								};
								if audio.hand_over(next)
									&& let Some(running) = audio.running()
								{
									running.data.retire.notify_one();
								}
							}
							Some(ready) => {
								tracing::info!(track = ready.name, decoder = ready.consumer.name(), "playing audio rendition");
								if speaker.is_none() {
									speaker = Some(device.get_or_try_init(|| self.output.speaker()).await?.clone());
								}
								spawned += 1;
								let task = spawned;
								let (delay, held) = tokio::sync::watch::channel(None);
								let player = AudioPlayback {
									delay: held,
									speaker: speaker.clone().expect("opened above"),
									presentation: self.presentation.clone(),
									playhead: self.playhead.clone(),
									latency: self.args.fixed_delay().unwrap_or_default().max(AUDIO_BUFFER_MIN),
									changed: self.drained.clone(),
									output: self.output.clone(),
									stretch: self.args.stretch(),
								};
								let (stop, stopped) = tokio::sync::oneshot::channel();
								let retire = Arc::new(tokio::sync::Notify::new());
								let demand = Demand {
									name: ready.name,
									_stop: stop,
									delay,
								};
								demand.hold(&self.args, ready.delay);
								// A snapshot that arrived while it opened kept the opening but may have raised its delay.
								demand.hold(&self.args, playback.latest().and_then(|snapshot| snapshot.audio.renditions.get(&demand.name)).and_then(|config| config.delay));
								let consumer = ready.consumer;
								let retired = retire.clone();
								tasks.spawn(async move {
									let result = until_stopped(play_audio(consumer, player, retired.notified()), stopped).await;
									(Kind::Audio, task, result)
								});
								let listening = Listening { demand, retire, outranked };
								playback.started(&mut audio, Running { task, track: ready.reference, data: listening });
							}
							None => audio.give_up(),
						}
					}
					// A better video rendition's broadcast landed, so it replaces the one playing
					// once it is ready.
					() = async { video_outranked.clone().expect("guarded").landed().await }, if video_outranked.is_some() => {
						if let Some(running) = video.running_mut() {
							running.data.outranked = None;
						}
						video.upgrade();
					}
					// A better audio rendition's broadcast landed, so it opens while the one
					// playing plays on.
					() = async { audio_outranked.clone().expect("guarded").landed().await }, if audio_outranked.is_some() => {
						if let Some(running) = audio.running_mut() {
							running.data.outranked = None;
						}
						audio.upgrade();
					}
					_ = tails.join_next(), if !tails.is_empty() => {}
					changed = viewport.changed(), if viewing => {
						if changed.is_err() {
							viewing = false;
							continue;
						}
						let orientation = *viewport.borrow();
						let switching = video.switching();
						let turned = video.running().zip(playback.latest()).is_some_and(|(running, snapshot)| {
							turned_away(&snapshot.video, &running.data.demand.name, running.data.shape, orientation, |name, config| {
								!playback.finished(&Track::of(&source, config, name))
							})
						});
						if video.opening().is_some()
							&& (video.running().is_none() || matches!(switching, Some(Switch::Move | Switch::Upgrade)))
						{
							// Picked again shaped like the window. A pick that tries the same
							// renditions in the same order keeps the opening it has. An upgrade
							// ranks against the new shape too, so it covers a turn.
							video.repick();
						} else if turned {
							// The playing rendition is the wrong way round.
							video.turn();
						} else if switching == Some(Switch::Turn) {
							// Turned back before the replacement was ready.
							video.give_up();
						}
					}
					// Followed for as long as it lasts, not just until something is
					// playing: a publisher retires renditions (a transcode ladder
					// resizing under a source that changed resolution) by naming the
					// replacement in a snapshot and only then finishing the track it
					// replaces, so the snapshot that matters lands while both halves
					// are still running.
					snapshot = catalogs.next(), if playback.following() => {
						match snapshot.context("failed to read the catalog")? {
							Some(mut snapshot) => {
								// A disabled rendition stops now.
								if video.running().is_some_and(|running| {
									snapshot.video.renditions.get(&running.data.demand.name).is_some_and(|config| !config.enabled)
								}) {
									playback.stop(&mut video);
								}
								if audio.running().is_some_and(|running| {
									snapshot.audio.renditions.get(&running.data.demand.name).is_some_and(|config| !config.enabled)
								}) {
									playback.stop(&mut audio);
								}
								// An opening that would try a disabled rendition is picked again, even
								// while the half waits for a stopped rendition to end.
								if video.opening().is_some_and(|opening| {
									opening.candidates.lock().unwrap().iter().any(|(name, _, _)| {
										snapshot.video.renditions.get(name).is_some_and(|config| !config.enabled)
									})
								}) {
									video.cancel_opening();
								}
								if audio.opening().is_some_and(|opening| {
									opening.candidates.lock().unwrap().iter().any(|(name, _, _)| {
										snapshot.audio.renditions.get(name).is_some_and(|config| !config.enabled)
									})
								}) {
									audio.cancel_opening();
								}
								// The replacement an upgrade handed over to plays once what plays
								// retires, unless this snapshot disabled, moved, or removed it: unlike a
								// rendition playing, it has no tail to play out.
								if audio.next().is_some_and(|next| {
									next.candidates.lock().unwrap().iter().any(|(name, _, track)| {
										snapshot
											.audio
											.renditions
											.get(name)
											.is_none_or(|config| !config.enabled || Track::of(&source, config, name) != *track)
									})
								}) {
									audio.drop_next();
								}
								selection.retain(&mut snapshot);
								// Video holds no tail beyond the frames queued for the window, so the
								// rendition it left plays on until its replacement is ready. Only a move
								// switches here: a snapshot never turns the picture, even when it adds a
								// rendition shaped like the window, which only a viewport change does. A
								// rendition that failed to open would otherwise be tried again on every
								// catalog update, and publishers update their hints often (a recorded
								// decision on #9287).
								if let Some(running) = video.running() {
									let name = &running.data.demand.name;
									let moved = snapshot
										.video
										.renditions
										.get(name)
										.map(|config| Track::of(&source, config, name) != running.track);
									let moving = video.switching() == Some(Switch::Move);
									match moved {
										Some(true) if !moving => video.move_off(),
										// Moved back onto the track it plays, so nothing replaces it.
										Some(false) if moving => video.stay(),
										_ => {}
									}
								}
								// Audio plays out the tail of the track it left, then picks again.
								let moved = audio.running().is_some_and(|running| {
									let name = &running.data.demand.name;
									snapshot
										.audio
										.renditions
										.get(name)
										.is_some_and(|config| Track::of(&source, config, name) != running.track)
								});
								if moved
									&& audio.retire()
									&& let Some(running) = audio.running()
								{
									running.data.retire.notify_one();
								}
								if let Some(running) = video.running() {
									let demand = &running.data.demand;
									demand.hold(&self.args, snapshot.video.renditions.get(&demand.name).and_then(|config| config.delay));
								}
								if let Some(running) = audio.running() {
									let demand = &running.data.demand;
									demand.hold(&self.args, snapshot.audio.renditions.get(&demand.name).and_then(|config| config.delay));
								}
								playback.received(snapshot);
							},
							// A rendition still opening may yet play, so whether anything did is
							// decided once nothing more can start.
							None => playback.catalog_ended = true,
						}
					}
				}
			}

			// The last sink went quiet.
			if !audio.playing() && tails.is_empty() && speaker.take().is_some() {
				self.playhead.clear();
				self.drained.notify_one();
			}

			// Start whatever isn't playing from the newest snapshot, which is not
			// necessarily the one that just arrived: the half that a retirement
			// stopped reads the snapshot naming its replacement afterwards.
			let Some(snapshot) = playback.pending(&video, &audio).cloned() else {
				unplayable(
					tasks.is_empty() && video.opening().is_none() && audio.opening().is_none(),
					&rejected,
				)?;
				continue;
			};

			if playback.wants(&video) {
				let orientation = *viewport.borrow_and_update();
				// A turn switches only to a rendition shaped like the window: the rest are no
				// better than the one playing.
				let turning = video.switching() == Some(Switch::Turn);
				// An upgrade switches only to a rendition ranked above the one playing.
				let upgrading = video.switching() == Some(Switch::Upgrade);
				// This read may have taken a viewport change before its branch saw it, such as
				// the window turning back or going square under the turn, so the turn is checked
				// against the viewport read here.
				let turned_back = turning
					&& !video.running().is_some_and(|running| {
						turned_away(
							&snapshot.video,
							&running.data.demand.name,
							running.data.shape,
							orientation,
							|name, config| !playback.finished(&Track::of(&source, config, name)),
						)
					});
				if turned_back {
					// Nothing in the snapshot was tried, so it stays unread: once the rendition
					// playing ends, the half reads it.
					video.give_up();
				} else {
					playback.read(&mut video, turning || upgrading);
					let mut available: Vec<VideoCandidate> = snapshot
						.video
						.enabled()
						.map(|(name, config)| (name.clone(), config.clone(), Track::of(&source, config, name)))
						// A finished track has only its newest group left, which already played.
						.filter(|(_, _, reference)| !playback.finished(reference))
						.collect();
					let mut candidates = arrange(&snapshot.video, available.clone(), orientation, turning);
					if upgrading && let Some(running) = video.running() {
						// One the catalog dropped has no rank left to beat, so nothing is an upgrade.
						let rank = candidates.iter().position(|(_, _, track)| *track == running.track);
						candidates.truncate(rank.unwrap_or(0));
						available.clone_from(&candidates);
					}
					if candidates.is_empty() {
						video.give_up();
					} else if video
						.opening()
						.is_none_or(|opening| !same_renditions(&opening.candidates.lock().unwrap(), &candidates))
					{
						// Replaces one still opening, which the newer snapshot or viewport overrides.
						let candidates = Arc::new(Mutex::new(candidates));
						let future =
							self.open_video(&source, snapshot.video.clone(), available, turning, candidates.clone());
						video.open(Opening {
							candidates,
							future: Box::pin(future),
						});
					}
				}
			}

			if playback.wants(&audio) {
				// An upgrade switches only to a rendition ranked above the one playing.
				let upgrading = audio.switching() == Some(Switch::Upgrade);
				playback.read(&mut audio, upgrading);
				let mut candidates: Vec<AudioCandidate> = snapshot
					.audio
					.enabled()
					.map(|(name, config)| (name.clone(), config.clone(), Track::of(&source, config, name)))
					// A finished track has only its newest group left, which already played.
					.filter(|(_, _, reference)| !playback.finished(reference))
					.collect();
				if upgrading && let Some(running) = audio.running() {
					// One the catalog dropped has no rank left to beat, so nothing is an upgrade.
					let rank = candidates.iter().position(|(_, _, track)| *track == running.track);
					candidates.truncate(rank.unwrap_or(0));
				}
				if candidates.is_empty() {
					audio.give_up();
				} else if audio
					.opening()
					.is_none_or(|opening| !same_renditions(&opening.candidates.lock().unwrap(), &candidates))
				{
					let future = self.open_audio(&source, candidates.clone());
					audio.open(Opening {
						candidates: Arc::new(Mutex::new(candidates)),
						future: Box::pin(future),
					});
				}
			}

			unplayable(
				tasks.is_empty() && video.opening().is_none() && audio.opening().is_none(),
				&rejected,
			)?;
		}
	}

	/// Open the first of `available` that resolves, subscribes, decodes, and answers, once the
	/// window's renderer is up, trying those shaped like the window first. The window reports its
	/// shape before its renderer comes up, so the order is read only then, and the first pick is
	/// already shaped like it rather than switching straight away. The order it tries goes into
	/// `order`, which a later pick compares with its own.
	async fn open_video(
		&self,
		source: &moq_mux::Source,
		video: hang::catalog::Video,
		available: Vec<VideoCandidate>,
		turning: bool,
		order: Order<hang::catalog::VideoConfig>,
	) -> Opened<VideoReady> {
		let render = self.output.render().await;
		let orientation = *self.viewport.borrow();
		let arranged = arrange(&video, available, orientation, turning);
		order.lock().unwrap().clone_from(&arranged);
		let video = &video;
		open_first(
			source,
			"video",
			arranged,
			|rendition, name, config, reference| async move {
				// The window's renderer refuses a GPU surface it cannot import, so
				// the decoder downloads those instead, all of them on a device
				// that imports none.
				let mut decode = moq_video::decode::Config::new();
				decode.output = render;
				decode.device = crate::device(&self.args.vaapi_device, &self.args.v4l2_decoder);
				let decoder = moq_video::decode::Sink::open(&config, &decode).await?;
				// Nothing older than the playhead is worth presenting, so the delay
				// doubles as the staleness budget on the wire.
				let delay = moq_mux::container::Delay {
					min: Some(self.args.video_delay()),
					max: Some(self.args.video_delay()),
				};
				let playout = moq_mux::container::Playout::new(delay, Duration::ZERO);
				let track = rendition.track(hang::catalog::Reference::track(&config, &name))?;
				// The whole delay from the first request, so a cold join never states a
				// zero minimum before the container's update. A live join: the default
				// range has no start, so the stated minimum places it behind each hop's
				// live edge, never at a warm copy's backlog, so it names no cached group
				// for the shared publisher subscription to rewind to.
				let subscriber = track
					.subscribe(
						playout.subscription(
							moq_net::track::Subscription::default()
								.with_priority(hang::catalog::PRIORITY.video)
								.with_ranges([moq_net::track::Range::ALL]),
						),
					)
					.await?;
				let format = catalog::hang::Container::try_from(&config)?;
				// The playout delay is the target latency, and the max how late a group may be.
				let mut track = playout.consumer(subscriber, format);
				// Ready only once the track answers with media, so a replacement swaps in with
				// its picture on the way rather than freezing what plays while it waits. A stop
				// marker or gap before any frame is no picture: nothing was read for it to end or
				// skip, and the frames that drain a stopped group are padding.
				let mut stopped = None;
				let first = loop {
					let read = track.read().await?.context("the track ended before its first frame")?;
					match &read {
						moq_mux::container::Read::Frame { group, .. } if stopped != Some(*group) => break read,
						moq_mux::container::Read::End { group, .. } => stopped = Some(*group),
						_ => {}
					}
				};
				Ok(VideoReady {
					shape: video.orientation(&config),
					name,
					reference,
					delay: config.delay,
					first,
					track,
					decoder,
				})
			},
		)
		.await
	}

	/// Open the first of `candidates` that resolves and decodes.
	async fn open_audio(&self, source: &moq_mux::Source, candidates: Vec<AudioCandidate>) -> Opened<AudioReady> {
		open_first(
			source,
			"audio",
			candidates,
			|rendition, name, config, reference| async move {
				let mut decode = moq_audio::decode::Options::new();
				decode.start = moq_audio::decode::Start::Latest;
				// Floored: the speaker holds at least AUDIO_BUFFER_MIN whatever was
				// asked for, so a smaller budget would skip a group the playhead could
				// still have reached, and would size the hole fill below to a playhead
				// that does not exist.
				decode.delay = moq_audio::decode::Delay {
					min: Some(self.args.fixed_delay().unwrap_or_default()),
					max: Some(self.args.max_delay().max(AUDIO_BUFFER_MIN)),
				};
				decode.estimate = self.args.fixed_delay().is_none();
				// The sink and the frame-duration math below both assume f32,
				// so ask for it rather than inheriting the decoder default.
				decode.output.format = moq_audio::Format::F32;
				let consumer = moq_audio::decode::Consumer::new(&rendition, &config, &name, decode).await?;
				Ok(AudioReady {
					name,
					reference,
					delay: config.delay,
					consumer,
				})
			},
		)
		.await
	}
}

/// Open the first of `candidates`, in rank order, that `open` takes, trying each once its
/// broadcast resolves. A rendition on a broadcast nothing has answered for yet holds back none
/// ranked below it whose broadcast already resolved, so a pending sibling never blanks a half
/// that has something to play: the lower one is tried, and a better one resolving meanwhile is
/// tried beside it rather than in its place, so a better broadcast that announces before its
/// track answers leaves the lower one free to play. The first attempt to open wins, reporting
/// any better rendition still opening or pending as outranking it. Only with every broadcast
/// left pending does it wait. A rendition pointing at a broadcast we can't reach is that
/// rendition's problem, not the catalog's: it falls through to the next like an unsupported
/// codec does.
async fn open_first<C, T, F>(
	source: &moq_mux::Source,
	kind: &str,
	candidates: Vec<(String, C, Track)>,
	mut open: impl FnMut(moq_net::broadcast::Consumer, String, C, Track) -> F,
) -> Opened<T>
where
	C: hang::catalog::Reference + Clone,
	F: Future<Output = anyhow::Result<T>>,
{
	/// Where a candidate is in being tried.
	#[derive(Clone, Copy, PartialEq, Eq)]
	enum Attempt {
		/// Not tried yet, as its broadcast may still be pending.
		Waiting,
		Opening,
		Rejected,
	}

	let mut rejected = Vec::new();
	let mut reject = |name: &str, err: &dyn std::fmt::Display| {
		tracing::warn!(track = name, %err, "cannot play {kind} rendition");
		rejected.push(format!("{kind} `{name}`: {err}"));
	};
	// Each candidate's broadcast request, issued up front so every one is asked at once.
	let mut bound = Vec::new();
	for (name, config, reference) in candidates {
		match source.bind(config.broadcast(), config.identity()) {
			Ok(binding) => bound.push((name, config, reference, binding, Attempt::Waiting)),
			Err(err) => reject(&name, &err),
		}
	}
	// The rank of the best attempt opening: only a candidate ranked above it is worth starting.
	let best = |bound: &[(String, C, Track, moq_mux::Binding, Attempt)]| {
		bound
			.iter()
			.position(|(.., attempt)| *attempt == Attempt::Opening)
			.unwrap_or(bound.len())
	};
	let mut attempts = futures::stream::FuturesUnordered::new();
	loop {
		// Start the best candidate whose broadcast answered, if it ranks above every attempt
		// opening; any ranked above it are still pending.
		let above = best(&bound);
		for (index, (name, config, reference, binding, attempt)) in bound[..above].iter_mut().enumerate() {
			if *attempt != Attempt::Waiting {
				continue;
			}
			match binding.poll_broadcast(&kio::Waiter::noop()) {
				Poll::Pending => {}
				Poll::Ready(Err(err)) => {
					*attempt = Attempt::Rejected;
					reject(name, &err);
				}
				Poll::Ready(Ok(rendition)) => {
					*attempt = Attempt::Opening;
					let opening = open(rendition, name.clone(), config.clone(), reference.clone());
					attempts.push(Box::pin(async move { (index, opening.await) }));
					break;
				}
			}
		}
		let above = best(&bound);
		let watching = bound[..above].iter().any(|(.., attempt)| *attempt == Attempt::Waiting);
		if attempts.is_empty() && !watching {
			return Opened {
				ready: None,
				rejected,
				outranked: None,
			};
		}
		let finished = tokio::select! {
			Some(finished) = futures::StreamExt::next(&mut attempts), if !attempts.is_empty() => Some(finished),
			// A better rendition's broadcast landed, so it opens beside any attempt under way.
			() = kio::wait(|waiter| poll_any(
				bound[..above]
					.iter()
					.filter(|(.., attempt)| *attempt == Attempt::Waiting)
					.map(|(.., binding, _)| binding),
				waiter,
			)), if watching => None,
		};
		match finished {
			None => {}
			Some((index, Ok(ready))) => {
				// Dropping the attempts still opening cancels them. A better one among them has
				// landed, so it outranks what opened, and its upgrade tries it again at once.
				let pending: Vec<_> = bound
					.drain(..index)
					.filter(|(.., attempt)| *attempt != Attempt::Rejected)
					.map(|(.., binding, _)| binding)
					.collect();
				return Opened {
					ready: Some(ready),
					rejected,
					outranked: (!pending.is_empty()).then(|| Outranked(Arc::new(pending))),
				};
			}
			Some((index, Err(err))) => {
				let (name, .., attempt) = &mut bound[index];
				*attempt = Attempt::Rejected;
				reject(name, &format!("{err:#}"));
			}
		}
	}
}

/// Ready once any of `bindings` resolves, whether or not to a broadcast it can play.
fn poll_any<'a>(bindings: impl IntoIterator<Item = &'a moq_mux::Binding>, waiter: &kio::Waiter) -> Poll<()> {
	for binding in bindings {
		if binding.poll_broadcast(waiter).is_ready() {
			return Poll::Ready(());
		}
	}
	Poll::Pending
}

/// The broadcast requests of renditions ranked above the one a half plays, still pending when
/// it was picked. Held so the requests stay open, and so the half picks again once one lands.
#[derive(Clone)]
struct Outranked(Arc<Vec<moq_mux::Binding>>);

impl Outranked {
	/// Resolves once a better-ranked rendition's broadcast lands.
	async fn landed(self) {
		kio::wait(|waiter| poll_any(self.0.iter(), waiter)).await
	}
}

/// A video rendition to try: its name, its config, and the track it plays from.
type VideoCandidate = (String, hang::catalog::VideoConfig, Track);

/// An audio rendition to try: its name, its config, and the track it plays from.
type AudioCandidate = (String, hang::catalog::AudioConfig, Track);

/// Whether two picks try the same renditions in the same order. A rendition is its name and
/// track, which always carry the same content, so a newer snapshot that only updates its hints,
/// such as a measured bitrate or jitter, keeps the opening rather than starting it over.
fn same_renditions<C>(left: &[(String, C, Track)], right: &[(String, C, Track)]) -> bool {
	left.len() == right.len()
		&& left
			.iter()
			.zip(right)
			.all(|((left, _, left_track), (right, _, right_track))| left == right && left_track == right_track)
}

/// The renditions an opening tries, in order: each one's name, config, and track.
type Order<C> = Arc<Mutex<Vec<(String, C, Track)>>>;

/// A rendition being opened, and the candidates it tries in order, so a pick that would try the
/// same keeps it rather than starting over.
struct Opening<'a, T: Ready> {
	/// Shared with the opening, which rearranges video once the window's renderer is up.
	candidates: Order<T::Config>,
	future: std::pin::Pin<Box<dyn Future<Output = Opened<T>> + Send + 'a>>,
}

/// A rendition opened and ready to play.
trait Ready {
	/// The catalog config it is opened from.
	type Config;
}

impl Ready for VideoReady {
	type Config = hang::catalog::VideoConfig;
}

impl Ready for AudioReady {
	type Config = hang::catalog::AudioConfig;
}

/// What opening a rendition came to.
struct Opened<T> {
	/// The first candidate that opened, if any did.
	ready: Option<T>,
	/// Why each candidate before it didn't.
	rejected: Vec<String>,
	/// The candidates ranked above `ready` whose broadcasts are still pending.
	outranked: Option<Outranked>,
}

/// A video rendition subscribed, answering, and with its decoder open, ready to play.
struct VideoReady {
	name: String,
	/// How it is shown.
	shape: Option<Orientation>,
	reference: Track,
	/// The catalog `delay` it asks for.
	delay: Option<Duration>,
	/// What the track answered with, which the rendition plays first.
	first: moq_mux::container::Read,
	track: moq_mux::container::Consumer<catalog::hang::Container>,
	decoder: moq_video::decode::Sink,
}

/// An audio rendition with its decoder open, ready to play.
struct AudioReady {
	name: String,
	reference: Track,
	/// The catalog `delay` it asks for.
	delay: Option<Duration>,
	consumer: moq_audio::decode::Consumer,
}

/// Fail with why nothing plays once renditions were on offer and not one of them
/// opened, with nothing already running or opening to fall back on.
fn unplayable(idle: bool, rejected: &[String]) -> anyhow::Result<()> {
	if idle && !rejected.is_empty() {
		return Err(Unplayable(format!("no playable rendition in the catalog: {}", rejected.join("; "))).into());
	}
	Ok(())
}

/// Drive `play` until it ends or `stopped` fires, reporting whether it ended on its
/// own. Decided by whichever branch won, not by who holds the stop handle: a
/// rendition can be disabled after its track ended but before the end is joined.
async fn until_stopped<T>(
	play: impl Future<Output = anyhow::Result<T>>,
	stopped: impl Future,
) -> anyhow::Result<(bool, Option<T>)> {
	tokio::select! {
		result = play => result.map(|left| (true, Some(left))),
		_ = stopped => Ok((false, None)),
	}
}

/// The rendition a half is playing, and what ends it. Dropping it stops the rendition.
struct Demand {
	name: String,
	/// Dropped with the demand, which stops the rendition.
	_stop: tokio::sync::oneshot::Sender<()>,
	/// The catalog `delay` the rendition holds, `None` under a fixed `--delay`.
	delay: tokio::sync::watch::Sender<Option<Duration>>,
}

impl Demand {
	/// Hold the rendition's newest catalog `delay`, unless the delay is fixed.
	/// It only grows, so a stale snapshot never lowers it.
	fn hold(&self, args: &Args, delay: Option<Duration>) {
		// Capped at the deepest delay a sink holds, so a hostile catalog cannot make
		// playback retain media without bound.
		let delay = delay
			.filter(|_| args.fixed_delay().is_none())
			.map(|delay| delay.min(moq_mux::container::DELAY_MAX));
		self.delay.send_if_modified(|held| {
			let raised = delay > *held;
			if raised {
				*held = delay;
			}
			raised
		});
	}
}

/// The audio rendition a half is playing: what ends it, and what ends it as though its track
/// finished, playing out what its sink holds, once the catalog moves it.
struct Listening {
	demand: Demand,
	retire: Arc<tokio::sync::Notify>,
	/// Better-ranked renditions whose broadcasts were still pending when this one was picked.
	outranked: Option<Outranked>,
}

/// The video rendition a half is playing: what ends it, and how it is shown, kept for when a
/// snapshot drops its entry.
struct Showing {
	demand: Demand,
	shape: Option<Orientation>,
	/// Better-ranked renditions whose broadcasts were still pending when this one was picked.
	outranked: Option<Outranked>,
}

/// Whether `config` is shown in the orientation opposite the viewport's. A square or unsized
/// rendition suits either, and every one suits a square viewport or none.
fn opposed(video: &hang::catalog::Video, config: &hang::catalog::VideoConfig, viewport: Option<Orientation>) -> bool {
	viewport.is_some_and(|viewport| video.orientation(config).is_some_and(|shown| shown != viewport))
}

/// The renditions to try, given in rank order, those shaped like the viewport first, then the
/// rest as a decode fallback, so `preference` ranks within an orientation. A `turning` pick keeps
/// only those shaped like it.
fn arrange(
	video: &hang::catalog::Video,
	mut candidates: Vec<VideoCandidate>,
	viewport: Option<Orientation>,
	turning: bool,
) -> Vec<VideoCandidate> {
	if turning {
		candidates.retain(|(_, config, _)| !opposed(video, config, viewport));
	}
	// Stable, so rank order holds within each side.
	candidates.sort_by_key(|(_, config, _)| opposed(video, config, viewport));
	candidates
}

/// Whether the viewport turned away from the `playing` rendition while one shaped like it, and
/// `available`, is on offer, which now ranks first. `shown` is how the rendition was shown when
/// it started, for a snapshot that no longer lists it.
fn turned_away(
	video: &hang::catalog::Video,
	playing: &str,
	shown: Option<Orientation>,
	viewport: Option<Orientation>,
	available: impl Fn(&str, &hang::catalog::VideoConfig) -> bool,
) -> bool {
	let shown = video
		.renditions
		.get(playing)
		.map_or(shown, |config| video.orientation(config));
	viewport.is_some_and(|viewport| shown.is_some_and(|shown| shown != viewport))
		&& video
			.enabled()
			.any(|(name, config)| !opposed(video, config, viewport) && available(name, config))
}

/// A broadcast this build cannot play, which ends the player. A broadcast that
/// ends or fails on the wire does not: the next announcement plays again.
#[derive(Debug)]
struct Unplayable(String);

impl std::fmt::Display for Unplayable {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.write_str(&self.0)
	}
}

impl std::error::Error for Unplayable {}

struct AudioPlayback<O: Output> {
	changed: Arc<tokio::sync::Notify>,
	speaker: O::Speaker,
	presentation: Presentation,
	playhead: Playhead,
	/// The depth the sink opens on, which also sizes its ring.
	latency: Duration,
	output: O,
	/// The rendition's catalog `delay`, added to the jitter target this track holds.
	delay: tokio::sync::watch::Receiver<Option<Duration>>,
	/// How far audio may trail the clock before it skips ahead; zero turns the time stretch off.
	stretch: Duration,
}

/// Play a track until it ends, or until `retired` resolves, which ends it where
/// it stands. Either way it hands back the sink with the delay it still holds.
async fn play_audio<O: Output>(
	mut consumer: moq_audio::decode::Consumer,
	playback: AudioPlayback<O>,
	retired: impl Future<Output = ()>,
) -> anyhow::Result<<O::Speaker as Speaker>::Sink> {
	let AudioPlayback {
		changed,
		speaker,
		presentation,
		playhead,
		latency,
		output,
		delay: stated,
		stretch,
	} = playback;

	// The playout delay is the consumer's to size, from how unevenly packets arrive,
	// and the sink is where it lives: a sample handed over now sounds that much
	// later. That estimate times each packet when `read` hands it over, so this
	// receiver never waits on the speaker between reads. Live playback lets
	// the callback stretch toward the target; a declared end plays comfort noise
	// until the next utterance is due. Each write anchors
	// the sink to when the shared clock says its media is due, so the callback
	// plays onto that clock in the steady state without letting a stall hold video.
	let role = presentation.track(moq_mux::container::Kind::Audio);
	let received = Mutex::new(VecDeque::new());
	let arrived = tokio::sync::Notify::new();
	let popped = tokio::sync::Notify::new();
	let max_delay = consumer.max_delay();
	let estimate = consumer.options().estimate;
	let sample_rate = consumer.sample_rate();
	let layout = consumer.layout();
	let channels = layout.channels();
	let mut input = moq_audio::playback::Input::default();
	input.format = moq_audio::Format::F32;
	input.sample_rate = sample_rate;
	input.layout = layout;
	// The floor the sink opens on, and what sizes its ring, so a fixed delay
	// must fit from setup. An estimated target moves: update the callback's
	// target on each write and let it stretch received PCM to converge.
	input.latency = latency;
	let mut sink = speaker.sink(input.clone())?;

	// One sample across every channel, the unit a write has to stay aligned to.
	let stride = channels as usize * size_of::<f32>();
	let samples = |duration: Duration| (duration.as_secs_f64() * sample_rate as f64).round() as u64;
	let mut silence = Vec::new();

	// The most the sink holds, within what its ring was sized for. A shared delay
	// past it, such as a slow video encoder's, stays encoded in the consumer until it is due instead, so the
	// ring never grows with the delay.
	let cap = max_delay
		.max(AUDIO_BUFFER_MIN)
		.min(latency + SINK_ROOM)
		.min(moq_mux::container::DELAY_MAX);

	// The longest hole worth playing through, in samples. A hole this player would
	// rather sit through is one it is already willing to buffer, which is what the
	// decoder's latency budget says, within what the sink holds: anything longer
	// is what that budget chose to skip, or silence the ring would drop along with
	// the frame after it. Past it the sink skips the hole and the clock re-anchors.
	let fill_max = samples(cap);

	let mut timeline = AudioTimeline::default();

	// The device's output latency the clock holds for.
	let peak = Mutex::new(Peak::default());
	// Audio is placed on the clock where it is heard, so the device's output
	// latency is part of the delay video waits for too. A fixed delay is floored
	// at it instead, since audio cannot sound any sooner. Capped as the catalog
	// delay is, so a hostile advertised jitter cannot make playback retain media
	// without bound.
	let audible = |delay: Duration| {
		let device = peak.lock().unwrap().peak();
		if estimate {
			delay + device.unwrap_or_default()
		} else {
			delay.max(device.unwrap_or(AUDIO_BUFFER_MIN))
		}
		.min(moq_mux::container::DELAY_MAX)
	};

	// How far ahead of its due time a write may sound: what the sink holds, at
	// most its cap, plus the device's latency after it.
	let reach = || cap + peak.lock().unwrap().peak().unwrap_or_default();

	// A replacement's opening cushion and its timeline plan must agree. The
	// callback now owns target convergence, rather than write-time padding
	// subtracting the incoming frame before it reaches the sink. Like a hole,
	// the cushion is silence the sink must hold, and a write ahead of its due
	// time leads it by the device's latency too.
	let lead = |delay: Duration| latency.max(audible(delay).min(reach()).max(AUDIO_BUFFER_MIN));

	let schedule = presentation.consume();
	// The consumer decodes a packet once the sink has room for it. Its playhead is
	// where the shared clock says the last write sounds, rather than what the sink
	// is sounding: audio follows that clock, so a raised delay holds media encoded
	// until it is due instead of decoding toward a sink that would play it early.
	let backpressure = consumer.backpressure();
	let steer = |wrote: Option<(moq_net::Timestamp, Duration)>| {
		let shared = schedule.delay();
		backpressure.set_latency(shared.min(cap), shared.saturating_sub(cap));
		if let Some((end, mapped)) = wrote
			&& let Some(due) = moq_net::Timestamp::try_from(mapped)
				.ok()
				.and_then(|end| presentation.due(end))
		{
			backpressure.observe(end, due);
		}
	};
	steer(None);
	// What the sink holds: the shared delay, which counts the device's own
	// latency, never below that latency, and capped last at what the sink was
	// sized for, so a device reporting more than that cannot get it refused.
	let target = |sink: &<O::Speaker as Speaker>::Sink| {
		schedule
			.delay()
			.max(sink.output_latency().unwrap_or(AUDIO_BUFFER_MIN))
			.min(cap)
	};
	// Anchor the sink to the shared clock: the media written so far ends at
	// `end`, and sounds when the clock says it is due. The callback stretches
	// onto that clock by its lag alone, and skips onto it what trails it past
	// the stretch.
	let anchor = |sink: &mut <O::Speaker as Speaker>::Sink, end: Duration| {
		if let Some(at) = moq_net::Timestamp::try_from(end)
			.ok()
			.and_then(|end| presentation.due(end))
		{
			sink.due(at, stretch);
		}
	};

	// Tracks whether the last read failed, so a stream the decoder can't read at
	// all logs once rather than once per packet.
	let mut dropping = false;

	// The jitter target the last write held, so a rise in the stated delay applies
	// at once rather than waiting on the next write.
	let jitter = Mutex::new(None);
	// The jitter target and the catalog delay are each capped, and so is their sum,
	// so one ceiling bounds the clock however the publisher splits its advertisement.
	let capped = |jitter: Duration, delay: Option<Duration>| {
		(jitter + delay.unwrap_or_default()).min(moq_mux::container::DELAY_MAX)
	};
	let retarget = |delay: Option<Duration>| {
		let Some(jitter) = *jitter.lock().unwrap() else { return };
		if role.set_target(capped(jitter, delay)) {
			changed.notify_one();
			output.send(Event::Wake);
		}
	};
	let receive = async {
		let mut stated = stated.clone();
		let mut retired = std::pin::pin!(retired);
		// Media went missing since the last frame, so the next one does not continue it.
		let mut lost = false;
		// Declared ends no frame has crossed yet. A short utterance can declare its
		// end before its first PCM, so ends queue until a frame crosses them.
		let mut ends = VecDeque::new();
		// The subscription reaches the estimator's ceiling past the shared delay wherever that
		// moves, never below what it opened with, so every frame late enough to move the target
		// is observed, whatever the stretch.
		let floor = consumer.options().delay.max.unwrap_or_default();
		let mut asked = floor;
		loop {
			let seen = schedule.delay();
			let max = seen.saturating_add(REACH).max(floor);
			if max != asked {
				consumer.set_max_delay(max);
				asked = max;
			}
			// Stop reading while play is held with more than the age budget queued,
			// so the subscription evicts the excess instead of this queue growing
			// without bound.
			while received
				.lock()
				.unwrap()
				.front()
				.is_some_and(|(.., queued): &(_, _, _, Instant)| queued.elapsed() > max_delay + schedule.delay())
			{
				tokio::select! {
					() = popped.notified() => {}
					Ok(()) = stated.changed() => retarget(*stated.borrow_and_update()),
				}
			}
			let event = tokio::select! {
				event = consumer.read() => event,
				() = &mut retired => Ok(None),
				Ok(()) = stated.changed() => {
					retarget(*stated.borrow_and_update());
					continue;
				}
				_ = role.delay_changed(seen) => continue,
			};
			let event = match event {
				Err(err) if !matches!(err, moq_audio::Error::Decode(_)) => return Err(err.into()),
				event => event,
			};
			let ended = matches!(event, Ok(None));
			let delay = consumer.delay();
			let crossing = match &event {
				Ok(Some(moq_audio::decode::Read::End { end, .. })) => {
					ends.push_back(Duration::try_from(*end)?);
					None
				}
				// Playback restarts at the frame after it, so that is what carries it.
				Ok(Some(moq_audio::decode::Read::Gap { .. })) => {
					lost = true;
					continue;
				}
				Ok(Some(moq_audio::decode::Read::Frame(frame))) => {
					let lost = std::mem::take(&mut lost);
					// A frame at or past an end comes from the encoder that replaced it; one
					// before it is the stopped encoder's drain. Ends it crosses together
					// closed utterances with no media.
					let at = Duration::try_from(frame.timestamp)?;
					let mut crossed = false;
					while ends.front().is_some_and(|end| at >= *end) {
						ends.pop_front();
						crossed = true;
					}
					match crossed {
						true => Some(Crossing::End {
							ahead: !ends.is_empty(),
						}),
						false if lost => Some(Crossing::Loss),
						false => None,
					}
				}
				_ => None,
			};
			received.lock().unwrap().push_back((
				event,
				delay,
				crossing,
				consumer.arrival().unwrap_or_else(Instant::now),
			));
			arrived.notify_one();
			if ended {
				return Ok::<_, anyhow::Error>(());
			}
		}
	};
	let play = async {
		let mut catalog = stated.clone();
		// Sleep until `deadline`, recomputing it whenever the shared or the catalog delay
		// changes, so a change while asleep never leaves audio on a stale deadline.
		// Reports whether it slept at all.
		let wait = async |catalog: &mut tokio::sync::watch::Receiver<Option<Duration>>,
		                  deadline: &(dyn Fn() -> Option<Instant> + Sync)| {
			let mut slept = false;
			loop {
				let seen = schedule.delay();
				let Some(at) = deadline().filter(|at| *at > Instant::now()) else {
					return slept;
				};
				slept = true;
				tokio::select! {
					() = kio::clock::sleep_until(at) => {}
					_ = role.delay_changed(seen) => {}
					Ok(()) = catalog.changed() => retarget(*catalog.borrow_and_update()),
				}
			}
		};
		// The sink was told its audio ends in a declared silence and has not been told what follows.
		let mut silent = false;
		// The frame being written starts an utterance whose end is already declared.
		let mut declare = false;
		// The frame being written starts the utterance after a declared silence.
		let mut utterance = false;
		// The last frame written was trimmed, so a trim of this one continues that skip.
		let mut trimming = false;
		// Where the media written so far ends, on the consumer's timeline and mapped.
		let mut wrote: Option<(moq_net::Timestamp, Duration)> = None;
		// The next write starts this sink, which plays it one opening cushion out.
		let mut fresh = true;
		loop {
			let (event, delay, crossing, arrived_at) = loop {
				if let Some(event) = received.lock().unwrap().pop_front() {
					popped.notify_one();
					break event;
				}
				let seen = schedule.delay();
				tokio::select! {
					() = arrived.notified() => {}
					// No write may come to carry a delay change, so the speaker follows it now.
					_ = role.delay_changed(seen) => {
						steer(wrote);
						sink.set_latency(target(&sink))?;
						if let Some((_, end)) = wrote {
							anchor(&mut sink, end);
						}
					}
				}
			};
			// A replacement sink must open with the current cushion, not the
			// old floor and a target update after its initial silence was fixed.
			input.latency = lead(delay);
			let frame = match event {
				Ok(Some(moq_audio::decode::Read::Frame(frame))) => {
					match crossing {
						Some(Crossing::Loss) => {
							// What follows a declared silence starts the next utterance however many
							// groups were lost on the way, rather than filling the silence as a hole.
							if std::mem::take(&mut silent) {
								timeline = AudioTimeline::default();
								sink.resume();
								utterance = true;
							}
						}
						Some(Crossing::End { ahead }) => {
							// A declared boundary preserves the old sink, unlike an undeclared hole:
							// its tail and then comfort noise play until this frame is due. The
							// timestamps kept moving through the silence, so the clock carries on.
							declare = ahead;
							timeline = AudioTimeline::default();
							silent = false;
							sink.resume();
							utterance = true;
						}
						None => {}
					}
					frame
				}
				// An end read while already silent belongs to an utterance still to start,
				// which its first frame's crossing carries.
				Ok(Some(moq_audio::decode::Read::End { .. })) => {
					// The stopped encoder's final frames may follow, and play before the silence.
					if !silent {
						sink.silence();
						silent = true;
					}
					continue;
				}
				// Never queued: receive folds it into the next frame's crossing.
				Ok(Some(moq_audio::decode::Read::Gap { .. })) => continue,
				// A read this player predates has nothing to play.
				Ok(Some(_)) => continue,
				Ok(None) => break,
				// One bad packet is that packet's problem: the decoder stays usable, so
				// skip it rather than ending playback and taking the video window down
				// with it.
				Err(err @ moq_audio::Error::Decode(_)) => {
					if dropping {
						tracing::debug!(%err, "dropping an audio frame");
					} else {
						tracing::warn!(%err, "dropping an audio frame");
						dropping = true;
					}
					continue;
				}
				Err(err) => return Err(err.into()),
			};
			dropping = false;

			let length = frame.data.len() / stride;
			let start = Duration::try_from(frame.timestamp)?;
			let timing = timeline.push(start, length, sample_rate, fill_max);

			// A rewind or a hole too large to fill starts a new playback sink. The old
			// sink has no media clock, so its buffered audio cannot be carried across a
			// timeline region the player skipped.
			if timing.reset_sink {
				silent = false;
				drop(sink);
				// The media either side of the break is unrelated, so its arrivals
				// stop holding the clock and the playhead starts over.
				role.reset();
				playhead.restart();
				sink = speaker.sink(input.clone())?;
				fresh = true;
			}

			// A hole in the media is a hole in the audio, not a splice. Handing the next
			// frame straight to the speaker shortens the track by the missing duration,
			// which leaves it running ahead of media time until the clock below
			// re-anchors, taking the video with it. So the hole goes in as silence ahead
			// of the frame, and the pair is fitted to the target as one write.
			// The arrival, not the write: how far the speaker trails is what the clock
			// steers, never what moves it. The target is this track's own jitter
			// target, which playback holds unless another track asks for more.
			let mut moved = role.received(frame.timestamp, arrived_at);
			// The publisher's encoder delay is on top of the arrival jitter, so the
			// clock holds both while the sink keeps sizing for jitter alone. The
			// clock holds the whole measured target, which the subscription follows,
			// though the sink holds no more of it than its cap: the rest stays encoded.
			peak.lock().unwrap().observe(sink.output_latency(), Instant::now());
			let held = audible(delay);
			*jitter.lock().unwrap() = Some(held);
			moved |= role.set_target(capped(held, *stated.borrow()));
			// The consumer releases media due further out than the sink holds only
			// when its timeline ends before the media behind it covers the hold, so
			// that little waits here instead of overflowing the sink.
			let held_back = wait(&mut catalog, &|| {
				presentation
					.due(frame.timestamp)
					.and_then(|due| due.checked_sub(reach()))
			})
			.await;
			// A fresh sink plays its first write behind an opening cushion sized before
			// the delay was known, and a held frame behind only what was already
			// written, so the rest of the lead goes in as silence ahead of the frame,
			// which already covers any hole before it. A frame starting an utterance is
			// instead placed one target out by the sink itself.
			let short = presentation
				.due(frame.timestamp)
				.filter(|_| (fresh || held_back) && !utterance)
				.map(|due| {
					due.saturating_duration_since(Instant::now())
						.saturating_sub(sink.buffered())
				})
				.map_or(0, samples);
			// Convergence is the callback's: it slows down when its buffer runs low and
			// catches up with the clock once it holds more than its target, and only
			// skips what trails the clock past what the subscription still delivers.
			sink.set_latency(target(&sink))?;

			// The packet pays for the channel count; the catalog cannot allocate
			// a duration's worth of silence before any samples arrive.
			silence.resize(length.min(AUDIO_SILENCE) * stride, 0);
			let mut quiet = timing.silence.max(short);
			while quiet > 0 && !silence.is_empty() {
				let frames = quiet.min((silence.len() / stride) as u64) as usize;
				let written = sink.write(&silence[..frames * stride])?;
				// Once full, padding is dropped just like live media. A catalog's
				// rate must not make us walk seconds of refused input samples.
				if written.dropped_sample_frames != 0 {
					break;
				}
				quiet -= frames as u64;
			}
			let written = sink.write(&frame.data)?;
			anchor(&mut sink, timing.end);
			let trimmed = written.dropped_sample_frames as u64;
			if trimmed > 0 {
				output.send(Event::Trimmed {
					audio: Duration::from_secs_f64(trimmed as f64 / sample_rate as f64),
					skip: !std::mem::replace(&mut trimming, true),
				});
			} else {
				trimming = false;
			}
			// A dropped write leaves the utterance to start on the next frame the sink accepts.
			if utterance && written.accepted_sample_frames > 0 {
				utterance = false;
				output.send(Event::Utterance(Duration::try_from(frame.timestamp)?));
			}
			if std::mem::take(&mut declare) {
				sink.silence();
				silent = true;
			}

			if written.accepted_sample_frames > 0 {
				fresh = false;
			}
			let end =
				Duration::try_from(frame.timestamp)? + Duration::from_secs_f64(length as f64 / sample_rate as f64);
			wrote = moq_net::Timestamp::try_from(end).ok().map(|end| (end, timing.end));
			steer(wrote);
			playhead.wrote(timing.end, sink.buffered(), Instant::now());
			// A move has to wake the window: it is asleep on a deadline computed from
			// the old schedule, and every queued frame is due earlier now.
			if moved {
				changed.notify_one();
				output.send(Event::Wake);
			}
		}

		Ok::<_, anyhow::Error>(sink)
	};
	let (_, sink) = tokio::try_join!(receive, play)?;
	Ok(sink)
}

/// The highest output latency the device reported within [`OUTPUT_WINDOW`].
#[derive(Default)]
struct Peak {
	/// Each reading that no later one is at least as high as, oldest first, so
	/// the front is the peak.
	readings: VecDeque<(Instant, Duration)>,
}

impl Peak {
	/// Fold in the latency the device reports now, if any, returning the peak.
	fn observe(&mut self, latency: Option<Duration>, now: Instant) -> Option<Duration> {
		if let Some(latency) = latency {
			while self.readings.back().is_some_and(|&(_, last)| last <= latency) {
				self.readings.pop_back();
			}
			self.readings.push_back((now, latency));
		}
		while self
			.readings
			.front()
			.is_some_and(|&(at, _)| now.saturating_duration_since(at) > OUTPUT_WINDOW)
			&& self.readings.len() > 1
		{
			self.readings.pop_front();
		}
		self.peak()
	}

	/// The peak as of the last reading.
	fn peak(&self) -> Option<Duration> {
		self.readings.front().map(|&(_, latency)| latency)
	}
}

/// Play out what a retired sink still holds, instead of cutting the tail off
/// by dropping it.
async fn drain(sink: impl Sink) {
	// The estimated target moves, so the depth the sink was opened with says
	// nothing about what it holds now: read it at retirement instead.
	let _ = kio::clock::timeout(sink.buffered() + AUDIO_DRAIN_GRACE, sink.finish()).await;
}

#[cfg(test)]
mod tests {
	use bytes::Bytes;
	use hang::catalog::{AudioCodec, AudioConfig};
	use moq_mux::catalog::hang::Container;

	use super::*;

	/// A landscape program ranked first, beside a lower-ranked portrait one and a square one.
	fn oriented() -> hang::catalog::Video {
		let mut video = hang::catalog::Video::default();
		for (name, width, height, preference) in [
			("wide", 1920, 1080, 0),
			("wide/360p", 640, 360, 0),
			("tall", 1080, 1920, -1),
			("square", 720, 720, -2),
		] {
			let mut config = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8);
			config.coded_width = Some(width);
			config.coded_height = Some(height);
			config.preference = preference;
			video.insert(name, config).unwrap();
		}
		video
	}

	#[test]
	fn renditions_shaped_like_the_viewport_come_first() {
		let video = oriented();
		let ranked = |video: &hang::catalog::Video| {
			video
				.enabled()
				.map(|(name, config)| (name.clone(), config.clone(), Track::named(name)))
				.collect::<Vec<_>>()
		};
		let arranged = |viewport, turning| {
			arrange(&video, ranked(&video), viewport, turning)
				.into_iter()
				.map(|(name, _, _)| name)
				.collect::<Vec<_>>()
		};
		let names = |viewport| arranged(viewport, false);
		assert_eq!(names(None), ["wide", "wide/360p", "tall", "square"]);
		// A square rendition suits either way, so it passes the opposite one.
		assert_eq!(
			names(Some(Orientation::Landscape)),
			["wide", "wide/360p", "square", "tall"]
		);
		assert_eq!(
			names(Some(Orientation::Portrait)),
			["tall", "square", "wide", "wide/360p"]
		);
		// A turn tries only those shaped like the window.
		assert_eq!(arranged(Some(Orientation::Portrait), true), ["tall", "square"]);
	}

	#[test]
	fn turning_away_picks_again_only_with_a_rendition_to_turn_to() {
		let mut video = oriented();
		let all = |_: &str, _: &hang::catalog::VideoConfig| true;
		let wide = Some(Orientation::Landscape);
		assert!(turned_away(&video, "wide", None, Some(Orientation::Portrait), all));
		assert!(!turned_away(&video, "wide", None, Some(Orientation::Landscape), all));
		assert!(!turned_away(&video, "wide", None, None, all));
		// A square rendition suits either way.
		assert!(!turned_away(&video, "square", None, Some(Orientation::Landscape), all));
		// The catalog's entry outranks the shape it started with.
		assert!(!turned_away(&video, "square", wide, Some(Orientation::Portrait), all));
		// One the snapshot no longer lists keeps the shape it started with.
		assert!(turned_away(&video, "gone", wide, Some(Orientation::Portrait), all));
		assert!(!turned_away(&video, "gone", None, Some(Orientation::Portrait), all));

		// Nothing to turn to once the portrait-friendly ones are disabled or unavailable, such as
		// finished or on a broadcast not yet in hand.
		let available = |name: &str, _: &hang::catalog::VideoConfig| name != "square";
		video.renditions.get_mut("tall").unwrap().enabled = false;
		assert!(!turned_away(
			&video,
			"wide",
			None,
			Some(Orientation::Portrait),
			available
		));
	}
	use crate::play::args::Delay;
	use crate::play::fake::Recorder;

	const SAMPLE_RATE: u32 = 48_000;
	/// Samples per packet.
	const PACKET: u64 = 960;
	const PACKET_DURATION: Duration = Duration::from_millis(20);

	#[tokio::test(start_paused = true)]
	async fn an_unfed_catalog_needs_no_silence_buffer() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let _track = broadcast
			.create_track("audio", hang::container::track_info(hang::catalog::PRIORITY.audio))
			.unwrap();
		let consumer = moq_audio::decode::Consumer::new(
			&broadcast.consume(),
			&AudioConfig::new(AudioCodec::Pcm, 4_000_000_000, 8),
			"audio",
			moq_audio::decode::Options::new(),
		)
		.await
		.unwrap();
		let recorder = Recorder::default();
		let playback = AudioPlayback {
			delay: tokio::sync::watch::channel(None).1,
			changed: Default::default(),
			speaker: recorder.clone(),
			presentation: Presentation::new(AUDIO_BUFFER_MIN),
			playhead: Default::default(),
			latency: AUDIO_BUFFER_MIN,
			output: recorder,
			stretch: Duration::ZERO,
		};
		let _sink = play_audio(consumer, playback, std::future::ready(())).await.unwrap();
	}

	#[tokio::test]
	async fn a_quick_audio_resume_preserves_the_old_speaker_tail() {
		tokio::time::pause();
		let delay = Duration::from_millis(200);
		let broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track("audio", hang::container::track_info(hang::catalog::PRIORITY.audio))
			.unwrap();
		let mut producer = moq_mux::container::Producer::new(track, Container::Legacy(moq_mux::container::Kind::Audio));
		let mut options = moq_audio::decode::Options::new();
		options.delay = moq_audio::decode::Delay {
			min: Some(delay),
			max: Some(delay),
		};
		options.estimate = false;
		options.output.format = moq_audio::Format::F32;
		let consumer = moq_audio::decode::Consumer::new(
			&broadcast.consume(),
			&AudioConfig::new(AudioCodec::Pcm, SAMPLE_RATE, 1),
			"audio",
			options,
		)
		.await
		.unwrap();
		let recorder = Recorder::default();
		let playback = AudioPlayback {
			delay: tokio::sync::watch::channel(None).1,
			changed: Default::default(),
			speaker: recorder.clone(),
			presentation: Presentation::new(delay),
			playhead: Default::default(),
			latency: AUDIO_BUFFER_MIN,
			output: recorder.clone(),
			stretch: Duration::ZERO,
		};
		let task = tokio::spawn(async move {
			play_audio(consumer, playback, std::future::pending())
				.await
				.unwrap()
				.finish()
				.await;
		});
		producer.write(packet(0, 0.25)).unwrap();
		tokio::task::yield_now().await;
		producer.discontinuity().unwrap();
		// The resume starts after the marker: group starts only move forward.
		producer.write(packet(1, 0.5)).unwrap();
		let _finished = producer.finish().unwrap();
		task.await.unwrap();
		// The resume keeps the sink, which fills the declared silence with comfort noise.
		assert_eq!(recorder.latencies(), vec![AUDIO_BUFFER_MIN]);
		let played = recorder.played();
		let old: Vec<_> = played.iter().filter(|part| part.sample == 0.25).collect();
		let new: Vec<_> = played.iter().filter(|part| part.sample == 0.5).collect();
		assert_eq!(
			old.iter().map(|part| part.to - part.from).sum::<Duration>(),
			PACKET_DURATION
		);
		assert_eq!(
			new.iter().map(|part| part.to - part.from).sum::<Duration>(),
			PACKET_DURATION
		);
		assert!(old.last().unwrap().to <= new.first().unwrap().from);
	}

	/// A video encoder delay past audio's own budget still plays audio in sync:
	/// the media waits encoded until it is due rather than sounding early.
	#[tokio::test]
	async fn audio_holds_a_shared_delay_past_its_budget() {
		let budget = Duration::from_millis(200);
		let encoder = Duration::from_secs(1);
		assert_on_time(first_sound(budget, budget, None, Some(encoder)).await, encoder);
	}

	/// A catalog delay the sink can hold still plays audio when it is due: the
	/// sink opens on a cushion sized before the delay was known, so the first
	/// write is placed behind the rest of the lead rather than sounding early.
	#[tokio::test]
	async fn audio_holds_a_catalog_delay_within_its_budget() {
		let jitter = Duration::from_millis(200);
		let catalog = Duration::from_millis(150);
		let sounded = first_sound(jitter, Duration::from_secs(1), Some(catalog), None).await;
		assert_on_time(sounded, jitter + catalog);
	}

	/// A measured target past the budget the track opened with still sets the
	/// clock: the subscription follows the delay, so the budget never caps it.
	#[tokio::test]
	async fn audio_holds_a_target_past_its_opening_budget() {
		let jitter = Duration::from_millis(300);
		assert_on_time(
			first_sound(jitter, Duration::from_millis(100), None, None).await,
			jitter,
		);
	}

	/// A target past the deepest delay a sink holds, such as a hostile advertised
	/// jitter's, holds the clock at that depth instead.
	#[tokio::test]
	async fn audio_caps_a_target_past_the_deepest_delay() {
		let latency = moq_mux::container::DELAY_MAX;
		assert_on_time(
			first_sound(Duration::from_secs(20), Duration::from_secs(5), None, None).await,
			latency,
		);
	}

	/// A capped jitter target plus a capped catalog delay still holds the clock at
	/// one ceiling, the same the browser holds.
	#[tokio::test]
	async fn audio_caps_a_target_plus_catalog_delay_at_the_deepest_delay() {
		let latency = moq_mux::container::DELAY_MAX;
		let hour = Duration::from_secs(3600);
		assert_on_time(first_sound(latency, latency, Some(hour), None).await, latency);
	}

	/// A target past what the sink's ring was sized for, under a budget longer
	/// still, holds the excess encoded rather than failing the sink.
	#[tokio::test]
	async fn audio_holds_a_target_past_its_ring_encoded() {
		let jitter = Duration::from_secs(4);
		assert_on_time(first_sound(jitter, Duration::from_secs(5), None, None).await, jitter);
	}

	/// A hole longer than the sink's ring holds is skipped rather than filled with
	/// silence the ring would drop along with the frame after it.
	#[tokio::test]
	async fn a_hole_past_the_ring_is_skipped() {
		tokio::time::pause();
		let broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track("audio", hang::container::track_info(hang::catalog::PRIORITY.audio))
			.unwrap();
		let mut producer = moq_mux::container::Producer::new(track, Container::Legacy(moq_mux::container::Kind::Audio));
		let mut options = moq_audio::decode::Options::new();
		let jitter = Duration::from_millis(100);
		// A budget past the ring the sink was sized for.
		options.delay = moq_audio::decode::Delay {
			min: Some(jitter),
			max: Some(Duration::from_secs(5)),
		};
		options.estimate = false;
		options.output.format = moq_audio::Format::F32;
		let consumer = moq_audio::decode::Consumer::new(
			&broadcast.consume(),
			&AudioConfig::new(AudioCodec::Pcm, SAMPLE_RATE, 1),
			"audio",
			options,
		)
		.await
		.unwrap();
		let recorder = Recorder::default();
		let (_catalog, stated) = tokio::sync::watch::channel(None);
		let playback = AudioPlayback {
			delay: stated,
			changed: Default::default(),
			speaker: recorder.clone(),
			presentation: Presentation::new(jitter),
			playhead: Default::default(),
			latency: AUDIO_BUFFER_MIN,
			output: recorder.clone(),
			stretch: Duration::from_secs(5),
		};
		let task = tokio::spawn(async move {
			play_audio(consumer, playback, std::future::pending())
				.await
				.unwrap()
				.finish()
				.await;
		});
		for index in 0..30 {
			producer.write(packet(index, 0.25)).unwrap();
			kio::clock::sleep(PACKET_DURATION).await;
		}
		// Three seconds of packets the publisher never sent, then live again.
		let after = 30 + 3000 / PACKET_DURATION.as_millis() as u64;
		producer.write(packet(after, 0.5)).unwrap();
		for index in after + 1..after + 30 {
			kio::clock::sleep(PACKET_DURATION).await;
			producer.write(packet(index, 0.25)).unwrap();
		}
		let _finished = producer.finish().unwrap();
		task.await.unwrap();
		assert!(
			recorder.played().iter().any(|part| part.sample == 0.5),
			"the frame after the hole played"
		);
	}

	fn assert_on_time(sounded: Duration, due: Duration) {
		assert!(
			sounded.max(due) - sounded.min(due) <= Duration::from_millis(1),
			"audio sounded at {sounded:?}, due {due:?}"
		);
	}

	/// How long after it arrives the first packet of a live track sounds, holding
	/// `min` as the jitter target within a `max` budget, plus a catalog and a video delay.
	async fn first_sound(min: Duration, max: Duration, catalog: Option<Duration>, video: Option<Duration>) -> Duration {
		tokio::time::pause();
		let broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track("audio", hang::container::track_info(hang::catalog::PRIORITY.audio))
			.unwrap();
		let mut producer = moq_mux::container::Producer::new(track, Container::Legacy(moq_mux::container::Kind::Audio));
		let mut options = moq_audio::decode::Options::new();
		options.delay = moq_audio::decode::Delay {
			min: Some(min),
			max: Some(max),
		};
		options.estimate = false;
		options.output.format = moq_audio::Format::F32;
		let consumer = moq_audio::decode::Consumer::new(
			&broadcast.consume(),
			&AudioConfig::new(AudioCodec::Pcm, SAMPLE_RATE, 1),
			"audio",
			options,
		)
		.await
		.unwrap();
		let presentation = Presentation::new(min);
		// Held for the whole test: a dropped track stops asking for its delay.
		let _video = video.map(|video| {
			let track = presentation.track(moq_mux::container::Kind::Video);
			track.set_target(video);
			track
		});
		let (_catalog, stated) = tokio::sync::watch::channel(catalog);
		let recorder = Recorder::default();
		let playback = AudioPlayback {
			delay: stated,
			changed: Default::default(),
			speaker: recorder.clone(),
			presentation,
			playhead: Default::default(),
			// What `--delay auto` opens on, well short of the lead the held media needs.
			latency: AUDIO_BUFFER_MIN,
			output: recorder.clone(),
			stretch: Duration::ZERO,
		};
		let task = tokio::spawn(async move {
			play_audio(consumer, playback, std::future::pending())
				.await
				.unwrap()
				.finish()
				.await;
		});
		let start = Instant::now();
		// Live, then a burst the track's end releases at once.
		for index in 0..60 {
			producer.write(packet(index, 0.25)).unwrap();
			kio::clock::sleep(PACKET_DURATION).await;
		}
		for index in 60..65 {
			producer.write(packet(index, 0.25)).unwrap();
		}
		let _finished = producer.finish().unwrap();
		task.await.unwrap();
		let first = recorder
			.played()
			.into_iter()
			.find(|part| part.sample == 0.25)
			.expect("played audio");
		first.from - start
	}

	/// A push-to-talk mute declares its silence and sends nothing until the next
	/// press. The sink carries on through it, so the native ring fills it with
	/// comfort noise, and the next press plays one delay after it arrives.
	#[tokio::test]
	async fn a_mute_keeps_the_sink_and_the_next_press_plays_one_delay_out() {
		mute(false).await;
	}

	/// A group lost after the mute's marker still leaves the next press its own
	/// utterance, rather than a hole filled with two seconds of silence.
	#[tokio::test]
	async fn a_press_after_a_lost_group_still_plays_one_delay_out() {
		mute(true).await;
	}

	async fn mute(lost: bool) {
		tokio::time::pause();
		let delay = Duration::from_millis(300);
		let broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track("audio", hang::container::track_info(hang::catalog::PRIORITY.audio))
			.unwrap();
		let mut producer = moq_mux::container::Producer::new(track, Container::Legacy(moq_mux::container::Kind::Audio));
		let mut options = moq_audio::decode::Options::new();
		options.delay = moq_audio::decode::Delay {
			min: Some(delay),
			max: Some(delay),
		};
		options.estimate = false;
		options.output.format = moq_audio::Format::F32;
		let consumer = moq_audio::decode::Consumer::new(
			&broadcast.consume(),
			&AudioConfig::new(AudioCodec::Pcm, SAMPLE_RATE, 1),
			"audio",
			options,
		)
		.await
		.unwrap();
		let recorder = Recorder::default();
		let playback = AudioPlayback {
			delay: tokio::sync::watch::channel(None).1,
			changed: Default::default(),
			speaker: recorder.clone(),
			presentation: Presentation::new(delay),
			playhead: Default::default(),
			latency: delay,
			output: recorder.clone(),
			stretch: Duration::ZERO,
		};
		let task = tokio::spawn(async move {
			play_audio(consumer, playback, std::future::pending())
				.await
				.unwrap()
				.finish()
				.await;
		});
		for index in 0..10 {
			producer.write(packet(index, 0.25)).unwrap();
			kio::clock::sleep(PACKET_DURATION).await;
		}
		producer.discontinuity().unwrap();
		// A seek creates the press's first group without appending it, so nothing declares the
		// starts between it and the marker skipped: a group there may have been lost.
		if lost {
			producer
				.seek(packet(110, 0.5).timestamp.as_micros().unwrap() as u64)
				.unwrap();
		}
		// Two seconds muted, with the timeline running on through it.
		kio::clock::sleep(Duration::from_secs(2)).await;
		let pressed = Instant::now();
		for index in 110..120 {
			producer.write(packet(index, 0.5)).unwrap();
			kio::clock::sleep(PACKET_DURATION).await;
		}
		let _finished = producer.finish().unwrap();
		task.await.unwrap();

		assert_eq!(recorder.latencies(), vec![delay], "the mute replaced the sink");
		let utterances: Vec<_> = recorder
			.events()
			.into_iter()
			.filter_map(|event| match event {
				Event::Utterance(timestamp) => Some(timestamp),
				_ => None,
			})
			.collect();
		assert_eq!(
			utterances,
			vec![110 * PACKET_DURATION],
			"the press starts from its first frame"
		);
		let played = recorder.played();
		let first = played.iter().find(|part| part.sample == 0.5).unwrap();
		assert_eq!(first.utterance, 1);
		// A local track owes nothing, so the ordered read waits for no lost group.
		assert_eq!(first.from - pressed, delay, "{played:?}");
		let new: Vec<_> = played.iter().filter(|part| part.sample == 0.5).collect();
		assert_eq!(
			new.iter().map(|part| part.to - part.from).sum::<Duration>(),
			10 * PACKET_DURATION
		);
	}

	#[tokio::test]
	async fn paired_quick_resumes_keep_audio_and_reordered_video_together() {
		quick_resumes(true, true, Duration::from_millis(200)).await;
	}

	#[tokio::test]
	async fn an_audio_resume_aligns_with_continuous_video() {
		quick_resumes(true, false, Duration::from_millis(200)).await;
	}

	#[tokio::test]
	async fn a_video_resume_aligns_with_continuous_audio() {
		quick_resumes(false, true, Duration::from_millis(200)).await;
	}

	#[tokio::test]
	async fn paired_resumes_follow_the_speaker_when_delays_differ() {
		quick_resumes(true, true, Duration::from_millis(80)).await;
	}

	/// Audio stalls by congestion, then its backlog lands late and too far ahead
	/// to fill, so it starts a fresh sink. Video keeps its own schedule throughout:
	/// a stalled speaker never holds the picture.
	#[tokio::test]
	async fn stalled_audio_no_longer_holds_video() {
		struct Passthrough(Option<moq_video::Frame>);
		impl super::super::video::Decoder for Passthrough {
			async fn decode(&mut self, frame: moq_mux::container::Frame) -> anyhow::Result<()> {
				self.0 = Some(moq_video::Frame::new(
					moq_video::Surface::rgba(&[128; 16 * 16 * 4], moq_video::Size::new(16, 16))?,
					frame.timestamp,
				));
				Ok(())
			}
			async fn flush(&mut self) -> anyhow::Result<()> {
				Ok(())
			}
			fn receive(&mut self) -> anyhow::Result<Option<moq_video::Frame>> {
				Ok(self.0.take())
			}
		}
		tokio::time::pause();
		let start = Instant::now();
		let delay = Duration::from_millis(200);
		let recorder = Recorder::default();
		let origin = moq_tokio::origin::spawn();
		let media = media(&origin, delay, recorder.clone());
		let broadcast = moq_net::broadcast::Info::new().produce();
		let audio = broadcast
			.create_track("audio", hang::container::track_info(hang::catalog::PRIORITY.audio))
			.unwrap();
		let video = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let subscriber = video
			.consume()
			.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(delay),
						max: Some(delay),
					},
				)]),
			)
			.await
			.unwrap();
		let mut audio = moq_mux::container::Producer::new(audio, Container::Legacy(moq_mux::container::Kind::Audio));
		let mut video = moq_mux::container::Producer::new(video, Container::Legacy(moq_mux::container::Kind::Video));
		let mut options = moq_audio::decode::Options::new();
		options.delay = moq_audio::decode::Delay {
			min: Some(delay),
			max: Some(delay),
		};
		options.estimate = false;
		options.output.format = moq_audio::Format::F32;
		let consumer = moq_audio::decode::Consumer::new(
			&broadcast.consume(),
			&AudioConfig::new(AudioCodec::Pcm, SAMPLE_RATE, 1),
			"audio",
			options,
		)
		.await
		.unwrap();
		let audio_play = AudioPlayback {
			delay: tokio::sync::watch::channel(None).1,
			changed: media.drained.clone(),
			speaker: recorder.clone(),
			presentation: media.presentation.clone(),
			playhead: Default::default(),
			latency: delay,
			output: recorder.clone(),
			stretch: Duration::ZERO,
		};
		let video_play = Video {
			presentation: media.presentation.clone(),
			frames: media.video.clone(),
			changed: media.drained.clone(),
			output: recorder.clone(),
			max_delay: delay,
			min_delay: delay,
			delay: tokio::sync::watch::channel(None).1,
		};
		let audio_task = tokio::spawn(async move {
			play_audio(consumer, audio_play, std::future::pending()).await.unwrap();
		});
		let video_task = tokio::spawn(video_play.run(
			moq_mux::container::Consumer::new(subscriber, Container::Legacy(moq_mux::container::Kind::Video)),
			Passthrough(None),
		));
		let mut shown = Vec::new();
		for at in 0..1_400u64 {
			if at % 20 == 0 && at < 1_200 {
				let index = at / 20;
				video
					.write(moq_mux::container::Frame {
						timestamp: moq_net::Timestamp::from_millis(at).unwrap(),
						duration: Some(moq_net::Timestamp::from_millis(20).unwrap()),
						keyframe: index == 0,
						payload: Bytes::from_static(b"picture"),
					})
					.unwrap();
				// Audio stops after 200 ms; its backlog lands in one late burst at 800 ms.
				if !(10..=40).contains(&index) {
					audio.write(packet(index, if index < 10 { 0.25 } else { 0.5 })).unwrap();
				} else if index == 40 {
					for late in 10..=40 {
						audio.write(packet(late, 0.5)).unwrap();
					}
				}
			}
			tokio::task::yield_now().await;
			if let Some(timestamp) = recorder.present(&media) {
				shown.push((timestamp, Instant::now()));
			}
			tokio::time::advance(Duration::from_millis(1)).await;
		}
		let _finished = audio.finish().unwrap();
		let _finished = video.finish().unwrap();
		video_task.await.unwrap().unwrap();
		audio_task.abort();
		assert!(
			recorder.played().iter().any(|part| part.sample == 0.5),
			"audio never resumed"
		);
		// The burst lands within the reach, so it continues the sink rather than
		// aging out into a hole; the real ring skips it onto the clock.
		assert_eq!(recorder.latencies().len(), 1, "the late burst replaced the sink");
		assert!(shown.len() > 50, "video stalled: {} pictures", shown.len());
		for (timestamp, at) in shown {
			let due = start
				.saturating_add(delay)
				.saturating_add(Duration::try_from(timestamp).unwrap());
			assert!(
				at.saturating_duration_since(due) <= Duration::from_millis(1)
					&& due.saturating_duration_since(at) <= Duration::from_millis(1),
				"picture {timestamp:?} shown at {at:?}, due {due:?}"
			);
		}
	}

	async fn quick_resumes(audio_end: bool, video_end: bool, audio_latency: Duration) {
		struct Buffered(Vec<moq_video::Frame>, std::collections::VecDeque<moq_video::Frame>);
		impl super::super::video::Decoder for Buffered {
			async fn decode(&mut self, frame: moq_mux::container::Frame) -> anyhow::Result<()> {
				self.0.push(moq_video::Frame::new(
					moq_video::Surface::rgba(&[128; 16 * 16 * 4], moq_video::Size::new(16, 16))?,
					frame.timestamp,
				));
				self.0.sort_by(|a, b| a.timestamp.partial_cmp(&b.timestamp).unwrap());
				let ready = self.0.len().saturating_sub(2);
				self.1.extend(self.0.drain(..ready));
				Ok(())
			}
			async fn flush(&mut self) -> anyhow::Result<()> {
				self.1.extend(self.0.drain(..));
				Ok(())
			}
			fn receive(&mut self) -> anyhow::Result<Option<moq_video::Frame>> {
				Ok(self.1.pop_front())
			}
		}
		tokio::time::pause();
		let playback_start = Instant::now();
		let delay = Duration::from_millis(200);
		let recorder = Recorder::default();
		let origin = moq_tokio::origin::spawn();
		let media = media(&origin, delay, recorder.clone());
		let broadcast = moq_net::broadcast::Info::new().produce();
		let audio = broadcast
			.create_track("audio", hang::container::track_info(hang::catalog::PRIORITY.audio))
			.unwrap();
		let video = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let subscriber = video
			.consume()
			.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(delay),
						max: Some(delay),
					},
				)]),
			)
			.await
			.unwrap();
		let mut audio = moq_mux::container::Producer::new(audio, Container::Legacy(moq_mux::container::Kind::Audio));
		let mut video = moq_mux::container::Producer::new(video, Container::Legacy(moq_mux::container::Kind::Video));
		let mut options = moq_audio::decode::Options::new();
		options.delay = moq_audio::decode::Delay {
			min: Some(delay),
			max: Some(delay),
		};
		options.estimate = false;
		options.output.format = moq_audio::Format::F32;
		let consumer = moq_audio::decode::Consumer::new(
			&broadcast.consume(),
			&AudioConfig::new(AudioCodec::Pcm, SAMPLE_RATE, 1),
			"audio",
			options,
		)
		.await
		.unwrap();
		let audio_play = AudioPlayback {
			delay: tokio::sync::watch::channel(None).1,
			changed: media.drained.clone(),
			speaker: recorder.clone(),
			presentation: media.presentation.clone(),
			playhead: Default::default(),
			latency: audio_latency,
			output: recorder.clone(),
			stretch: Duration::ZERO,
		};
		let video_play = Video {
			presentation: media.presentation.clone(),
			frames: media.video.clone(),
			changed: media.drained.clone(),
			output: recorder.clone(),
			max_delay: delay,
			min_delay: delay,
			delay: tokio::sync::watch::channel(None).1,
		};
		let audio_task = tokio::spawn(async move {
			play_audio(consumer, audio_play, std::future::pending())
				.await
				.unwrap()
				.finish()
				.await;
		});
		let video_task = tokio::spawn(video_play.run(
			moq_mux::container::Consumer::new(subscriber, Container::Legacy(moq_mux::container::Kind::Video)),
			Buffered(Vec::new(), Default::default()),
		));
		let picture = |index: u64, keyframe: bool| moq_mux::container::Frame {
			timestamp: moq_net::Timestamp::from_millis(index * 20).unwrap(),
			duration: Some(moq_net::Timestamp::from_millis(20).unwrap()),
			keyframe,
			payload: Bytes::from_static(b"picture"),
		};
		for index in 0..3 {
			let mut pcm = packet(index, 0.25);
			pcm.keyframe = index == 0;
			audio.write(pcm).unwrap();
			video.write(picture(index, index == 0)).unwrap();
			kio::clock::sleep(PACKET_DURATION).await;
		}
		if audio_end {
			audio.discontinuity().unwrap();
		}
		if video_end {
			video.discontinuity().unwrap();
		}
		for index in 0..3 {
			let mut pcm = packet(index + 3, 0.5);
			pcm.keyframe = index == 0;
			audio.write(pcm).unwrap();
			video.write(picture(index + 3, video_end && index == 0)).unwrap();
			// Timestamps run on through the end, so the replacement arrives live.
			kio::clock::sleep(PACKET_DURATION).await;
		}
		let (mut audio, mut video) = (Some(audio), Some(video));
		let mut _audio_finished = audio.take_if(|_| audio_end).map(|audio| audio.finish().unwrap());
		let mut _video_finished = video.take_if(|_| video_end).map(|video| video.finish().unwrap());
		let mut shown = Vec::new();
		for step in 0..800 {
			if step % 20 == 0 {
				let index = 6 + step / 20;
				if let Some(audio) = &mut audio {
					audio.write(packet(index, 0.5)).unwrap();
				}
				if let Some(video) = &mut video {
					video.write(picture(index, false)).unwrap();
				}
			}
			tokio::task::yield_now().await;
			if let Some(timestamp) = recorder.present(&media) {
				shown.push((timestamp, Instant::now()));
			}
			tokio::time::advance(Duration::from_millis(1)).await;
		}
		_audio_finished = _audio_finished.or_else(|| audio.map(|audio| audio.finish().unwrap()));
		_video_finished = _video_finished.or_else(|| video.map(|video| video.finish().unwrap()));
		audio_task.await.unwrap();
		video_task.await.unwrap().unwrap();
		if video_end {
			assert_eq!(shown.len(), 6, "both decoder tails must be shown");
		}
		assert_eq!(
			shown[..3]
				.iter()
				.map(|(timestamp, _)| timestamp.as_millis().unwrap())
				.collect::<Vec<_>>(),
			[0, 20, 40]
		);
		let played = recorder.played();
		let old: Vec<_> = played.iter().filter(|part| part.sample == 0.25).collect();
		let new: Vec<_> = played.iter().filter(|part| part.sample == 0.5).collect();
		assert_eq!(
			old.iter().map(|part| part.to - part.from).sum::<Duration>(),
			PACKET_DURATION * 3
		);
		if audio_end {
			assert_eq!(
				new.iter().map(|part| part.to - part.from).sum::<Duration>(),
				PACKET_DURATION * 3
			);
		}
		assert!(old.last().unwrap().to <= new[0].from);
		let (picture, sample) = if audio_end && video_end {
			(shown[3].1, new[0].from)
		} else if audio_end {
			let timestamp = new[0].from.duration_since(playback_start.saturating_add(delay));
			let shown = shown
				.iter()
				.find(|(shown, _)| {
					let start = Duration::try_from(*shown).unwrap();
					start <= timestamp && timestamp < start + PACKET_DURATION
				})
				.expect("continuous video reaches resumed audio");
			(
				shown.1.saturating_add(timestamp - Duration::try_from(shown.0).unwrap()),
				new[0].from,
			)
		} else {
			let timestamp = Duration::try_from(shown[3].0).unwrap();
			let index = (shown[3].0.as_millis().unwrap() / 20) as usize;
			let offset = timestamp - PACKET_DURATION * index as u32;
			(shown[3].1, new[index - 3].from.saturating_add(offset))
		};
		assert!(
			picture.saturating_duration_since(sample) <= Duration::from_millis(1)
				&& sample.saturating_duration_since(picture) <= Duration::from_millis(1),
			"new A/V clocks diverged: picture={picture:?} sample={sample:?}"
		);
	}

	#[tokio::test]
	async fn an_end_before_its_first_pcm_still_resumes_that_timeline() {
		tokio::time::pause();
		let delay = Duration::from_millis(200);
		let broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track("audio", hang::container::track_info(hang::catalog::PRIORITY.audio))
			.unwrap();
		let mut producer = moq_mux::container::Producer::new(track, Container::Legacy(moq_mux::container::Kind::Audio));
		let mut options = moq_audio::decode::Options::new();
		options.delay = moq_audio::decode::Delay {
			min: Some(delay),
			max: Some(delay),
		};
		options.estimate = false;
		options.output.format = moq_audio::Format::F32;
		options.output.sample_rate = Some(SAMPLE_RATE);
		let consumer = moq_audio::decode::Consumer::new(
			&broadcast.consume(),
			&AudioConfig::new(AudioCodec::Pcm, 44_100, 1),
			"audio",
			options,
		)
		.await
		.unwrap();
		let recorder = Recorder::default();
		let playback = AudioPlayback {
			delay: tokio::sync::watch::channel(None).1,
			changed: Default::default(),
			speaker: recorder.clone(),
			presentation: Presentation::new(delay),
			playhead: Default::default(),
			latency: delay,
			output: recorder.clone(),
			stretch: Duration::ZERO,
		};
		let task = tokio::spawn(async move {
			play_audio(consumer, playback, std::future::pending())
				.await
				.unwrap()
				.finish()
				.await;
		});
		let pcm = |micros: u64, samples: usize, sample: f32| moq_mux::container::Frame {
			timestamp: moq_net::Timestamp::from_micros(micros).unwrap(),
			duration: Some(moq_net::Timestamp::from_scale(samples as u64, 44_100).unwrap()),
			keyframe: true,
			payload: std::iter::repeat_n(sample.to_le_bytes(), samples)
				.flatten()
				.collect::<Vec<_>>()
				.into(),
		};
		for index in 0..4 {
			producer.write(pcm(index * 23_220, 1024, 0.25)).unwrap();
		}
		producer.discontinuity().unwrap();
		// Too short for the resampler to emit anything before this encoder ends too.
		producer.write(pcm(100_000, 64, 0.5)).unwrap();
		producer.discontinuity().unwrap();
		for index in 0..4 {
			producer.write(pcm(200_000 + index * 23_220, 1024, 0.75)).unwrap();
		}
		let _finished = producer.finish().unwrap();
		task.await.unwrap();
		let played = recorder.played();
		let sinks = |range: std::ops::Range<f32>| {
			played
				.iter()
				.filter(|part| range.contains(&part.sample))
				.map(|part| (part.sink, part.utterance))
				.collect::<std::collections::BTreeSet<_>>()
		};
		let (old, short, new) = (sinks(0.2..0.3), sinks(0.4..0.6), sinks(0.7..0.8));
		assert!(!short.is_empty(), "the short timeline played: {played:?}");
		assert!(old.is_disjoint(&short), "the short timeline did not resume: {played:?}");
		assert!(short.is_disjoint(&new), "the next timeline did not resume: {played:?}");
	}

	#[tokio::test]
	async fn joining_a_paused_track_resumes_on_its_first_frame() {
		tokio::time::pause();
		let delay = Duration::from_millis(200);
		let broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track("audio", hang::container::track_info(hang::catalog::PRIORITY.audio))
			.unwrap();
		let mut producer = moq_mux::container::Producer::new(track, Container::Legacy(moq_mux::container::Kind::Audio));
		for index in 0..3 {
			producer.write(packet(index, 0.25)).unwrap();
		}
		producer.discontinuity().unwrap();
		// Joins at the end marker: the stopped encoder's media is already behind it.
		let mut options = moq_audio::decode::Options::new();
		options.delay = moq_audio::decode::Delay {
			min: Some(delay),
			max: Some(delay),
		};
		options.estimate = false;
		options.start = moq_audio::decode::Start::Latest;
		options.output.format = moq_audio::Format::F32;
		let consumer = moq_audio::decode::Consumer::new(
			&broadcast.consume(),
			&AudioConfig::new(AudioCodec::Pcm, SAMPLE_RATE, 1),
			"audio",
			options,
		)
		.await
		.unwrap();
		let recorder = Recorder::default();
		let playback = AudioPlayback {
			delay: tokio::sync::watch::channel(None).1,
			changed: Default::default(),
			speaker: recorder.clone(),
			presentation: Presentation::new(delay),
			playhead: Default::default(),
			latency: delay,
			output: recorder.clone(),
			stretch: Duration::ZERO,
		};
		let task = tokio::spawn(async move {
			play_audio(consumer, playback, std::future::pending())
				.await
				.unwrap()
				.finish()
				.await;
		});
		tokio::task::yield_now().await;
		for index in 3..6 {
			producer.write(packet(index, 0.5)).unwrap();
		}
		let _finished = producer.finish().unwrap();
		task.await.unwrap();
		let played = recorder.played();
		let resumed: Vec<_> = played.iter().filter(|part| part.sample == 0.5).collect();
		assert!(!resumed.is_empty(), "{played:?}");
		assert!(
			resumed.iter().all(|part| part.utterance > 0),
			"the next utterance did not start after the declared end: {played:?}"
		);
	}

	#[tokio::test]
	async fn an_evicted_replacement_gop_gives_way_to_the_next_replacement() {
		struct Passthrough(Option<moq_video::Frame>);
		impl super::super::video::Decoder for Passthrough {
			async fn decode(&mut self, frame: moq_mux::container::Frame) -> anyhow::Result<()> {
				self.0 = Some(moq_video::Frame::new(
					moq_video::Surface::rgba(&[128; 16 * 16 * 4], moq_video::Size::new(16, 16))?,
					frame.timestamp,
				));
				Ok(())
			}
			async fn flush(&mut self) -> anyhow::Result<()> {
				Ok(())
			}
			fn receive(&mut self) -> anyhow::Result<Option<moq_video::Frame>> {
				Ok(self.0.take())
			}
		}
		tokio::time::pause();
		let delay = Duration::from_millis(200);
		let recorder = Recorder::default();
		let origin = moq_tokio::origin::spawn();
		let media = media(&origin, delay, recorder.clone());
		let broadcast = moq_net::broadcast::Info::new().produce();
		let video = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		// The network keeps every group; only the player's age budget evicts.
		let subscriber = video
			.consume()
			.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_secs(10)),
						max: Some(Duration::from_secs(10)),
					},
				)]),
			)
			.await
			.unwrap();
		let video = moq_mux::container::Producer::new(video, Container::Legacy(moq_mux::container::Kind::Video));
		let video_play = Video {
			presentation: media.presentation.clone(),
			frames: media.video.clone(),
			changed: media.drained.clone(),
			output: recorder.clone(),
			max_delay: delay,
			min_delay: delay,
			delay: tokio::sync::watch::channel(None).1,
		};
		let video_task = tokio::spawn(video_play.run(
			moq_mux::container::Consumer::new(subscriber, Container::Legacy(moq_mux::container::Kind::Video)),
			Passthrough(None),
		));
		let picture = |millis: u64, keyframe: bool| moq_mux::container::Frame {
			timestamp: moq_net::Timestamp::from_millis(millis).unwrap(),
			duration: Some(moq_net::Timestamp::from_millis(20).unwrap()),
			keyframe,
			payload: Bytes::from_static(b"picture"),
		};
		let mut video = Some(video);
		let mut _finished = None;
		let mut shown = Vec::new();
		for at in 0..6_000 {
			let producer = video.as_mut();
			match at {
				0 | 20 | 40 => producer.unwrap().write(picture(at, at == 0)).unwrap(),
				// A burst longer than the age budget evicts this replacement's only keyframe.
				1_000 => {
					let producer = producer.unwrap();
					producer.discontinuity().unwrap();
					for index in 0..15 {
						producer.write(picture(at + index * 20, index == 0)).unwrap();
					}
				}
				5_000 => {
					let producer = producer.unwrap();
					producer.discontinuity().unwrap();
					producer.write(picture(at, true)).unwrap();
				}
				5_020 | 5_040 => producer.unwrap().write(picture(at, false)).unwrap(),
				5_060 => _finished = video.take().map(|video| video.finish().unwrap()),
				_ => {}
			}
			tokio::task::yield_now().await;
			if let Some(timestamp) = recorder.present(&media) {
				shown.push(timestamp.as_millis().unwrap());
			}
			tokio::time::advance(Duration::from_millis(1)).await;
		}
		video_task.await.unwrap().unwrap();
		assert_eq!(shown[..3], [0, 20, 40], "{shown:?}");
		assert_eq!(shown.len(), 6, "the next replacement plays: {shown:?}");
	}

	/// A mono PCM rendition, published and named in the catalog until dropped.
	fn rendition(
		broadcast: &moq_net::broadcast::Producer,
		catalog: &catalog::Producer,
		name: &str,
	) -> moq_mux::container::Producer<Container, AudioConfig> {
		let track = broadcast
			.create_track(name, hang::container::track_info(hang::catalog::PRIORITY.audio))
			.unwrap();
		catalog
			.audio(
				track,
				Container::Legacy(moq_mux::container::Kind::Audio),
				AudioConfig::new(AudioCodec::Pcm, SAMPLE_RATE, 1),
			)
			.unwrap()
	}

	/// The `index`th packet of the broadcast, every sample set to `sample` so the
	/// recorder can tell which rendition played it.
	fn packet(index: u64, sample: f32) -> moq_mux::container::Frame {
		let payload: Vec<u8> = std::iter::repeat_n(sample.to_le_bytes(), PACKET as usize)
			.flatten()
			.collect();
		moq_mux::container::Frame {
			timestamp: moq_net::Timestamp::from_scale(index * PACKET, SAMPLE_RATE as u64).unwrap(),
			duration: None,
			payload: Bytes::from(payload),
			keyframe: true,
		}
	}

	fn media(origin: &moq_net::origin::Producer, delay: Duration, output: Recorder) -> Media<Recorder> {
		Media {
			origin: origin.consume(),
			broadcast: "room".to_string(),
			args: Args {
				catalog_format: None,
				metrics: None,
				vaapi_device: None,
				v4l2_decoder: None,
				delay: Delay::Fixed(delay),
				stretch: Duration::ZERO.into(),
				select: Default::default(),
			},
			video: Default::default(),
			presentation: Presentation::new(delay),
			playhead: Default::default(),
			drained: Default::default(),
			output,
			viewport: tokio::sync::watch::channel(None).1,
		}
	}

	#[tokio::test]
	async fn disabling_a_live_high_rendition_plays_the_lower_one() {
		tokio::time::pause();
		let origin = moq_tokio::origin::spawn();
		let broadcast = origin.route("room", moq_net::origin::RouteOptions::default()).unwrap();
		let mut catalog = catalog::Producer::new(&mut broadcast.broadcast().clone(), Default::default()).unwrap();
		let mut high = rendition(broadcast.broadcast(), &catalog, "high");
		let recorder = Recorder::default();
		let player = tokio::spawn(media(&origin, Duration::from_millis(50), recorder.clone()).run());
		write(&mut high, 10, 0.25).await;
		let mut low = rendition(broadcast.broadcast(), &catalog, "low");
		catalog
			.mutate(
				|catalog| {
					let high = catalog.audio.renditions.get_mut("high").unwrap();
					high.bitrate = Some(u64::from(SAMPLE_RATE) * 32);
					high.enabled = false;
					// An unknown bitrate ranks below a known one without falsifying PCM metadata.
					catalog.audio.renditions.get_mut("low").unwrap().bitrate = None;
				},
				catalog.next_timestamp(),
			)
			.unwrap();
		for index in 10..40 {
			high.write(packet(index, 0.25)).unwrap();
			low.write(packet(index, 0.5)).unwrap();
			kio::clock::sleep(PACKET_DURATION).await;
		}
		let _finished = low.finish().unwrap();
		catalog.finish().unwrap();
		// The high rendition remains open: only the disabled catalog can stop it.
		settle(player, &recorder).await;
		assert!(recorder.played().iter().any(|part| part.sample == 0.25));
		assert!(
			recorder.played().iter().any(|part| part.sample == 0.5),
			"the live disabled track prevented fallback"
		);
	}

	/// A video rendition on a broadcast nothing announces opens beside the rest of playback,
	/// rather than holding everything up while it waits.
	#[tokio::test]
	async fn an_unannounced_video_rendition_holds_nothing_up() {
		tokio::time::pause();
		let origin = moq_tokio::origin::spawn();
		let broadcast = origin.route("room", moq_net::origin::RouteOptions::default()).unwrap();
		let mut catalog = catalog::Producer::new(&mut broadcast.broadcast().clone(), Default::default()).unwrap();
		catalog
			.mutate(
				|catalog| {
					let mut config = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8);
					config.broadcast = Some(moq_net::path::Relative::new("./portrait").unwrap());
					catalog.video.renditions.insert("tall".to_string(), config);
				},
				catalog.next_timestamp(),
			)
			.unwrap();
		let mut audio = rendition(broadcast.broadcast(), &catalog, "audio");
		let recorder = Recorder::default();
		let player = tokio::spawn(media(&origin, Duration::from_millis(50), recorder.clone()).run());
		write(&mut audio, 40, 0.25).await;
		settle(player, &recorder).await;
		assert!(
			recorder.played().iter().any(|part| part.sample == 0.25),
			"the unannounced video rendition held audio up"
		);
	}

	/// A better-ranked rendition on a sibling broadcast nothing announces yet holds back no
	/// fallback the catalog's own broadcast can play now, and wins once its broadcast lands.
	#[tokio::test]
	async fn a_pending_sibling_rendition_plays_the_fallback_until_it_lands() {
		tokio::time::pause();
		const LOW: f32 = 0.25;
		const HIGH: f32 = 0.5;
		let origin = moq_tokio::origin::spawn();
		let broadcast = origin.route("room", moq_net::origin::RouteOptions::default()).unwrap();
		let mut catalog = catalog::Producer::new(&mut broadcast.broadcast().clone(), Default::default()).unwrap();
		let mut low = rendition(broadcast.broadcast(), &catalog, "low");
		// Published at the sibling only later, but named in the catalog from the start.
		let sibling = origin.route("other", moq_net::origin::RouteOptions::hidden()).unwrap();
		let sibling_catalog = catalog::Producer::new(&mut sibling.broadcast().clone(), Default::default()).unwrap();
		let mut high = rendition(sibling.broadcast(), &sibling_catalog, "high");
		let mut config = sibling_catalog.snapshot().audio.renditions["high"].clone();
		config.broadcast = Some(moq_net::path::Relative::new("./other").unwrap());
		config.bitrate = Some(u64::from(SAMPLE_RATE) * 32);
		catalog
			.mutate(
				|catalog| {
					catalog.audio.renditions.insert("high".to_string(), config);
				},
				catalog.next_timestamp(),
			)
			.unwrap();

		let recorder = Recorder::default();
		let player = tokio::spawn(media(&origin, Duration::from_millis(500), recorder.clone()).run());
		for index in 0..40 {
			low.write(packet(index, LOW)).unwrap();
			kio::clock::sleep(PACKET_DURATION).await;
		}
		assert!(
			played(&recorder, LOW).0 > Duration::ZERO,
			"the pending sibling held the fallback back"
		);

		// The preferred rendition sends nothing for longer than the delay the fallback holds, so
		// retiring the fallback as its broadcast lands would go silent.
		sibling.announce().unwrap();
		for index in 40..120 {
			low.write(packet(index, LOW)).unwrap();
			if index >= 80 {
				high.write(packet(index, HIGH)).unwrap();
			}
			kio::clock::sleep(PACKET_DURATION).await;
		}
		settle(player, &recorder).await;
		assert!(
			played(&recorder, HIGH).0 > Duration::ZERO,
			"the preferred rendition never took over once its broadcast landed"
		);
		assert_eq!(
			recorder.played().last().map(|part| part.sample),
			Some(HIGH),
			"the fallback outlived the switch"
		);
		// The preferred rendition opens while the fallback plays on, which retires only once
		// the preferred one answers, so every fallback packet sent before then is heard.
		assert_eq!(
			played(&recorder, LOW).0,
			PACKET_DURATION * 81,
			"the fallback retired before its replacement was ready"
		);
	}

	/// An audio rendition named `name` on `broadcast`, a sibling of the catalog's when given.
	fn candidate(name: &str, broadcast: Option<&str>) -> AudioCandidate {
		let mut config = AudioConfig::new(AudioCodec::Pcm, SAMPLE_RATE, 1);
		config.broadcast = broadcast.map(|broadcast| moq_net::path::Relative::new(broadcast).unwrap());
		(name.to_string(), config, Track::named(name))
	}

	/// A rendition on a pending broadcast holds back none ranked below it that resolves, and
	/// reports itself outranking what opened. If it lands while a lower one is still opening, it
	/// opens beside that attempt and wins once it opens first.
	#[tokio::test(start_paused = true)]
	async fn a_pending_broadcast_holds_back_no_lower_rendition() {
		let origin = moq_tokio::origin::spawn();
		let room = origin.route("room", moq_net::origin::RouteOptions::default()).unwrap();
		let source = moq_mux::Source::new(origin.consume(), room.broadcast().consume());
		let candidates = || vec![candidate("high", Some("./other")), candidate("low", None)];

		let opened = open_first(&source, "audio", candidates(), |_, name, _, _| async move { Ok(name) }).await;
		assert_eq!(opened.ready.as_deref(), Some("low"));
		let outranked = opened.outranked.expect("the pending rendition outranks the fallback");

		// The fallback never answers, so only the better rendition landing ends the attempt.
		let other = origin.route("other", moq_net::origin::RouteOptions::hidden()).unwrap();
		let opening = tokio::spawn({
			let source = source.clone();
			async move {
				open_first(&source, "audio", candidates(), |_, name, _, _| async move {
					if name == "low" {
						std::future::pending::<()>().await;
					}
					Ok(name)
				})
				.await
			}
		});
		kio::clock::sleep(Duration::from_secs(1)).await;
		assert!(!opening.is_finished(), "nothing landed yet");
		other.announce().unwrap();
		outranked.landed().await;
		let opened = kio::clock::timeout(Duration::from_secs(1), opening)
			.await
			.unwrap()
			.unwrap();
		assert_eq!(opened.ready.as_deref(), Some("high"));
		assert!(opened.outranked.is_none(), "nothing ranks above the best");
	}

	/// A better broadcast that lands before its track answers leaves the lower attempt opening,
	/// so the lower one plays once it opens, and the better one outranks it at once.
	#[tokio::test(start_paused = true)]
	async fn a_landed_broadcast_still_opening_holds_back_no_lower_rendition() {
		let origin = moq_tokio::origin::spawn();
		let room = origin.route("room", moq_net::origin::RouteOptions::default()).unwrap();
		let source = moq_mux::Source::new(origin.consume(), room.broadcast().consume());
		let candidates = vec![candidate("high", Some("./other")), candidate("low", None)];

		let release = Arc::new(tokio::sync::Notify::new());
		let opening = tokio::spawn({
			let source = source.clone();
			let release = release.clone();
			async move {
				open_first(&source, "audio", candidates, |_, name, _, _| {
					let release = release.clone();
					async move {
						match name.as_str() {
							"high" => std::future::pending::<()>().await,
							_ => release.notified().await,
						}
						Ok(name)
					}
				})
				.await
			}
		});
		kio::clock::sleep(Duration::from_secs(1)).await;
		let other = origin.route("other", moq_net::origin::RouteOptions::hidden()).unwrap();
		other.announce().unwrap();
		kio::clock::sleep(Duration::from_secs(1)).await;
		assert!(!opening.is_finished(), "neither attempt opened yet");

		release.notify_one();
		let opened = kio::clock::timeout(Duration::from_secs(1), opening)
			.await
			.unwrap()
			.unwrap();
		assert_eq!(opened.ready.as_deref(), Some("low"), "the lower attempt was cut short");
		let outranked = opened.outranked.expect("the better rendition outranks the fallback");
		kio::clock::timeout(Duration::from_secs(1), outranked.landed())
			.await
			.expect("the better broadcast already landed");
	}

	/// A publisher retires an audio rendition by naming its replacement and then
	/// finishing the old track. The retired sink still holds a delay of audio, and
	/// the replacement's sink holds its own before its first sample sounds, so
	/// played one after the other the switch costs a delay of silence (#3966).
	#[tokio::test]
	async fn an_audio_rendition_switch_leaves_no_gap() {
		tokio::time::pause();

		const OLD: f32 = 0.25;
		const NEW: f32 = 0.5;
		let delay = Duration::from_millis(500);

		let origin = moq_tokio::origin::spawn();
		let broadcast = origin.route("room", moq_net::origin::RouteOptions::default()).unwrap();
		let mut catalog = catalog::Producer::new(&mut broadcast.broadcast().clone(), Default::default()).unwrap();

		let recorder = Recorder::default();
		let player = tokio::spawn(media(&origin, delay, recorder.clone()).run());

		// A second of the old rendition, published in real time.
		// Paced against absolute deadlines: tokio rounds each sleep up to the next
		// millisecond, which relative sleeps would accumulate into a publisher
		// falling behind the speaker.
		let mut old = rendition(broadcast.broadcast(), &catalog, "old");
		let start = Instant::now();
		let mut index = 0;
		while index < 50 {
			old.write(packet(index, OLD)).unwrap();
			index += 1;
			kio::clock::sleep_until(start.saturating_add(PACKET_DURATION * index as u32)).await;
		}

		// The replacement joins the catalog, then the old track finishes.
		let mut new = rendition(broadcast.broadcast(), &catalog, "new");
		new.write(packet(index, NEW)).unwrap();
		index += 1;
		drop(old.finish().unwrap());

		while index < 100 {
			kio::clock::sleep_until(start.saturating_add(PACKET_DURATION * index as u32)).await;
			new.write(packet(index, NEW)).unwrap();
			index += 1;
		}
		drop(new.finish().unwrap());
		catalog.finish().unwrap();

		settle(player, &recorder).await;

		let played = recorder.played();
		let old_end = played.iter().filter(|p| p.sample == OLD).map(|p| p.to).max().unwrap();
		let new_start = played.iter().filter(|p| p.sample == NEW).map(|p| p.from).min().unwrap();
		// Tokio rounds the replacement's pacing sleep to the next millisecond.
		// The exact sample count below rules out any truncation within that tick.
		let gap = new_start.saturating_duration_since(old_end);
		assert!(gap <= Duration::from_millis(1), "the switch went silent for {gap:?}");
		let old_duration: Duration = played.iter().filter(|p| p.sample == OLD).map(|p| p.to - p.from).sum();
		assert_eq!(old_duration, Duration::from_secs(1));
	}

	/// A catalog moves the rendition to another track while the old one still
	/// sends, then back. Each move plays out the retired sink's tail while the new
	/// track fills, as a finished track's does, and nothing the track it left sent
	/// after the move plays. A track left this way did not finish, so moving back
	/// plays it again.
	#[tokio::test]
	async fn a_moved_audio_rendition_plays_its_new_track() {
		tokio::time::pause();

		const OLD: f32 = 0.25;
		const LEFT: f32 = 0.125;
		const NEW: f32 = 0.5;
		const BACK: f32 = 0.75;
		let delay = Duration::from_millis(500);

		let origin = moq_tokio::origin::spawn();
		let broadcast = origin.route("room", moq_net::origin::RouteOptions::default()).unwrap();
		let mut catalog = catalog::Producer::new(&mut broadcast.broadcast().clone(), Default::default()).unwrap();
		let track = |name: &str| {
			let track = broadcast
				.broadcast()
				.create_track(name, hang::container::track_info(hang::catalog::PRIORITY.audio))
				.unwrap();
			moq_mux::container::Producer::new(track, Container::Legacy(moq_mux::container::Kind::Audio))
		};
		let (mut a, mut b) = (track("a"), track("b"));
		let point = |catalog: &mut catalog::Producer, track: &str| {
			let mut config = AudioConfig::new(AudioCodec::Pcm, SAMPLE_RATE, 1);
			config.container = hang::catalog::Container::Legacy;
			config.track = Some(track.to_string());
			catalog
				.mutate(
					|catalog| {
						catalog.audio.renditions.insert("main".to_string(), config);
					},
					catalog.next_timestamp(),
				)
				.unwrap();
		};
		point(&mut catalog, "a");

		let recorder = Recorder::default();
		let player = tokio::spawn(media(&origin, delay, recorder.clone()).run());

		// Paced against absolute deadlines, as in the switch test above. Each move
		// lands with only the new track's packet out, so the player takes it before
		// the track it left sends again.
		let start = Instant::now();
		let mut index = 0;
		while index < 150 {
			match index {
				0..50 => a.write(packet(index, OLD)).unwrap(),
				50 => {
					b.write(packet(index, NEW)).unwrap();
					point(&mut catalog, "b");
				}
				51..100 => {
					a.write(packet(index, LEFT)).unwrap();
					b.write(packet(index, NEW)).unwrap();
				}
				100 => {
					a.write(packet(index, BACK)).unwrap();
					point(&mut catalog, "a");
				}
				_ => a.write(packet(index, BACK)).unwrap(),
			}
			index += 1;
			kio::clock::sleep_until(start.saturating_add(PACKET_DURATION * index as u32)).await;
		}

		settle(player, &recorder).await;

		let played = recorder.played();
		assert!(
			!played.iter().any(|p| p.sample == LEFT),
			"the track it left kept playing"
		);
		let span = |sample: f32| {
			let parts = played.iter().filter(|p| p.sample == sample);
			let from = parts.clone().map(|p| p.from).min().unwrap();
			let to = parts.clone().map(|p| p.to).max().unwrap();
			let duration: Duration = parts.map(|p| p.to - p.from).sum();
			(from, to, duration)
		};
		let (_, old_end, old) = span(OLD);
		let (new_start, new_end, new) = span(NEW);
		let (back_start, _, back) = span(BACK);
		assert_eq!(old, Duration::from_secs(1), "the retired tail was cut");
		assert_eq!(new, Duration::from_secs(1), "the retired tail was cut");
		assert_eq!(
			back,
			Duration::from_secs(1),
			"the track moved back to did not play in full"
		);
		// Tokio rounds the pacing sleep to the next millisecond.
		for (end, start) in [(old_end, new_start), (new_end, back_start)] {
			let gap = start.saturating_duration_since(end);
			assert!(gap <= Duration::from_millis(1), "the move went silent for {gap:?}");
		}
	}

	/// Publish an audio-only broadcast at `room` under a fresh identity, returning what keeps
	/// it up and its one rendition.
	fn publish(
		origin: &moq_net::origin::Producer,
	) -> (
		moq_net::origin::Route,
		catalog::Producer,
		moq_mux::container::Producer<Container, AudioConfig>,
	) {
		let route = origin.route("room", moq_net::origin::RouteOptions::hidden()).unwrap();
		let catalog = catalog::Producer::new(&mut route.broadcast().clone(), Default::default()).unwrap();
		let audio = rendition(route.broadcast(), &catalog, "audio");
		route.announce().unwrap();
		(route, catalog, audio)
	}

	/// Answer the next request through `claim` with one `audio` rendition, under a fresh identity.
	async fn answer(
		claim: &moq_net::origin::Claim,
	) -> (
		moq_net::origin::Route,
		catalog::Producer,
		moq_mux::container::Producer<Container, AudioConfig>,
	) {
		let request = kio::clock::timeout(Duration::from_secs(5), claim.requested())
			.await
			.expect("the claim is asked")
			.unwrap();
		let route = request.route(moq_net::origin::RouteOptions::hidden()).unwrap();
		let mut broadcast = route.broadcast().clone();
		let catalog = catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
		let audio = rendition(&broadcast, &catalog, "audio");
		route.announce().unwrap();
		(route, catalog, audio)
	}

	/// Write `count` packets of `sample` in real time, its timeline starting at 0.
	async fn write(audio: &mut moq_mux::container::Producer<Container, AudioConfig>, count: u64, sample: f32) {
		for index in 0..count {
			audio.write(packet(index, sample)).unwrap();
			kio::clock::sleep(PACKET_DURATION).await;
		}
	}

	/// How long `sample` played for, and through how many sinks.
	fn played(recorder: &Recorder, sample: f32) -> (Duration, usize) {
		let played: Vec<_> = recorder.played().into_iter().filter(|p| p.sample == sample).collect();
		let mut sinks: Vec<_> = played.iter().map(|p| p.sink).collect();
		sinks.dedup();
		(played.iter().map(|p| p.to - p.from).sum(), sinks.len())
	}

	/// The player follows the path for as long as the origin lasts, so it never
	/// ends on its own: give it long enough to play out what it was handed, then
	/// stop it.
	async fn settle(player: tokio::task::JoinHandle<()>, recorder: &Recorder) {
		kio::clock::sleep(Duration::from_secs(5)).await;
		for event in recorder.events() {
			match event {
				Event::Failed(err) => panic!("playback failed: {err}"),
				Event::Ended | Event::Finished => panic!("the player stopped following the path"),
				Event::Wake | Event::Utterance(_) | Event::Trimmed { .. } => {}
			}
		}
		assert!(!player.is_finished(), "the player stopped following the path");
		player.abort();
	}

	/// A restarted publisher announces a fresh identity while its old run still
	/// stands. The restart drops the old broadcast and plays the new one, whose
	/// timeline starts over, instead of waiting for it to catch up.
	#[tokio::test]
	async fn a_republish_plays_the_new_broadcast() {
		tokio::time::pause();

		const OLD: f32 = 0.25;
		const NEW: f32 = 0.5;
		let origin = moq_tokio::origin::spawn();
		let (_old_broadcast, _old_catalog, mut old) = publish(&origin);
		let recorder = Recorder::default();
		let player = tokio::spawn(media(&origin, Duration::from_millis(50), recorder.clone()).run());
		write(&mut old, 10, OLD).await;

		let (_new_broadcast, _new_catalog, mut new) = publish(&origin);
		write(&mut new, 10, NEW).await;

		settle(player, &recorder).await;
		assert_eq!(recorder.speakers(), 1, "the replacement reopened the speaker");
		assert!(
			played(&recorder, OLD).0 > Duration::ZERO,
			"the old broadcast never played"
		);
		assert_eq!(
			played(&recorder, NEW).0,
			PACKET_DURATION * 10,
			"the new broadcast did not play in full"
		);
	}

	/// A selected handler's explicit refusal is terminal, rather than an offline gap that a
	/// later announcement might fix. Waiting forever hides a wrong path or denied request.
	#[tokio::test(start_paused = true)]
	async fn a_refused_broadcast_ends_the_player() {
		let origin = moq_tokio::origin::spawn();
		let handler = origin.claim("", moq_net::origin::RouteOptions::default()).unwrap();
		let recorder = Recorder::default();
		let player = tokio::spawn(media(&origin, Duration::from_millis(50), recorder.clone()).run());
		let request = handler.requested().await.unwrap();
		request.reject(moq_net::Error::NotFound);
		kio::clock::timeout(Duration::from_secs(1), player)
			.await
			.expect("a refusal held the player")
			.unwrap();
		assert!(recorder.events().iter().any(|event| matches!(event, Event::Failed(_))));
	}

	/// A restart that overlaps the old route still replaces it: it is a new identity, and
	/// the newest announcement at the exact path wins it.
	#[tokio::test]
	async fn an_overlapping_republish_plays_the_new_broadcast() {
		tokio::time::pause();

		const OLD: f32 = 0.25;
		const NEW: f32 = 0.5;
		let origin = moq_tokio::origin::spawn();

		let (_old_broadcast, _old_catalog, mut old) = publish(&origin);
		let recorder = Recorder::default();
		let player = tokio::spawn(media(&origin, Duration::from_millis(50), recorder.clone()).run());
		write(&mut old, 10, OLD).await;

		let (_new_broadcast, _new_catalog, mut new) = publish(&origin);
		write(&mut new, 10, NEW).await;

		settle(player, &recorder).await;
		assert_eq!(recorder.speakers(), 1, "the replacement reopened the speaker");
		assert!(
			played(&recorder, OLD).0 > Duration::ZERO,
			"the old broadcast never played"
		);
		assert_eq!(
			played(&recorder, NEW).0,
			PACKET_DURATION * 10,
			"the new broadcast did not play in full"
		);
	}

	/// A publisher that stops cleanly ends the path, and its restart starts it
	/// afresh. The player waits through the gap and plays the restarted broadcast.
	#[tokio::test]
	async fn a_broadcast_that_returns_plays_again() {
		tokio::time::pause();

		const OLD: f32 = 0.25;
		const NEW: f32 = 0.5;
		let origin = moq_tokio::origin::spawn();

		let (old_broadcast, mut old_catalog, mut old) = publish(&origin);
		let recorder = Recorder::default();
		let player = tokio::spawn(media(&origin, Duration::from_millis(50), recorder.clone()).run());
		write(&mut old, 10, OLD).await;
		let old = old.finish().unwrap();
		old_catalog.finish().unwrap();
		drop((old_broadcast, old_catalog, old));

		// Offline for longer than any timeout the player might give up on.
		kio::clock::sleep(Duration::from_secs(60)).await;
		assert!(!player.is_finished(), "the player stopped following the path");

		let (_new_broadcast, _new_catalog, mut new) = publish(&origin);
		write(&mut new, 10, NEW).await;

		settle(player, &recorder).await;
		assert_eq!(recorder.speakers(), 1, "the returning broadcast reopened the speaker");
		assert_eq!(
			played(&recorder, OLD).0,
			PACKET_DURATION * 10,
			"the old broadcast lost its tail"
		);
		assert_eq!(
			played(&recorder, NEW).0,
			PACKET_DURATION * 10,
			"the new broadcast did not play in full"
		);
	}

	/// A catalog update lands while a track plays, then the track ends. The
	/// player reads the newer snapshot, which still names the finished track, and
	/// must not resubscribe to it and replay its newest group.
	#[tokio::test]
	async fn a_finished_track_is_not_replayed_after_a_catalog_update() {
		tokio::time::pause();

		let origin = moq_tokio::origin::spawn();
		let (_broadcast, mut catalog, mut audio) = publish(&origin);
		let recorder = Recorder::default();
		let player = tokio::spawn(media(&origin, Duration::from_millis(50), recorder.clone()).run());
		write(&mut audio, 10, 0.25).await;
		catalog
			.mutate(
				|catalog| {
					catalog.audio.renditions.get_mut("audio").unwrap().bitrate = Some(u64::from(SAMPLE_RATE) * 32);
				},
				catalog.next_timestamp(),
			)
			.unwrap();
		let _finished = audio.finish().unwrap();

		// The catalog stays open, so the player keeps following it.
		settle(player, &recorder).await;
		assert_eq!(
			played(&recorder, 0.25).0,
			PACKET_DURATION * 10,
			"the finished track replayed"
		);
	}

	/// A catalog can disable a rendition after its track ended but before the
	/// player joins that end. The stop then lands on nothing, and the end still
	/// counts as finished, so re-enabling the rendition does not replay it.
	#[tokio::test]
	async fn a_stop_after_the_track_ended_still_reports_it_finished() {
		let (stop, stopped) = tokio::sync::oneshot::channel();
		let task = tokio::spawn(until_stopped(async { anyhow::Ok(()) }, stopped));
		while !task.is_finished() {
			tokio::task::yield_now().await;
		}
		assert!(stop.send(()).is_err(), "the stop raced a running task");
		let (finished, _) = task.await.unwrap().unwrap();
		assert!(finished, "a track that ended was reported as stopped");
	}

	/// Equivalent `broadcast` references name one track, so a snapshot that only
	/// respells the reference does not make a finished track look new.
	#[tokio::test]
	async fn equivalent_references_name_one_track() {
		let origin = moq_tokio::origin::spawn();
		let (broadcast, _catalog, _audio) = publish(&origin);
		let source = moq_mux::Source::new(origin.consume(), broadcast.broadcast().consume());
		let mut config = AudioConfig::new(AudioCodec::Pcm, SAMPLE_RATE, 1);
		let absent = Track::of(&source, &config, "audio");
		config.broadcast = Some(moq_net::path::Relative::new("").unwrap());
		assert_eq!(Track::of(&source, &config, "audio"), absent);
	}

	/// A covering route that arrives as the exact route goes, both before the player
	/// looks, is the path served again after a gap. A player with
	/// nothing playing plays it.
	#[tokio::test]
	async fn a_covering_route_after_a_gap_plays_while_idle() {
		tokio::time::pause();

		const OLD: f32 = 0.25;
		const NEW: f32 = 0.5;
		let origin = moq_tokio::origin::spawn();
		let (exact, mut exact_catalog, mut old) = publish(&origin);
		let recorder = Recorder::default();
		let player = tokio::spawn(media(&origin, Duration::from_millis(50), recorder.clone()).run());
		write(&mut old, 10, OLD).await;
		let old = old.finish().unwrap();
		exact_catalog.finish().unwrap();
		drop((exact_catalog, old));
		// The broadcast ends while its route stands, so the player goes idle.
		kio::clock::sleep(Duration::from_secs(60)).await;

		let pool = origin.claim("", moq_net::origin::RouteOptions::default()).unwrap();
		drop(exact);
		let (served, served_catalog, mut new) = answer(&pool).await;
		write(&mut new, 10, NEW).await;

		settle(player, &recorder).await;
		assert_eq!(
			played(&recorder, NEW).0,
			PACKET_DURATION * 10,
			"the idle player never played the covering route"
		);
		drop((served, served_catalog, pool));
	}

	/// The exact route goes while the old run still holds its delay, and a covering
	/// route takes over under another identity. Playback reaches the covering route as
	/// a restart.
	#[tokio::test]
	async fn a_handoff_to_a_covering_route_plays_on_while_draining() {
		tokio::time::pause();

		const OLD: f32 = 0.25;
		const NEW: f32 = 0.5;
		let origin = moq_tokio::origin::spawn();
		// One identity, served by the exact route and then answered again behind a covering one.
		let (exact, exact_catalog, mut old) = publish(&origin);
		let recorder = Recorder::default();
		let player = tokio::spawn(media(&origin, Duration::from_millis(500), recorder.clone()).run());
		for index in 0..50 {
			old.write(packet(index, OLD)).unwrap();
			kio::clock::sleep(PACKET_DURATION).await;
		}

		exact.withdraw();
		let pool = origin.claim("", moq_net::origin::RouteOptions::default()).unwrap();
		let (mirror, mirror_catalog, mut copy) = answer(&pool).await;
		for index in 50..60 {
			copy.write(packet(index, NEW)).unwrap();
			kio::clock::sleep(PACKET_DURATION).await;
		}

		settle(player, &recorder).await;
		assert!(
			played(&recorder, NEW).0 > Duration::ZERO,
			"the player never reached the covering route"
		);
		drop((mirror, mirror_catalog, copy, pool, exact, exact_catalog, old));
	}

	/// Re-pricing the same identity is an update, which playback rides through on
	/// one sink rather than starting over.
	#[tokio::test]
	async fn a_reprice_keeps_playing() {
		tokio::time::pause();

		const SAMPLE: f32 = 0.25;
		let origin = moq_tokio::origin::spawn();
		let (broadcast, _catalog, mut audio) = publish(&origin);
		let recorder = Recorder::default();
		let player = tokio::spawn(media(&origin, Duration::from_millis(50), recorder.clone()).run());
		for index in 0..20 {
			if index == 10 {
				broadcast.set_cost(5).unwrap();
			}
			audio.write(packet(index, SAMPLE)).unwrap();
			kio::clock::sleep(PACKET_DURATION).await;
		}

		settle(player, &recorder).await;
		assert_eq!(
			played(&recorder, SAMPLE),
			(PACKET_DURATION * 20, 1),
			"the update restarted playback"
		);
	}

	/// The latency the clock holds is the device's peak over the window: a lower
	/// reading holds until the higher one ages out, and a higher one takes over at once.
	#[test]
	fn the_output_latency_holds_its_peak_for_the_window() {
		let start = Instant::now();
		let at = |secs: u64| start.saturating_add(Duration::from_secs(secs));
		let ms = Duration::from_millis;
		let mut peak = Peak::default();
		assert_eq!(peak.observe(None, at(0)), None);
		assert_eq!(peak.observe(Some(ms(20)), at(0)), Some(ms(20)));
		assert_eq!(peak.observe(Some(ms(14)), at(1)), Some(ms(20)));
		assert_eq!(peak.observe(None, at(5)), Some(ms(20)));
		assert_eq!(peak.observe(Some(ms(15)), at(6)), Some(ms(15)));
		assert_eq!(peak.observe(Some(ms(40)), at(7)), Some(ms(40)));
		// Unreported since, the last peak stands.
		assert_eq!(peak.observe(None, at(60)), Some(ms(40)));
	}

	#[tokio::test]
	async fn a_finite_audio_track_plays_its_final_samples() {
		tokio::time::pause();
		let origin = moq_tokio::origin::spawn();
		let broadcast = origin.route("room", moq_net::origin::RouteOptions::default()).unwrap();
		let mut catalog = catalog::Producer::new(&mut broadcast.broadcast().clone(), Default::default()).unwrap();
		let recorder = Recorder::default();
		let player = tokio::spawn(media(&origin, Duration::from_millis(50), recorder.clone()).run());
		let mut audio = rendition(broadcast.broadcast(), &catalog, "audio");
		audio.write(packet(0, 0.25)).unwrap();
		kio::clock::sleep(PACKET_DURATION).await;
		drop(audio.finish().unwrap());
		catalog.finish().unwrap();
		settle(player, &recorder).await;
		let played: Duration = recorder.played().iter().map(|p| p.to - p.from).sum();
		assert_eq!(played, PACKET_DURATION, "the finished track lost its tail");
	}

	/// An encoder fed 1024-sample blocks emits each 960-sample packet once the
	/// block holding its end arrives: a little later each time, then two at once,
	/// so arrivals swing by a packet. That is cadence, and a fixed delay plays
	/// every sample through it.
	#[tokio::test]
	async fn arrivals_swinging_by_a_packet_play_every_sample() {
		tokio::time::pause();
		const BLOCK: u64 = 1024;
		const COUNT: u64 = 100;
		let origin = moq_tokio::origin::spawn();
		let (_broadcast, _catalog, mut audio) = publish(&origin);
		let recorder = Recorder::default();
		let player = tokio::spawn(media(&origin, Duration::from_millis(50), recorder.clone()).run());
		let start = Instant::now();
		for index in 0..COUNT {
			let ready = ((index + 1) * PACKET).div_ceil(BLOCK) * BLOCK;
			kio::clock::sleep_until(start.saturating_add(Duration::from_secs_f64(ready as f64 / SAMPLE_RATE as f64)))
				.await;
			audio.write(packet(index, 0.25)).unwrap();
		}
		settle(player, &recorder).await;
		assert_eq!(
			played(&recorder, 0.25).0,
			PACKET_DURATION * COUNT as u32,
			"a packet was skipped"
		);
	}

	/// The 61-frame tune-in burst from #3946 must reach the clock before the
	/// window drains its first picture, regardless of the raw queue's capacity.
	#[tokio::test]
	async fn a_wide_delay_observes_the_whole_tune_in_burst() {
		tokio::time::pause();
		let delay = Duration::from_secs(2);
		let origin = moq_tokio::origin::spawn();
		let broadcast = origin.route("room", moq_net::origin::RouteOptions::hidden()).unwrap();
		let track = broadcast
			.broadcast()
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let mut producer = moq_mux::container::Producer::new(track, Container::Legacy(moq_mux::container::Kind::Data));
		let mut config = moq_video::encode::Config::new(64, 64, moq_video::FrameRate::new(30, 1).unwrap());
		config.kind = moq_video::encode::Kind::Software;
		config.join = Duration::from_secs(4);
		let mut encoder = moq_video::encode::Encoder::new(&config).unwrap();
		for index in 0..=60 {
			let surface = moq_video::Surface::rgba(&vec![128; 64 * 64 * 4], moq_video::Size::new(64, 64)).unwrap();
			let frame = moq_video::Frame::new(surface, moq_net::Timestamp::from_millis(index * 33).unwrap());
			encoder.encode(&frame).unwrap();
			while let Some(encoded) = encoder.receive().unwrap() {
				producer
					.write(moq_mux::container::Frame {
						timestamp: encoded.timestamp,
						duration: None,
						payload: encoded.payload,
						keyframe: index == 0,
					})
					.unwrap();
			}
		}
		let _finished = producer.finish().unwrap();
		let catalog = hang::catalog::VideoConfig::new(hang::catalog::H264 {
			inline: true,
			profile: 0x42,
			constraints: 0,
			level: 30,
		});
		let mut options = moq_video::decode::Options::new();
		options.decoder.kind = moq_video::decode::Kind::Software;
		options.delay.max = Some(delay);
		let decoder = moq_video::decode::Sink::open(&catalog, &options.decoder).await.unwrap();
		let subscriber = broadcast
			.broadcast()
			.consume()
			.track("video")
			.unwrap()
			.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(delay),
						max: Some(delay),
					},
				)]),
			)
			.await
			.unwrap();
		let track = moq_mux::container::Consumer::new(subscriber, Container::try_from(&catalog).unwrap());
		let recorder = Recorder::default();
		let media = media(&origin, delay, recorder.clone());
		let now = Instant::now();
		let last = moq_net::Timestamp::from_millis(60 * 33).unwrap();
		let task = tokio::spawn(
			Video {
				presentation: media.presentation.clone(),
				frames: media.video.clone(),
				changed: media.drained.clone(),
				output: recorder.clone(),
				max_delay: delay,
				min_delay: delay,
				delay: tokio::sync::watch::channel(None).1,
			}
			.run(track, decoder),
		);
		loop {
			tokio::task::yield_now().await;
			if media.video.lock().unwrap().len() == 30
				|| media.presentation.due(last) == Some(now.saturating_add(delay))
			{
				break;
			}
		}
		let due = media.presentation.due(last);
		task.abort();
		let _ = task.await;
		assert_eq!(
			due,
			Some(now.saturating_add(delay)),
			"the decoder did not observe the live edge"
		);
		assert!(recorder.present(&media).is_none(), "nothing is due before its delay");
	}
}
