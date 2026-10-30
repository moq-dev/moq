//! One track's object keys, listed once and extended after its newest key.

use std::collections::{BTreeMap, BTreeSet};

use futures::TryStreamExt;
use moq_net::{Timescale, Timestamp};
use object_store::path::Path;
use object_store::{ObjectStore, ObjectStoreExt};

use crate::layout::{self, End, Name};
use crate::{Error, Info, Result};

/// Where an object starts: its first sequence and that sequence's first frame index.
pub(super) type Start = (u64, u64);

/// One listed object: where it starts, its first frame's timestamp on a timed track, and
/// its key.
pub(super) type Listing = (Start, Option<u64>, Path);

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
	/// Each object's first frame timestamp, on a timed track, and key.
	objects: BTreeMap<Start, (Option<u64>, Path)>,
	/// Each timed object's first frame timestamp with where it starts, so a time finds the
	/// object holding it in one lookup. Timestamps never rewind, so they sort as the starts do.
	times: BTreeSet<(u64, Start)>,
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
			times: BTreeSet::new(),
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
		for (start, pts, path) in objects {
			self.objects.insert(*start, (*pts, path.clone()));
			if let Some(pts) = pts {
				self.times.insert((*pts, *start));
			}
			self.cursor = path.clone();
		}
	}

	/// The newest object, which holds the newest stored sequence.
	pub fn newest(&self) -> Option<(Start, Path)> {
		self.objects
			.last_key_value()
			.map(|(start, (_, path))| (*start, path.clone()))
	}

	/// The objects holding group `sequence` from frame `frame` on, oldest first: the last one
	/// starting at or before it, then each continuing the group. Also the start of the first
	/// object after them, which bounds a gap.
	pub fn holding(&self, sequence: u64, frame: u64) -> (Vec<(Start, Path)>, Option<Start>) {
		let mut holding = Vec::new();
		if let Some((start, (_, path))) = self.objects.range(..=(sequence, frame)).next_back() {
			holding.push((*start, path.clone()));
		}
		let mut after = None;
		for (start, (_, path)) in self.objects.range((sequence, frame.saturating_add(1))..) {
			if start.0 != sequence {
				after = Some(*start);
				break;
			}
			holding.push((*start, path.clone()));
		}
		(holding, after)
	}

	/// The newest object whose first frame presents at or before `pts`: the one holding it,
	/// if any does. `None` when every object starts after it, or on an untimed track.
	pub fn at(&self, pts: Timestamp) -> Option<(Start, Path)> {
		// A stored timestamp is whole, so it is at or before `pts` exactly when it is at or
		// before `pts` rounded down to the track's timescale.
		let limit = u64::try_from(pts.as_scale(self.timescale?)).unwrap_or(u64::MAX);
		let (_, start) = self.times.range(..=(limit, (u64::MAX, u64::MAX))).next_back()?;
		self.objects.get(start).map(|(_, path)| (*start, path.clone()))
	}

	/// The object just before the one starting at `start`.
	pub fn before(&self, start: Start) -> Option<(Start, Path)> {
		let (start, (_, path)) = self.objects.range(..start).next_back()?;
		Some((*start, path.clone()))
	}

	/// The object just after the one starting at `start`, or the oldest for `None`.
	pub fn after(&self, start: Option<Start>) -> Option<(Start, Path)> {
		let (start, (_, path)) = match start {
			Some(start) => self.objects.range((start.0, start.1.saturating_add(1))..).next()?,
			None => self.objects.first_key_value()?,
		};
		Some((*start, path.clone()))
	}

	/// Forget an object the store no longer has, such as one a DVR pruned.
	pub fn forget(&mut self, start: Start) {
		if let Some((Some(pts), _)) = self.objects.remove(&start) {
			self.times.remove(&(pts, start));
		}
	}
}

/// List a track's directory after `cursor`, refusing anything the layout does not name, and
/// read its `end` when `end` is set. `.info` sorts before every object key and `end` after,
/// so listing after `.info` lists everything else. Object stores promise no listing order, so
/// it is sorted here.
pub(super) async fn list<S: ObjectStore>(
	store: &S,
	dir: &Path,
	cursor: &Path,
	timed: bool,
	end: bool,
) -> Result<Listed> {
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
		match Name::parse(filename, timed)? {
			Name::Info => {}
			Name::End => found = Some(meta.location),
			Name::Object(start) => listed
				.objects
				.push(((start.group, start.frame), start.pts, meta.location)),
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
