//! Rendition selection: which half of the pipeline still needs a track, and
//! which catalog snapshot it may pick one from.

use std::collections::HashSet;

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

/// One half of the pipeline: whether it is playing, and which catalog snapshot
/// it last picked a rendition from.
#[derive(Default)]
struct Half {
	playing: bool,
	/// The replacement being picked while the rendition playing plays on.
	switching: Option<Switch>,
	/// The snapshot was last read for a turn, which skipped the renditions shaped against the
	/// window, so the half reads it again in full once its rendition ends.
	narrowed: bool,
	/// The track this half is playing, whose end is a finish. Cleared once retired.
	track: Option<Track>,
	/// The snapshot this half last read. A half reads a snapshot once, so a
	/// track that ends with nothing newer on offer stays stopped rather than
	/// resubscribing to the rendition it just finished, and doing it again every
	/// time that resubscription ends.
	read: Option<u64>,
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
	video: Half,
	audio: Half,
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
}

impl Playback {
	fn half(&self, kind: Kind) -> &Half {
		match kind {
			Kind::Video => &self.video,
			Kind::Audio => &self.audio,
		}
	}

	fn half_mut(&mut self, kind: Kind) -> &mut Half {
		match kind {
			Kind::Video => &mut self.video,
			Kind::Audio => &mut self.audio,
		}
	}

	/// Hold onto a snapshot, which may not be read until a track ends.
	pub(super) fn received(&mut self, snapshot: catalog::hang::Catalog) {
		self.snapshots += 1;
		self.latest = Some(snapshot);
	}

	pub(super) fn started(&mut self, kind: Kind, track: Track) {
		self.played = true;
		let half = self.half_mut(kind);
		half.playing = true;
		half.switching = None;
		half.narrowed = false;
		half.track = Some(track);
	}

	/// Whether a task is driving this half.
	pub(super) fn playing(&self, kind: Kind) -> bool {
		self.half(kind).playing
	}

	/// Record a task ending, re-arming selection for that half. `finished` is
	/// whether its track ended on its own, rather than being stopped.
	pub(super) fn ended(&mut self, kind: Kind, finished: bool) {
		let half = self.half_mut(kind);
		half.playing = false;
		half.switching = None;
		// The renditions a turn skipped are no worse than nothing.
		if std::mem::take(&mut half.narrowed) {
			half.read = None;
		}
		if let Some(track) = half.track.take()
			&& finished
		{
			self.finished.insert(track);
		}
	}

	/// The catalog moved this half's rendition off the track it plays, which it now
	/// retires: that track ending is no finish, so a move back can play it again.
	pub(super) fn retire(&mut self, kind: Kind) {
		self.half_mut(kind).track = None;
	}

	/// Pick a replacement from the snapshot on hand while this half plays on, as when the
	/// viewport turned away from its rendition or the catalog moved it. A move stays a move.
	pub(super) fn switch(&mut self, kind: Kind, switch: Switch) {
		let half = self.half_mut(kind);
		if half.switching != Some(Switch::Move) {
			half.switching = Some(switch);
		}
		half.read = None;
	}

	/// The catalog moved this half's rendition back onto `track`, which it still plays, so
	/// nothing replaces it and its end is a finish again.
	pub(super) fn stay(&mut self, kind: Kind, track: Track) {
		let half = self.half_mut(kind);
		half.switching = None;
		half.track = Some(track);
	}

	/// The replacement being picked, if any.
	pub(super) fn switching(&self, kind: Kind) -> Option<Switch> {
		self.half(kind).switching
	}

	/// Pick again from the snapshot on hand, as when the viewport changed under a pick.
	pub(super) fn repick(&mut self, kind: Kind) {
		self.half_mut(kind).read = None;
	}

	/// Give up a turn's replacement, which keeps what plays. A move's stays wanted, so the next
	/// snapshot tries again.
	pub(super) fn settle(&mut self, kind: Kind) {
		let half = self.half_mut(kind);
		half.switching.take_if(|switch| *switch == Switch::Turn);
	}

	/// The newest snapshot, whether or not every half has read it.
	pub(super) fn latest(&self) -> Option<&catalog::hang::Catalog> {
		self.latest.as_ref()
	}

	/// Whether `track` already ended on its own.
	pub(super) fn finished(&self, track: &Track) -> bool {
		self.finished.contains(track)
	}

