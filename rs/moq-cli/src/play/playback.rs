//! Rendition selection: which half of the pipeline still needs a track, and
//! which catalog snapshot it may pick one from.

use moq_mux::catalog;

/// Which half of the pipeline a playback task drives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
	Video,
	Audio,
}

/// One half of the pipeline: whether it is playing, and which catalog snapshot
/// it last picked a rendition from.
#[derive(Default)]
struct Half {
	playing: bool,
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

	pub(super) fn started(&mut self, kind: Kind) {
		self.played = true;
		self.half_mut(kind).playing = true;
	}

	/// Record a task ending, re-arming selection for that half.
	pub(super) fn ended(&mut self, kind: Option<Kind>) {
		if let Some(kind) = kind {
			self.half_mut(kind).playing = false;
		}
	}

	/// Whether this half needs a rendition and hasn't already looked for one in
	/// the snapshot on hand.
	pub(super) fn wants(&self, kind: Kind) -> bool {
		let half = self.half(kind);
		!half.playing && half.read != Some(self.snapshots)
	}

	/// Record that this half read the snapshot on hand, whether or not it found
	/// anything playable in it.
	pub(super) fn read(&mut self, kind: Kind) {
		let snapshots = self.snapshots;
		self.half_mut(kind).read = Some(snapshots);
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

/// The half a finished task was driving, or `None` if it was cancelled (on the
/// way out, where which half it was no longer matters).
pub(super) fn joined(
	result: Result<(Kind, anyhow::Result<()>), tokio::task::JoinError>,
) -> anyhow::Result<Option<Kind>> {
	match result {
		Ok((kind, result)) => result.map(|()| Some(kind)),
		Err(err) if err.is_cancelled() => Ok(None),
		Err(err) => Err(err.into()),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

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
		playback.started(Kind::Video);
		playback.read(Kind::Video);
		playback.started(Kind::Audio);
		playback.read(Kind::Audio);

		// The snapshot naming the replacement, while the retired track plays on.
		playback.received(Default::default());
		assert!(playback.pending().is_none(), "nothing to start while both halves play");

		playback.ended(Some(Kind::Video));
		assert!(!playback.done(), "playback ended on a retired rendition");
		assert!(playback.audio.playing, "audio ended with the video rendition");
		assert!(playback.pending().is_some(), "the retirement snapshot was dropped");
		assert!(playback.wants(Kind::Video), "the replacement was never looked for");
		assert!(!playback.wants(Kind::Audio), "audio is still playing its own rendition");

		// The replacement lands and playback carries on.
		playback.read(Kind::Video);
		playback.started(Kind::Video);
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
		playback.read(Kind::Video);
		playback.started(Kind::Video);
		playback.read(Kind::Audio);

		playback.ended(Some(Kind::Video));
		assert!(playback.pending().is_none(), "the player would resubscribe in a loop");

		playback.received(Default::default());
		assert!(playback.pending().is_some(), "a fresh snapshot must be read");
	}

	/// The catalog track ending is what ends playback, since it is the only thing
	/// that rules out a replacement rendition. Tracks ending before it just stop
	/// their own half.
	#[test]
	fn playback_ends_with_the_catalog() {
		let mut playback = Playback::default();
		playback.started(Kind::Video);

		playback.catalog_ended = true;
		assert!(!playback.done(), "playback ended while video was still playing");

		playback.ended(Some(Kind::Video));
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
		playback.started(Kind::Video);
		playback.read(Kind::Video);

		// The last snapshot names the replacement, then the catalog ends, then the
		// retired track does.
		playback.received(Default::default());
		playback.catalog_ended = true;
		playback.ended(Some(Kind::Video));

		assert!(playback.pending().is_some(), "the final snapshot was never offered");
		assert!(!playback.done(), "playback ended with a replacement still unread");

		// Read it for both halves, the way the selection pass does, find nothing
		// playable in it, and only then stop.
		playback.read(Kind::Video);
		playback.read(Kind::Audio);
		assert!(playback.done());
	}
}
