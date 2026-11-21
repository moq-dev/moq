//! Rendition selection: which half of the pipeline still needs a track, and
//! which catalog snapshot it may pick one from.

use std::collections::{HashMap, HashSet};

use hang::catalog::Reference;
use moq_mux::catalog;

/// A half of the player: the picture or the speaker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
	Video,
	Audio,
}

/// The track a rendition plays from: its resolved broadcast path and its name there.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct Track {
	/// Resolved rather than as written, since several references name one broadcast.
	broadcast: Option<moq_net::Path>,
	name: String,
}

impl Track {
	/// The track the rendition `id`, configured by `config` in `source`'s catalog, plays from.
	pub(super) fn of(source: &moq_mux::Source, config: &impl Reference, id: &str) -> Self {
		Self {
			broadcast: source.resolve_reference(config.broadcast()),
			name: config.track(id).to_string(),
		}
	}

	/// A track on the catalog's own broadcast, for tests.
	#[cfg(test)]
	pub(super) fn named(name: &str) -> Self {
		Self {
			broadcast: None,
			name: name.to_string(),
		}
	}
}

/// Why a half picks a replacement while the rendition it plays plays on, which it stops only
/// once the replacement is ready.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Switch {
	/// The viewport turned away from the rendition, so only one shaped like it is better.
	Turn,
	/// The catalog moved the rendition to another track, so any replacement is wanted, whichever
	/// way the window turns. It stays wanted until one starts: a failed attempt tries again from
	/// the next snapshot.
	Move,
}

/// Which catalog snapshot a half last read. A half reads a snapshot once, so a track that ends
/// with nothing newer on offer stays stopped rather than resubscribing to the rendition it just
/// finished, and doing it again every time that resubscription ends.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Read {
	#[default]
	Unread,
	/// Every enabled rendition in the snapshot was considered.
	Full(u64),
	/// The snapshot was read for a turn, which skipped the renditions shaped against the window,
	/// so the half reads it again in full once the rendition playing ends.
	Narrowed(u64),
}

impl Read {
	fn covers(self, snapshot: u64) -> bool {
		matches!(self, Self::Full(read) | Self::Narrowed(read) if read == snapshot)
	}

	/// What is left read once the rendition playing ends: the renditions a turn skipped are no
	/// worse than nothing.
	fn ended(self) -> Self {
		match self {
			Self::Narrowed(_) => Self::Unread,
			read => read,
		}
	}
}

/// A rendition a half plays: the task playing it, the track it plays from, and `data`, what
/// the player holds for it, such as the handle that stops it when dropped.
pub(super) struct Running<D> {
	pub(super) task: u64,
	pub(super) track: Track,
	pub(super) data: D,
}

/// Where a half is in playing a rendition, each phase holding what only it has. `O` is a
/// rendition being opened, which replaces the one playing once it is ready.
enum Phase<D, O> {
	/// Nothing plays.
	Idle { opening: Option<O> },
	/// A rendition plays, and its end is a finish.
	Playing { running: Running<D> },
	/// A rendition plays while a replacement is picked, and stops once one is ready. Its end is
	/// a finish only on a turn: a move left its track.
	Switching {
		running: Running<D>,
		switch: Switch,
		opening: Option<O>,
	},
	/// The catalog moved the rendition, which plays out what it holds and ends; that end is no
	/// finish, so a move back can play the track again.
	Retiring { running: Running<D> },
	/// The rendition was stopped, as when the catalog disabled it, and its task is ending.
	Stopping { task: u64, opening: Option<O> },
}

/// One half of the player: its phase, and the snapshot it last read, which every phase keeps.
pub(super) struct Half<D, O> {
	phase: Phase<D, O>,
	read: Read,
}

impl<D, O> Default for Half<D, O> {
	fn default() -> Self {
		Self {
			phase: Phase::Idle { opening: None },
			read: Read::Unread,
		}
	}
}

impl<D, O> Half<D, O> {
	fn take(&mut self) -> Phase<D, O> {
		std::mem::replace(&mut self.phase, Phase::Idle { opening: None })
	}

