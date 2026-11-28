//! One track's object keys, listed once, extended after its newest key, and listed whole again
//! to drop what the store pruned.

use std::collections::BTreeMap;
use std::sync::Mutex;

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
///
/// Retention deletes objects that a listing after the newest key never sees, so the whole
/// directory is listed again once the objects added since its last whole listing outnumber the
/// ones that listing found, and whenever a read finds one gone. The index then holds at most
/// about twice what the store retains, for one listed key per added object.
pub(super) struct Index {
	pub dir: Path,
	pub timescale: Option<Timescale>,
	/// Each object's key. A group's sequence is its start, so these sort by time too.
	objects: BTreeMap<Start, Path>,
	/// `.info`, which a listing of the whole directory starts after.
	origin: Path,
	/// The newest key the live feed took, which the next listing starts after.
	cursor: Path,
	/// The newest start ever indexed. Only objects after it are added, so one dropped up to it,
	/// because the store pruned it or a read found it gone, never comes back from a stale listing.
	known: Option<Start>,
	/// How many objects the last whole listing left, and how many listings added since.
	listed: usize,
	added: usize,
}

impl Index {
	pub fn new(dir: Path, info: &Info) -> Result<Self> {
		let origin = crate::path::push(&dir, layout::TRACK_INFO)?;
		Ok(Self {
			dir,
			timescale: super::track::timescale(info)?,
			objects: BTreeMap::new(),
			cursor: origin.clone(),
			origin,
			known: None,
			listed: 0,
			added: 0,
		})
	}

	/// How many objects it holds.
	#[cfg(test)]
	pub fn len(&self) -> usize {
		self.objects.len()
	}

	pub fn timed(&self) -> bool {
		self.timescale.is_some()
	}

	/// Add the objects a listing found that are newer than any indexed before.
	fn add(&mut self, objects: &[Listing]) {
		for (start, path) in objects {
			if Some(*start) > self.known {
				self.objects.insert(*start, path.clone());
				self.known = Some(*start);
				self.added += 1;
			}
		}
	}

	/// The newest start ever indexed, when the next refresh should list the whole directory,
	/// which [`Self::reconcile`] then reconciles up to.
	fn due(&self) -> Option<Option<Start>> {
		(self.added > self.listed).then_some(self.known)
	}

	/// Take in a listing of the whole directory, `found`: drop the objects up to `through`, all
	/// indexed before it began, that it no longer holds, since the store pruned them, and add
	/// the newer ones. The cursor stays, so the live feed still takes the new ones.
	fn reconcile(&mut self, through: Option<Start>, found: &[Listing]) {
		self.objects.retain(|start, _| {
			Some(*start) > through || found.binary_search_by_key(start, |(start, _)| *start).is_ok()
		});
		self.add(found);
		self.listed = self.objects.len();
		self.added = 0;
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
}

/// Take in the objects new since `index`'s cursor and return them, with the track's `end` when
/// `end` is set. When the index is due, the listing covers the whole directory, dropping what
/// the store pruned.
pub(super) async fn refresh<S: ObjectStore>(store: &S, index: &Mutex<Index>, end: bool) -> Result<Listed> {
	let (dir, from, cursor, through) = {
		let index = index.lock().unwrap();
		let through = index.due();
		let from = match through {
			Some(_) => index.origin.clone(),
			None => index.cursor.clone(),
		};
		(index.dir.clone(), from, index.cursor.clone(), through)
	};
	let mut listed = list(store, &dir, &from, end).await?;
	let mut index = index.lock().unwrap();
	if let Some(through) = through {
		index.reconcile(through, &listed.objects);
		listed.objects.retain(|(_, path)| *path > cursor);
	}
	index.add(&listed.objects);
	if let Some((_, path)) = listed.objects.last() {
		index.cursor = path.clone();
	}
	// The first listing starts at the origin too.
	if from == index.origin {
		index.listed = index.objects.len();
		index.added = 0;
	}
	Ok(listed)
}

/// Drop the object starting at `missing`, which a read found gone, and with it every other
/// object the store pruned, by listing the whole directory. Lists nothing when another read
/// dropped it already. A listing that still holds it never adds it back.
pub(super) async fn pruned<S: ObjectStore>(store: &S, index: &Mutex<Index>, missing: Start) -> Result<()> {
	let (dir, from, through) = {
		let mut index = index.lock().unwrap();
		if index.objects.remove(&missing).is_none() {
			return Ok(());
		}
		(index.dir.clone(), index.origin.clone(), index.known)
	};
	let listed = list(store, &dir, &from, false).await?;
	index.lock().unwrap().reconcile(through, &listed.objects);
	Ok(())
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