	/// Whether this half needs a rendition, or a replacement for the one it plays, and hasn't
	/// already looked for one in the snapshot on hand.
	pub(super) fn wants(&self, kind: Kind) -> bool {
		let half = self.half(kind);
		(!half.playing || half.switching.is_some()) && half.read != Some(self.snapshots)
	}

	/// Record that this half read the snapshot on hand, whether or not it found
	/// anything playable in it. `narrowed` if a turn skipped some of it.
	pub(super) fn read(&mut self, kind: Kind, narrowed: bool) {
		let snapshots = self.snapshots;
		let half = self.half_mut(kind);
		half.read = Some(snapshots);
		half.narrowed = narrowed;
	}

	/// The snapshot to pick renditions from, if either half still needs one.
	pub(super) fn pending(&self) -> Option<&catalog::hang::Catalog> {
		let snapshot = self.latest.as_ref()?;
		(self.wants(Kind::Video) || self.wants(Kind::Audio)).then_some(snapshot)
	}

	/// Whether the catalog is still worth reading.
	///
	/// Deliberately blind to what is playing: the snapshot that retires a
	/// rendition arrives while both halves are still running, and it is the only
	/// warning we get.
	pub(super) fn following(&self) -> bool {
		!self.catalog_ended
	}

	/// True once nothing is playing and nothing more can start.
	///
	/// A snapshot no half has read yet still can: the last thing a catalog says
	/// before it ends may be the rendition that replaces the one just retired.
	pub(super) fn done(&self) -> bool {
		self.catalog_ended && !self.video.playing && !self.audio.playing && self.pending().is_none()
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

	fn track(name: &str) -> Track {
		Track {
			broadcast: None,
			name: name.to_string(),
		}
	}

	/// Only a track that ended on its own is finished: a disabled rendition was
	/// stopped and may be enabled again.
	#[test]
	fn only_an_ended_track_is_finished() {
		let mut playback = Playback::default();
		playback.started(Kind::Video, track("video"));
		playback.ended(Kind::Video, false);
		assert!(!playback.finished(&track("video")));

		playback.started(Kind::Video, track("video"));
		playback.ended(Kind::Video, true);
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
		let mut playback = Playback::default();
		playback.received(Default::default());
		playback.started(Kind::Video, track("video"));
		playback.read(Kind::Video, false);
		playback.started(Kind::Audio, track("audio"));
		playback.read(Kind::Audio, false);

		// The snapshot naming the replacement, while the retired track plays on.
		playback.received(Default::default());
		assert!(playback.pending().is_none(), "nothing to start while both halves play");

		playback.ended(Kind::Video, true);
		assert!(!playback.done(), "playback ended on a retired rendition");
		assert!(playback.audio.playing, "audio ended with the video rendition");
		assert!(playback.pending().is_some(), "the retirement snapshot was dropped");
		assert!(playback.wants(Kind::Video), "the replacement was never looked for");
		assert!(!playback.wants(Kind::Audio), "audio is still playing its own rendition");

		// The replacement lands and playback carries on.
		playback.read(Kind::Video, false);
		playback.started(Kind::Video, track("video"));
		assert!(playback.pending().is_none());
		assert!(!playback.done());
	}

	/// A track that ends with nothing newer on offer stays stopped. Reading the
	/// snapshot it was selected from again would resubscribe to the rendition
	/// that just finished, and do it again the moment that ended too.
	#[test]
	fn a_read_snapshot_is_not_read_twice() {
		let mut playback = Playback::default();
		playback.received(Default::default());
		// A video-only catalog: both halves read the snapshot, only one found
		// something in it.
		playback.read(Kind::Video, false);
		playback.started(Kind::Video, track("video"));
		playback.read(Kind::Audio, false);

		playback.ended(Kind::Video, true);
		assert!(playback.pending().is_none(), "the player would resubscribe in a loop");

		playback.received(Default::default());
		assert!(playback.pending().is_some(), "a fresh snapshot must be read");
	}

	/// A switching half picks a replacement from the snapshot on hand while it plays on, and
	/// picks once per snapshot: a newer one asks again until the replacement starts.
	#[test]
	fn a_switching_half_picks_while_it_plays() {
		let mut playback = Playback::default();
		playback.received(Default::default());
		playback.read(Kind::Video, false);
		playback.started(Kind::Video, track("wide"));
		playback.read(Kind::Audio, false);
		assert!(playback.pending().is_none());

		playback.switch(Kind::Video, Switch::Turn);
		assert!(playback.wants(Kind::Video), "a switch picks from the snapshot on hand");
		playback.read(Kind::Video, false);
		assert!(playback.pending().is_none(), "picked once per snapshot");
		playback.received(Default::default());
		assert!(playback.wants(Kind::Video), "a newer snapshot picks again");
		playback.read(Kind::Video, false);

		playback.started(Kind::Video, track("tall"));
		playback.received(Default::default());
		assert!(!playback.wants(Kind::Video), "the replacement plays");
		playback.read(Kind::Audio, false);

		// Given up, the half keeps playing what it has.
		playback.switch(Kind::Video, Switch::Turn);
		playback.settle(Kind::Video);
		assert!(!playback.wants(Kind::Video));
		assert!(playback.playing(Kind::Video));
	}

	/// A move's replacement stays wanted after an attempt finds nothing, so the next snapshot
	/// tries again rather than leaving the moved rendition playing; the same snapshot is not
	/// tried twice. Moving back onto the playing track ends the switch.
	#[test]
	fn a_failed_move_tries_again_from_the_next_snapshot() {
		let mut playback = Playback::default();
		playback.received(Default::default());
		playback.read(Kind::Video, false);
		playback.started(Kind::Video, track("a"));
		playback.read(Kind::Audio, false);

		playback.retire(Kind::Video);
		playback.switch(Kind::Video, Switch::Move);
		playback.read(Kind::Video, false);
		playback.settle(Kind::Video);
		assert_eq!(playback.switching(Kind::Video), Some(Switch::Move));
		assert!(!playback.wants(Kind::Video), "the same snapshot is tried once");

		// A turn doesn't narrow a move.
		playback.switch(Kind::Video, Switch::Turn);
		assert_eq!(playback.switching(Kind::Video), Some(Switch::Move));
		playback.read(Kind::Video, false);

		playback.received(Default::default());
		assert!(playback.wants(Kind::Video), "the move was given up");

		playback.stay(Kind::Video, track("a"));
		assert!(!playback.wants(Kind::Video));
		playback.ended(Kind::Video, true);
		assert!(
			playback.finished(&track("a")),
			"a track moved back onto is finished by its end"
		);
	}

	/// A turn tries only renditions shaped like the window. Once the rendition it would have
	/// replaced ends, the rest are no worse than nothing, so the half reads the snapshot again.
	#[test]
	fn a_narrowed_read_is_read_again_once_its_rendition_ends() {
		let mut playback = Playback::default();
		playback.received(Default::default());
		playback.read(Kind::Video, false);
		playback.started(Kind::Video, track("wide"));
		playback.read(Kind::Audio, false);

		playback.switch(Kind::Video, Switch::Turn);
		playback.read(Kind::Video, true);
		playback.settle(Kind::Video);
		assert_eq!(playback.switching(Kind::Video), None);
		assert!(playback.pending().is_none());

		playback.ended(Kind::Video, true);
		assert!(
			playback.wants(Kind::Video),
			"the renditions the turn skipped were never tried"
		);
		playback.read(Kind::Video, false);
		playback.ended(Kind::Video, false);
		assert!(!playback.wants(Kind::Video), "a full read is not read again");
	}

	/// The catalog track ending is what ends playback, since it is the only thing
	/// that rules out a replacement rendition. Tracks ending before it just stop
	/// their own half.
	#[test]
	fn playback_ends_with_the_catalog() {
		let mut playback = Playback::default();
		playback.started(Kind::Video, track("video"));

		playback.catalog_ended = true;
		assert!(!playback.done(), "playback ended while video was still playing");

		playback.ended(Kind::Video, true);
		assert!(playback.done());
	}

	/// The catalog's last word can be the replacement for the rendition it
	/// retires, and the retired track outlives the catalog by the group it was
	/// mid-way through. So a half that has not read the final snapshot yet is
	/// still a half that can start something, however finished everything else
	/// looks.
	#[test]
	fn a_final_snapshot_outlives_the_catalog() {
		let mut playback = Playback::default();
		playback.received(Default::default());
		playback.started(Kind::Video, track("video"));
		playback.read(Kind::Video, false);

		// The last snapshot names the replacement, then the catalog ends, then the
		// retired track does.
		playback.received(Default::default());
		playback.catalog_ended = true;
		playback.ended(Kind::Video, true);

		assert!(playback.pending().is_some(), "the final snapshot was never offered");
		assert!(!playback.done(), "playback ended with a replacement still unread");

		// Read it for both halves, the way the selection pass does, find nothing
		// playable in it, and only then stop.
		playback.read(Kind::Video, false);
		playback.read(Kind::Audio, false);
		assert!(playback.done());
	}
}