	/// Whether a task is driving this half.
	pub(super) fn playing(&self) -> bool {
		!matches!(self.phase, Phase::Idle { .. })
	}

	/// Whether a task is driving this half or a rendition is opening for it.
	fn busy(&self) -> bool {
		!matches!(self.phase, Phase::Idle { opening: None })
	}

	/// The rendition playing, unless it was stopped.
	pub(super) fn running(&self) -> Option<&Running<D>> {
		match &self.phase {
			Phase::Playing { running } | Phase::Switching { running, .. } | Phase::Retiring { running } => {
				Some(running)
			}
			Phase::Idle { .. } | Phase::Stopping { .. } => None,
		}
	}

	/// Whether `task` drives this half, rather than being one a replacement already stopped.
	pub(super) fn is_current(&self, task: u64) -> bool {
		match &self.phase {
			Phase::Stopping { task: stopping, .. } => *stopping == task,
			_ => self.running().is_some_and(|running| running.task == task),
		}
	}

	/// The replacement being picked, if any.
	pub(super) fn switching(&self) -> Option<Switch> {
		match &self.phase {
			Phase::Switching { switch, .. } => Some(*switch),
			_ => None,
		}
	}

	/// The rendition being opened, if any.
	pub(super) fn opening(&self) -> Option<&O> {
		match &self.phase {
			Phase::Idle { opening } | Phase::Switching { opening, .. } | Phase::Stopping { opening, .. } => {
				opening.as_ref()
			}
			Phase::Playing { .. } | Phase::Retiring { .. } => None,
		}
	}

	pub(super) fn opening_mut(&mut self) -> Option<&mut O> {
		match &mut self.phase {
			Phase::Idle { opening } | Phase::Switching { opening, .. } | Phase::Stopping { opening, .. } => {
				opening.as_mut()
			}
			Phase::Playing { .. } | Phase::Retiring { .. } => None,
		}
	}

	/// Open `opening`, replacing any still opening, which a newer pick overrides. Only a half
	/// that wants a rendition picks one.
	pub(super) fn open(&mut self, opening: O) {
		match &mut self.phase {
			Phase::Idle { opening: slot } | Phase::Switching { opening: slot, .. } => *slot = Some(opening),
			Phase::Playing { .. } | Phase::Retiring { .. } | Phase::Stopping { .. } => {
				debug_assert!(false, "opened a rendition for a half that wanted none")
			}
		}
	}

	/// Give up the rendition being picked, which found nothing to open or is no longer wanted.
	/// A turn's switch ends with it, keeping what plays; a move's stays wanted, so the next
	/// snapshot tries again.
	pub(super) fn give_up(&mut self) {
		self.phase = match self.take() {
			Phase::Idle { .. } => Phase::Idle { opening: None },
			Phase::Switching {
				running,
				switch: Switch::Turn,
				..
			} => Phase::Playing { running },
			Phase::Switching {
				running,
				switch: Switch::Move,
				..
			} => Phase::Switching {
				running,
				switch: Switch::Move,
				opening: None,
			},
			Phase::Stopping { task, .. } => Phase::Stopping { task, opening: None },
			phase => phase,
		};
	}

	/// The viewport turned away from the rendition playing, so pick a replacement shaped like it
	/// from the snapshot on hand. A move stays a move.
	pub(super) fn turn(&mut self) {
		self.read = Read::Unread;
		self.phase = match self.take() {
			Phase::Playing { running } => Phase::Switching {
				running,
				switch: Switch::Turn,
				opening: None,
			},
			phase => phase,
		};
	}

	/// The catalog moved the rendition playing to another track, so pick any replacement from
	/// the snapshot on hand while it plays on.
	pub(super) fn move_off(&mut self) {
		self.read = Read::Unread;
		self.phase = match self.take() {
			Phase::Playing { running } => Phase::Switching {
				running,
				switch: Switch::Move,
				opening: None,
			},
			Phase::Switching { running, opening, .. } => Phase::Switching {
				running,
				switch: Switch::Move,
				opening,
			},
			phase => phase,
		};
	}

