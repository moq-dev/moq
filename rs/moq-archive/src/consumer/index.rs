//! One track's object keys, listed once and extended after its newest key.

use std::collections::BTreeMap;

use futures::TryStreamExt;
use moq_net::Timescale;
use object_store::path::Path;
use object_store::{ObjectStore, ObjectStoreExt};

use crate::layout::{self, End, Name};
use crate::{Error, Info, Result};

/// Where an object starts: its first sequence and that sequence's first frame index.
pub(super) type Start = (u64, u64);

/// One listed object: where it starts, and its key.
pub(super) type Listing = (Start, Path);

/// What one listing of a track's directory found.
#[derive(Default)]
pub(super) struct Listed {
	/// The objects new since the cursor, oldest first.
	pub objects: Vec<Listing>,
	/// The track's `end`, when asked for.
	pub end: Option<End>,
}

/// A track's stored objects, by where each starts.
pub(super) struct Index {
	pub dir: Path,
	pub timescale: Option<Timescale>,
	/// Each object's key. A group's sequence is its start, so these sort by time too.
	objects: BTreeMap<Start, Path>,
	/// The newest key listed, which the next listing starts after.
	cursor: Path,
}

impl Index {
	pub fn new(dir: Path, info: &Info) -> Result<Self> {
		let cursor = crate::path::push(&dir, layout::TRACK_INFO)?;
		Ok(Self {
			dir,
			timescale: super::track::timescale(info)?,
			objects: BTreeMap::new(),
			cursor,
		})
	}

	pub fn timed(&self) -> bool {
		self.timescale.is_some()
	}

	/// The newest key listed, for the next listing to start after.
	pub fn cursor(&self) -> &Path {
		&self.cursor
	}

	/// Take in the objects a listing after [`Self::cursor`] found.
	pub fn extend(&mut self, objects: &[Listing]) {
		for (start, path) in objects {
			self.objects.insert(*start, path.clone());
			self.cursor = path.clone();
		}
	}

	/// The newest object, which holds the newest stored sequence.
	pub fn newest(&self) -> Option<(Start, Path)> {
		self.objects
			.last_key_value()
			.map(|(start, path)| (*start, path.clone()))
	}

	/// The objects holding group `sequence` from frame `frame` on, oldest first: the last one
	/// starting at or before it, then each continuing the group. Also the start of the first
	/// object after them, which bounds a gap.
	pub fn holding(&self, sequence: u64, frame: u64) -> (Vec<(Start, Path)>, Option<Start>) {
		let mut holding = Vec::new();
		if let Some((start, path)) = self.objects.range(..=(sequence, frame)).next_back() {
			holding.push((*start, path.clone()));
		}
		let mut after = None;
		for (start, path) in self.objects.range((sequence, frame.saturating_add(1))..) {
			if start.0 != sequence {
				after = Some(*start);
				break;
			}
			holding.push((*start, path.clone()));
		}
		(holding, after)
	}

	/// The objects that may hold the newest group starting at or before `limit`, in the track's
	/// timescale, newest first: from the newest object starting a group at or before it, or the
	/// oldest object when none does, through the last object starting at or before it. Every later
	/// object holds only later groups. One continuing a group the next object still continues
	/// starts none, so it is skipped.
	pub fn around(&self, limit: u64) -> Vec<(Start, Path)> {
		let last = (limit, u64::MAX);
		let from = self
			.objects
			.range(..=last)
			.rev()
			.find(|(start, _)| start.1 == 0)
			.map_or((0, 0), |(start, _)| *start);

		let mut around = Vec::new();
		let mut objects = self.objects.range(from..=last).peekable();
		while let Some((start, path)) = objects.next() {
			let continued = start.1 > 0 && objects.peek().is_some_and(|(next, _)| next.0 == start.0);
			if !continued {
				around.push((*start, path.clone()));
			}
		}
		around.reverse();
		around
	}

	/// The object just after the one starting at `start`, or the oldest for `None`.
	pub fn after(&self, start: Option<Start>) -> Option<(Start, Path)> {
		let (start, path) = match start {
			Some(start) => self.objects.range((start.0, start.1.saturating_add(1))..).next()?,
			None => self.objects.first_key_value()?,
		};
		Some((*start, path.clone()))
	}

	/// Forget an object the store no longer has, such as one a DVR pruned.
	pub fn forget(&mut self, start: Start) {
		self.objects.remove(&start);
	}
}

/// List a track's directory after `cursor`, refusing anything the layout does not name, and
/// read its `end` when `end` is set. `.info` sorts before every object key and `end` after,
/// so listing after `.info` lists everything else. Object stores promise no listing order, so
/// it is sorted here.
pub(super) async fn list<S: ObjectStore>(store: &S, dir: &Path, cursor: &Path, end: bool) -> Result<Listed> {
	let metas: Vec<_> = store.list_with_offset(Some(dir), cursor).try_collect().await?;
	let mut listed = Listed::default();
	let mut found = None;
	for meta in metas {
		// A listing is recursive; anything deeper than the track's own entries is foreign.
		if meta.location.prefix_match(dir).is_none_or(|parts| parts.count() != 1) {
			return Err(Error::Path(meta.location.to_string()));
		}
		let filename = meta
			.location
			.filename()
			.ok_or_else(|| Error::Path(meta.location.to_string()))?;
		match Name::parse(filename)? {
			Name::Info => {}
			Name::End => found = Some(meta.location),
			Name::Object(start) => listed.objects.push(((start.group, start.frame), meta.location)),
		}
	}
	listed.objects.sort();
	if end {
		// The producer writes every track's `end` before the broadcast's `.end`.
		let path = found.ok_or_else(|| Error::NotFound(format!("{dir}/{}", layout::TRACK_END)))?;
		listed.end = Some(End::decode(&store.get(&path).await?.bytes().await?)?);
	}
	Ok(listed)
}

/// A track's `.info`.
pub(super) async fn info<S: ObjectStore>(store: &S, dir: &Path) -> Result<Info> {
	let path = crate::path::push(dir, layout::TRACK_INFO)?;
	Info::decode(&store.get(&path).await?.bytes().await?)
}
