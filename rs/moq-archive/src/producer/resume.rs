//! What a prefix already holds, so a restarted producer continues each track after its last
//! stored frame.

use std::collections::HashMap;

use futures::TryStreamExt;
use moq_net::track::Position;
use object_store::path::Path;
use object_store::{ObjectStore, ObjectStoreExt};

use crate::layout::{self, End, Name, Object, Start};
use crate::{Error, Info, Result, Store, VERSION};

/// One track as stored.
pub(super) struct Stored {
	pub info: Info,
	/// Every object's key, oldest first.
	pub objects: Vec<Path>,
	/// Where the track continues: just past its newest object's last frame.
	pub next: Option<Position>,
	/// How its last run ended, when the producer that stored it saw that.
	pub end: Option<End>,
}

/// Every stored track by name, or `None` when the broadcast's `.end` says it is complete.
pub(super) async fn load<S: ObjectStore>(store: &Store<S>) -> Result<Option<HashMap<String, Stored>>> {
	let prefix = store.prefix();
	let scope = (!prefix.as_ref().is_empty()).then_some(prefix);
	let listed = store.inner().list_with_delimiter(scope).await?;

	if let Some(object) = listed.objects.first() {
		match object.location.filename() {
			Some(layout::BROADCAST_END) => return Ok(None),
			_ => return Err(Error::Path(object.location.to_string())),
		}
	}

	let mut tracks = HashMap::new();
	for dir in listed.common_prefixes {
		let segment = dir.filename().ok_or_else(|| Error::Path(dir.to_string()))?;
		let name = layout::parse_track(segment)?;
		let track = load_track(store, &dir).await?;
		tracks.insert(name, track);
	}
	Ok(Some(tracks))
}

async fn load_track<S: ObjectStore>(store: &Store<S>, dir: &Path) -> Result<Stored> {
	let entries: Vec<_> = store.inner().list(Some(dir)).try_collect().await?;
	let info_path = crate::path::push(dir, layout::TRACK_INFO)?;
	let info = Info::decode(&store.inner().get(&info_path).await?.bytes().await?)?;
	// A reader still reads an older format, but nothing appends to it.
	if info.version != VERSION {
		return Err(Error::Version(info.version));
	}
	let timed = info.timescale.is_some();

	let mut objects: Vec<(Start, Path)> = Vec::new();
	let mut end = None;
	for entry in entries {
		// A flat listing only returns objects directly under the track; anything deeper is foreign.
		if entry.location.prefix_match(dir).is_none_or(|parts| parts.count() != 1) {
			return Err(Error::Path(entry.location.to_string()));
		}
		let filename = entry
			.location
			.filename()
			.ok_or_else(|| Error::Path(entry.location.to_string()))?;
		match Name::parse(filename, timed)? {
			Name::Info => {}
			Name::End => end = Some(End::decode(&store.inner().get(&entry.location).await?.bytes().await?)?),
			Name::Object(start) => objects.push((start, entry.location)),
		}
	}
	objects.sort();

	let next = match objects.last() {
		Some((start, path)) => {
			let object = Object::decode(store.inner().get(path).await?.bytes().await?, timed)?;
			if object.start()? != *start {
				return Err(Error::Path(path.to_string()));
			}
			Some(object.end()?)
		}
		None => None,
	};

	Ok(Stored {
		info,
		objects: objects.into_iter().map(|(_, path)| path).collect(),
		next,
		end,
	})
}