	/// The catalog moved the rendition back onto the track it plays, so nothing replaces it and
	/// its end is a finish again.
	pub(super) fn stay(&mut self) {
		self.phase = match self.take() {
			Phase::Switching {
				running,
				switch: Switch::Move,
				..
			} => Phase::Playing { running },
			phase => phase,
		};
	}

	/// Pick again from the snapshot on hand, as when the viewport changed under a pick.
	pub(super) fn repick(&mut self) {
		self.read = Read::Unread;
	}

	/// The catalog moved the rendition, which now plays out what it holds and ends. Returns
	/// whether it was playing, and so is told to retire now.
	pub(super) fn retire(&mut self) -> bool {
		let mut retired = false;
		self.phase = match self.take() {
			Phase::Playing { running } => {
				retired = true;
				Phase::Retiring { running }
			}
			phase => phase,
		};
		retired
	}

	/// Stop the rendition playing, as when the catalog disabled it, dropping what the player
	/// holds for it. A rendition still opening carries on.
	pub(super) fn stop(&mut self) {
		self.phase = match self.take() {
			Phase::Playing { running } | Phase::Retiring { running } => Phase::Stopping {
				task: running.task,
				opening: None,
			},
			Phase::Switching { running, opening, .. } => Phase::Stopping {
				task: running.task,
				opening,
			},
			phase => phase,
		};
	}
}

/// What is playing, and whether anything else still can.
///
/// A track ending is not the end of playback. Audio and video end
/// independently, and either can end while the broadcast plays on: a publisher
/// retires a rendition (a transcode ladder resizing under a source that changed
/// resolution) by naming the replacement in a catalog snapshot and only then
/// finishing the retired track. That snapshot therefore lands while the doomed
/// track is still playing, so it is held onto and read again once the track
/// ends. Playback stops only once the catalog itself is over.
#[derive(Default)]
pub(super) struct Playback {
	/// The newest snapshot, held until every half has read it.
	latest: Option<catalog::hang::Catalog>,
	/// How many snapshots have arrived. A half records this rather than the
	/// catalog itself, so "newer than the one I read" costs a comparison and
	/// survives intermediate snapshots being dropped.
	snapshots: u64,
	/// Whether anything ever played, so a catalog with nothing playable in it
	/// reports why rather than exiting as a success.
	pub(super) played: bool,
	/// Set once the catalog track ends, which disarms its branch (a stream that
	/// has returned `None` returns it forever, so polling it again spins) and
	/// means no replacement rendition can arrive.
	pub(super) catalog_ended: bool,
	/// Tracks that ended on their own. A name always means the same content, so
	/// one that finished has nothing more to play: selecting it again from a
	/// newer snapshot would only replay its newest group.
	finished: HashSet<Track>,
	/// The tracks of tasks a replacement stopped, by task, whose own finish still counts if
	/// they got there first. A track the catalog moved off is not among them.
	replaced: HashMap<u64, Track>,
}

impl Playback {
	/// Hold onto a snapshot, which may not be read until a track ends.
	pub(super) fn received(&mut self, snapshot: catalog::hang::Catalog) {
		self.snapshots += 1;
		self.latest = Some(snapshot);
	}

	/// The newest snapshot, whether or not every half has read it.
	pub(super) fn latest(&self) -> Option<&catalog::hang::Catalog> {
		self.latest.as_ref()
	}

	/// Whether `track` already ended on its own.
	pub(super) fn finished(&self, track: &Track) -> bool {
		self.finished.contains(track)
	}

	/// Whether `half` needs a rendition, or a replacement for the one it plays, and hasn't
	/// already looked for one in the snapshot on hand.
	pub(super) fn wants<D, O>(&self, half: &Half<D, O>) -> bool {
		matches!(half.phase, Phase::Idle { .. } | Phase::Switching { .. }) && !half.read.covers(self.snapshots)
	}

	/// Record that `half` read the snapshot on hand, whether or not it found anything playable
	/// in it. `narrowed` if a turn skipped some of it.
	pub(super) fn read<D, O>(&self, half: &mut Half<D, O>, narrowed: bool) {
		half.read = match narrowed {
			true => Read::Narrowed(self.snapshots),
			false => Read::Full(self.snapshots),
		};
	}

	/// `running` started playing for `half`, stopping the rendition it replaces.
	pub(super) fn started<D, O>(&mut self, half: &mut Half<D, O>, running: Running<D>) {
		self.played = true;
		let replaced = match half.take() {
			Phase::Playing { running } => Some(running),
			Phase::Switching {
				running,
				switch: Switch::Turn,
				..
			} => Some(running),
			// A move left the track, and a stopped task's end is no finish.
			Phase::Switching { .. } | Phase::Retiring { .. } | Phase::Idle { .. } | Phase::Stopping { .. } => None,
		};
		// Dropping the rest of what it held stops it.
		if let Some(replaced) = replaced {
			self.replaced.insert(replaced.task, replaced.track);
		}
		// A turn's replacement started from a narrowed read, which stays narrowed: once the
		// replacement ends, the renditions the turn skipped are read again.
		half.phase = Phase::Playing { running };
	}

	/// Record `task` ending, `finished` if its track ended on its own rather than being stopped.
	/// The half it drives picks again; one a replacement already stopped changes nothing but
	/// whether its own track finished first.
	pub(super) fn ended<D, O>(&mut self, half: &mut Half<D, O>, task: u64, finished: bool) {
		if let Some(track) = self.replaced.remove(&task)
			&& finished
		{
			self.finished.insert(track);
		}
		if !half.is_current(task) {
			return;
		}
		let (finishes, opening) = match half.take() {
			Phase::Playing { running } => (Some(running.track), None),
			Phase::Switching {
				running,
				switch: Switch::Turn,
				opening,
			} => (Some(running.track), opening),
			Phase::Switching { opening, .. } | Phase::Stopping { opening, .. } => (None, opening),
			Phase::Retiring { .. } | Phase::Idle { .. } => (None, None),
		};
		if let Some(track) = finishes
			&& finished
		{
			self.finished.insert(track);
		}
		// A rendition still opening plays once ready, with nothing left to replace.
		half.phase = Phase::Idle { opening };
		half.read = half.read.ended();
	}

	/// The snapshot to pick renditions from, if either half still needs one.
	pub(super) fn pending<V, VO, A, AO>(
		&self,
		video: &Half<V, VO>,
		audio: &Half<A, AO>,
	) -> Option<&catalog::hang::Catalog> {
		let snapshot = self.latest.as_ref()?;
		(self.wants(video) || self.wants(audio)).then_some(snapshot)
	}

	/// Whether the catalog is still worth reading.
	///
	/// Deliberately blind to what is playing: the snapshot that retires a
	/// rendition arrives while both halves are still running, and it is the only
	/// warning we get.
	pub(super) fn following(&self) -> bool {
		!self.catalog_ended
	}

	/// True once nothing is playing or opening and nothing more can start.
	///
	/// A snapshot no half has read yet still can: the last thing a catalog says
	/// before it ends may be the rendition that replaces the one just retired.
	pub(super) fn done<V, VO, A, AO>(&self, video: &Half<V, VO>, audio: &Half<A, AO>) -> bool {
		self.catalog_ended && !video.busy() && !audio.busy() && self.pending(video, audio).is_none()
	}
}

/// What a finished task returned, or `None` if it was cancelled (on the way out, where which
/// half it was no longer matters). Its own result is left to the caller, which ignores a task a
/// replacement already stopped.
pub(super) fn joined<T>(result: Result<T, tokio::task::JoinError>) -> anyhow::Result<Option<T>> {
	match result {
		Ok(ended) => Ok(Some(ended)),
		Err(err) if err.is_cancelled() => Ok(None),
		Err(err) => Err(err.into()),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// A half whose renditions hold nothing, opening a named rendition.
	type TestHalf = Half<(), &'static str>;

	fn track(name: &str) -> Track {
		Track {
			broadcast: None,
			name: name.to_string(),
		}
	}

	fn running(task: u64, name: &str) -> Running<()> {
		Running {
			task,
			track: track(name),
			data: (),
		}
	}

	/// Both halves, with a snapshot on hand that both have read and `video` playing as task 1.
	fn playing(video: &str) -> (Playback, TestHalf, TestHalf) {
		let mut playback = Playback::default();
		let (mut video_half, mut audio) = (TestHalf::default(), TestHalf::default());
		playback.received(Default::default());
		playback.read(&mut video_half, false);
		playback.started(&mut video_half, running(1, video));
		playback.read(&mut audio, false);
		(playback, video_half, audio)
	}

	/// Only a track that ended on its own is finished: a disabled rendition was
	/// stopped and may be enabled again.
	#[test]
	fn only_an_ended_track_is_finished() {
		let mut playback = Playback::default();
		let mut video = TestHalf::default();
		playback.started(&mut video, running(1, "video"));
		playback.ended(&mut video, 1, false);
		assert!(!playback.finished(&track("video")));

		playback.started(&mut video, running(2, "video"));
		playback.ended(&mut video, 2, true);
		assert!(playback.finished(&track("video")));
		assert!(!playback.finished(&track("audio")));
	}

	/// A publisher retiring a rendition (a transcode ladder resizing under a
	/// source that changed resolution) names the replacement in a catalog
	/// snapshot and only then finishes the retired track, at the end of the group
	/// it was mid-way through. So the snapshot lands while both halves are still
	/// playing and has to be kept: it is the only warning the player gets, and
	/// the half whose track ends afterwards reads it then.
	#[test]
	fn a_finished_track_re_arms_its_half() {
		let (mut playback, mut video, mut audio) = playing("video");
		playback.started(&mut audio, running(2, "audio"));

		// The snapshot naming the replacement, while the retired track plays on.
		playback.received(Default::default());
		assert!(
			playback.pending(&video, &audio).is_none(),
			"nothing to start while both halves play"
		);

		playback.ended(&mut video, 1, true);
		assert!(!playback.done(&video, &audio), "playback ended on a retired rendition");
		assert!(audio.playing(), "audio ended with the video rendition");
		assert!(
			playback.pending(&video, &audio).is_some(),
			"the retirement snapshot was dropped"
		);
		assert!(playback.wants(&video), "the replacement was never looked for");
		assert!(!playback.wants(&audio), "audio is still playing its own rendition");

		// The replacement lands and playback carries on.
		playback.read(&mut video, false);
		playback.started(&mut video, running(3, "video"));
		assert!(playback.pending(&video, &audio).is_none());
		assert!(!playback.done(&video, &audio));
	}

	/// A track that ends with nothing newer on offer stays stopped. Reading the
	/// snapshot it was selected from again would resubscribe to the rendition
	/// that just finished, and do it again the moment that ended too.
	#[test]
	fn a_read_snapshot_is_not_read_twice() {
		// A video-only catalog: both halves read the snapshot, only one found something in it.
		let (mut playback, mut video, audio) = playing("video");

		playback.ended(&mut video, 1, true);
		assert!(
			playback.pending(&video, &audio).is_none(),
			"the player would resubscribe in a loop"
		);

		playback.received(Default::default());
		assert!(
			playback.pending(&video, &audio).is_some(),
			"a fresh snapshot must be read"
		);
	}

	/// A switching half picks a replacement from the snapshot on hand while it plays on, and
	/// picks once per snapshot: a newer one asks again until the replacement starts.
	#[test]
	fn a_switching_half_picks_while_it_plays() {
		let (mut playback, mut video, mut audio) = playing("wide");
		assert!(playback.pending(&video, &audio).is_none());

		video.turn();
		assert!(playback.wants(&video), "a switch picks from the snapshot on hand");
		playback.read(&mut video, false);
		assert!(playback.pending(&video, &audio).is_none(), "picked once per snapshot");
		playback.received(Default::default());
		assert!(playback.wants(&video), "a newer snapshot picks again");
		playback.read(&mut video, false);

		playback.started(&mut video, running(2, "tall"));
		playback.received(Default::default());
		assert!(!playback.wants(&video), "the replacement plays");
		playback.read(&mut audio, false);

		// Given up, the half keeps playing what it has.
		video.turn();
		video.give_up();
		assert!(!playback.wants(&video));
		assert!(video.playing());
		assert_eq!(video.switching(), None);
	}

	/// A move's replacement stays wanted after an attempt finds nothing, so the next snapshot
	/// tries again rather than leaving the moved rendition playing; the same snapshot is not
	/// tried twice. Moving back onto the playing track ends the switch.
	#[test]
	fn a_failed_move_tries_again_from_the_next_snapshot() {
		let (mut playback, mut video, _audio) = playing("a");

		video.move_off();
		playback.read(&mut video, false);
		video.give_up();
		assert_eq!(video.switching(), Some(Switch::Move));
		assert!(!playback.wants(&video), "the same snapshot is tried once");

		// A turn doesn't narrow a move.
		video.turn();
		assert_eq!(video.switching(), Some(Switch::Move));
		playback.read(&mut video, false);

		playback.received(Default::default());
		assert!(playback.wants(&video), "the move was given up");

		video.stay();
		assert!(!playback.wants(&video));
		playback.ended(&mut video, 1, true);
		assert!(
			playback.finished(&track("a")),
			"a track moved back onto is finished by its end"
		);
	}

	/// A turn tries only renditions shaped like the window. Once the rendition it would have
	/// replaced ends, the rest are no worse than nothing, so the half reads the snapshot again.
	#[test]
	fn a_narrowed_read_is_read_again_once_its_rendition_ends() {
		let (mut playback, mut video, audio) = playing("wide");

		video.turn();
		playback.read(&mut video, true);
		video.give_up();
		assert_eq!(video.switching(), None);
		assert!(playback.pending(&video, &audio).is_none());

		playback.ended(&mut video, 1, true);
		assert!(
			playback.wants(&video),
			"the renditions the turn skipped were never tried"
		);
		playback.read(&mut video, false);
		playback.started(&mut video, running(2, "tall"));
		playback.ended(&mut video, 2, false);
		assert!(!playback.wants(&video), "a full read is not read again");
	}

	/// A turn that starts its replacement still skipped the renditions shaped against the
	/// window, so once that replacement finishes, the half reads the snapshot again in full and
	/// can go back to the rendition it turned away from.
	#[test]
	fn a_turns_replacement_ending_reads_its_snapshot_again() {
		let (mut playback, mut video, audio) = playing("wide");

		video.turn();
		playback.read(&mut video, true);
		playback.started(&mut video, running(2, "tall"));
		assert!(playback.pending(&video, &audio).is_none());

		playback.ended(&mut video, 2, true);
		assert!(playback.finished(&track("tall")));
		assert!(!playback.finished(&track("wide")), "the turn stopped it");
		assert!(playback.wants(&video), "the rendition the turn left is still playable");
	}

	/// A task a replacement stopped no longer drives the half, but if it finished its own track
	/// before the stop reached it, that track is finished, so a later pick skips it.
	#[test]
	fn a_replaced_track_that_finished_is_finished() {
		let (mut playback, mut video, _audio) = playing("wide");
		video.turn();
		playback.started(&mut video, running(2, "tall"));
		assert!(!video.is_current(1));

		playback.ended(&mut video, 1, true);
		assert!(playback.finished(&track("wide")));
		assert!(video.playing(), "its replacement still plays");
		assert!(video.is_current(2));
	}

	/// A track the catalog moved off is no finish when its task ends, whether a replacement
	/// stopped it first or not.
	#[test]
	fn a_moved_track_is_never_finished() {
		let (mut playback, mut video, _audio) = playing("a");
		video.move_off();
		playback.started(&mut video, running(2, "b"));
		playback.ended(&mut video, 1, true);
		assert!(!playback.finished(&track("a")), "replaced after a move");

		video.move_off();
		playback.ended(&mut video, 2, true);
		assert!(!playback.finished(&track("b")), "ended mid-move");
		assert!(!video.playing());
	}

	/// A retired rendition plays out and ends, and that end is no finish.
	#[test]
	fn a_retired_rendition_is_not_finished() {
		let (mut playback, mut audio, _video) = playing("audio");
		assert!(audio.retire());
		assert!(!audio.retire(), "told to retire once");
		assert!(audio.playing());
		assert!(!playback.wants(&audio), "a retiring half waits for its end");
		playback.ended(&mut audio, 1, true);
		assert!(!playback.finished(&track("audio")));
		assert!(!audio.playing());
	}

	/// A stopped rendition still drives its half until its task ends, so nothing is picked and
	/// playback does not end under it; a rendition opening for it carries on.
	#[test]
	fn a_stopped_rendition_drives_its_half_until_it_ends() {
		let (mut playback, mut video, mut audio) = playing("a");
		video.move_off();
		video.open("b");
		video.stop();
		assert!(video.running().is_none());
		assert!(video.is_current(1));
		assert_eq!(video.opening(), Some(&"b"));

		playback.received(Default::default());
		playback.catalog_ended = true;
		assert!(!playback.wants(&video), "the stopped task still drives it");
		assert!(!playback.done(&video, &audio));

		playback.ended(&mut video, 1, false);
		assert_eq!(video.opening(), Some(&"b"), "the opening plays once ready");
		assert!(!playback.done(&video, &audio));
		video.give_up();
		playback.read(&mut video, false);
		playback.read(&mut audio, false);
		assert!(playback.done(&video, &audio));
	}

	/// A rendition that ends while its replacement opens leaves the opening to play once ready.
	#[test]
	fn a_replacement_still_opening_outlives_the_rendition_it_replaces() {
		let (mut playback, mut video, _audio) = playing("wide");
		video.turn();
		playback.read(&mut video, true);
		video.open("tall");

		playback.ended(&mut video, 1, true);
		assert!(!video.playing());
		assert_eq!(video.opening(), Some(&"tall"));
		assert!(
			playback.finished(&track("wide")),
			"a turn's rendition finishes by its end"
		);

		playback.started(&mut video, running(2, "tall"));
		assert!(video.opening().is_none());
		assert!(video.is_current(2));
	}

	/// Turning back gives up a turn's opening and keeps what plays; a move's switch stays
	/// wanted when its opening is given up, and moving back drops the opening.
	#[test]
	fn giving_up_ends_a_turn_but_not_a_move() {
		let (playback, mut video, _audio) = playing("wide");
		video.turn();
		playback.read(&mut video, true);
		video.open("tall");
		video.give_up();
		assert_eq!(video.switching(), None);
		assert!(video.opening().is_none());

		video.move_off();
		video.open("other");
		video.give_up();
		assert_eq!(video.switching(), Some(Switch::Move));
		assert!(video.opening().is_none());

		video.open("other");
		video.stay();
		assert_eq!(video.switching(), None);
		assert!(video.opening().is_none(), "nothing replaces a rendition moved back");
	}

	/// The catalog track ending is what ends playback, since it is the only thing
	/// that rules out a replacement rendition. Tracks ending before it just stop
	/// their own half.
	#[test]
	fn playback_ends_with_the_catalog() {
		let mut playback = Playback::default();
		let (mut video, audio) = (TestHalf::default(), TestHalf::default());
		playback.started(&mut video, running(1, "video"));

		playback.catalog_ended = true;
		assert!(
			!playback.done(&video, &audio),
			"playback ended while video was still playing"
		);

		playback.ended(&mut video, 1, true);
		assert!(playback.done(&video, &audio));
	}

	/// The catalog's last word can be the replacement for the rendition it
	/// retires, and the retired track outlives the catalog by the group it was
	/// mid-way through. So a half that has not read the final snapshot yet is
	/// still a half that can start something, however finished everything else
	/// looks.
	#[test]
	fn a_final_snapshot_outlives_the_catalog() {
		let (mut playback, mut video, mut audio) = playing("video");

		// The last snapshot names the replacement, then the catalog ends, then the
		// retired track does.
		playback.received(Default::default());
		playback.catalog_ended = true;
		playback.ended(&mut video, 1, true);

		assert!(
			playback.pending(&video, &audio).is_some(),
			"the final snapshot was never offered"
		);
		assert!(
			!playback.done(&video, &audio),
			"playback ended with a replacement still unread"
		);

		// Read it for both halves, the way the selection pass does, find nothing
		// playable in it, and only then stop.
		playback.read(&mut video, false);
		playback.read(&mut audio, false);
		assert!(playback.done(&video, &audio));
	}
}
