use bytes::Bytes;
use futures::StreamExt;
use futures::stream::BoxStream;
use object_store::list::{PaginatedListOptions, PaginatedListResult, PaginatedListStore};
use object_store::path::Path;
use object_store::{ObjectMeta, ObjectStore, ObjectStoreExt, PutMode, PutPayload};

use crate::info::Info;
use crate::path::{self, Key};
use crate::segment::Object;
use crate::{Error, Result};

/// Versioned recording objects on a generic [`ObjectStore`].
#[derive(Clone, Debug)]
pub struct Store<T> {
	inner: T,
	prefix: Path,
}

/// How to enumerate objects. Collect the stream before sorting or advancing a cursor.
#[derive(Debug, Clone, Default)]
pub struct List<'a> {
	/// Restrict to this prefix (already under the store prefix). `None` lists the whole recording.
	pub prefix: Option<&'a Path>,
	/// Exclusive start location. `None` starts from the beginning.
	pub offset: Option<&'a Path>,
}

/// An object name from a listing, without its payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
	/// Parsed recording key.
	pub key: Key,
	/// Exact store location, including the recording prefix.
	pub location: Path,
	/// Object size in bytes.
	pub size: u64,
}

/// One page from a [`PaginatedListStore`] backend.
#[derive(Debug, Clone)]
pub struct Page {
	/// Objects in this page, in the backend's order.
	pub objects: Vec<Listed>,
	/// Continuation token for the next page, if any.
	pub page_token: Option<String>,
}

impl<T: ObjectStore> Store<T> {
	/// Wrap `inner` with an application-defined recording prefix.
	pub fn new(inner: T, prefix: impl Into<Path>) -> Self {
		Self {
			inner,
			prefix: prefix.into(),
		}
	}

	/// The underlying object store.
	pub fn inner(&self) -> &T {
		&self.inner
	}

	/// The recording prefix.
	pub fn prefix(&self) -> &Path {
		&self.prefix
	}

	/// Encode `key` under this store's prefix.
	pub fn path(&self, key: &Key) -> Result<Path> {
		key.path(&self.prefix)
	}

	/// `<prefix>/<encoded-track>/groups`
	pub fn groups_prefix(&self, track: &str) -> Result<Path> {
		path::groups_prefix(&self.prefix, track)
	}

	/// `<prefix>/<encoded-track>/segments`
	pub fn segments_prefix(&self, track: &str) -> Result<Path> {
		path::segments_prefix(&self.prefix, track)
	}

	/// Exclusive listing offset `groups/<group>` for a FETCH of `group`.
	pub fn groups_offset(&self, track: &str, group: u64) -> Result<Path> {
		path::groups_offset(&self.prefix, track, group)
	}

	/// Create `.info`, or accept an existing object with the same parsed properties.
	pub async fn put_info(&self, track: &str, info: &Info) -> Result<Key> {
		let key = Key::info(track)?;
		let path = self.path(&key)?;
		let bytes = info.encode()?;
		match self.create(&path, bytes).await? {
			Create::Created => Ok(key),
			Create::Exists => {
				let existing = self.get_bytes(&path).await?;
				let parsed = Info::decode(&existing)?;
				if parsed.priority != info.priority {
					return Err(Error::Priority {
						existing: parsed.priority,
						intended: info.priority,
					});
				}
				if parsed.timescale != info.timescale {
					return Err(Error::TimescaleMismatch {
						existing: parsed.timescale,
						intended: info.timescale,
					});
				}
				Ok(key)
			}
		}
	}

	/// Fetch and validate a track's `.info`.
	pub async fn get_info(&self, track: &str) -> Result<Info> {
		let path = self.path(&Key::info(track)?)?;
		Info::decode(&self.get_bytes(&path).await?)
	}

	/// Create a range-named groups object. A collision is accepted only when the bytes match.
	pub async fn put_groups(&self, track: &str, object: &Object) -> Result<Key> {
		let (smallest, largest) = object.bounds()?;
		let key = Key::groups(track, largest, smallest)?;
		self.put_segment(&key, object.encode()?).await?;
		Ok(key)
	}

	/// Fetch a groups object and require its table to match the filename bounds.
	pub async fn get_groups(&self, track: &str, largest: u64, smallest: u64) -> Result<Object> {
		let path = self.path(&Key::groups(track, largest, smallest)?)?;
		Object::decode_groups(self.get_bytes(&path).await?, largest, smallest)
	}

	/// Create a timeline object at `segments/<segment>`. A collision is accepted only when the bytes match.
	pub async fn put_segments(&self, track: &str, segment: u64, object: &Object) -> Result<Key> {
		let key = Key::segments(track, segment)?;
		self.put_segment(&key, object.encode()?).await?;
		Ok(key)
	}

	/// Fetch and validate a timeline object.
	pub async fn get_segments(&self, track: &str, segment: u64) -> Result<Object> {
		let path = self.path(&Key::segments(track, segment)?)?;
		Object::decode(self.get_bytes(&path).await?)
	}

	/// Delete the object at `key`.
	pub async fn delete(&self, key: &Key) -> Result<()> {
		let path = self.path(key)?;
		self.inner.delete(&path).await?;
		Ok(())
	}

	/// List object names. The order is not guaranteed and listing may traverse the filesystem.
	pub fn list(&self, spec: List<'_>) -> BoxStream<'static, Result<Listed>> {
		let prefix = match self.list_prefix(spec.prefix) {
			Ok(prefix) => prefix,
			Err(err) => return futures::stream::once(async move { Err(err) }).boxed(),
		};
		let store_prefix = self.prefix.clone();
		let stream = match spec.offset {
			Some(offset) => self.inner.list_with_offset(prefix.as_ref(), offset),
			None => self.inner.list(prefix.as_ref()),
		};
		stream.map(move |item| Listed::from_meta(&store_prefix, item?)).boxed()
	}

	async fn put_segment(&self, key: &Key, bytes: Bytes) -> Result<()> {
		let path = self.path(key)?;
		match self.create(&path, bytes.clone()).await? {
			Create::Created => Ok(()),
			Create::Exists => {
				let existing = self.get_bytes(&path).await?;
				if existing == bytes {
					Ok(())
				} else {
					Err(Error::Conflict(path.to_string()))
				}
			}
		}
	}

	async fn create(&self, path: &Path, bytes: Bytes) -> Result<Create> {
		match self
			.inner
			.put_opts(path, PutPayload::from(bytes), PutMode::Create.into())
			.await
		{
			Ok(_) => Ok(Create::Created),
			Err(object_store::Error::AlreadyExists { .. }) => Ok(Create::Exists),
			Err(err) => Err(err.into()),
		}
	}

	async fn get_bytes(&self, path: &Path) -> Result<Bytes> {
		Ok(self.inner.get(path).await?.bytes().await?)
	}

	fn list_prefix(&self, prefix: Option<&Path>) -> Result<Option<Path>> {
		match prefix {
			Some(prefix) if prefix == &self.prefix || prefix.prefix_matches(&self.prefix) => Ok(Some(prefix.clone())),
			Some(prefix) => Err(Error::Path(prefix.to_string())),
			None if self.prefix.as_ref().is_empty() => Ok(None),
			None => Ok(Some(self.prefix.clone())),
		}
	}
}

impl<T: ObjectStore + PaginatedListStore> Store<T> {
	/// Seek with `offset` and `max_keys` on a backend that has verified lexical listing.
	pub async fn list_paginated(&self, prefix: Option<&str>, opts: PaginatedListOptions) -> Result<Page> {
		let joined = match prefix {
			None => self.prefix.as_ref().to_string(),
			Some(prefix) if self.prefix.as_ref().is_empty() => prefix.to_string(),
			Some(prefix) => format!("{}/{prefix}", self.prefix.as_ref()),
		};
		let prefix = (!joined.is_empty()).then_some(joined.as_str());
		let PaginatedListResult { result, page_token } = self.inner.list_paginated(prefix, opts).await?;
		let objects = result
			.objects
			.into_iter()
			.map(|meta| Listed::from_meta(&self.prefix, meta))
			.collect::<Result<Vec<_>>>()?;
		Ok(Page { objects, page_token })
	}
}

impl Listed {
	fn from_meta(prefix: &Path, meta: ObjectMeta) -> Result<Self> {
		let key = Key::parse(prefix, &meta.location)?;
		Ok(Self {
			key,
			location: meta.location,
			size: meta.size,
		})
	}
}

enum Create {
	Created,
	Exists,
}

#[cfg(test)]
mod tests {
	use std::collections::BTreeSet;

	use futures::TryStreamExt;
	use object_store::memory::InMemory;
	use object_store::path::Path;

	use super::*;
	use crate::segment::{Frame, Group};
	use crate::{ID_MAX, encode_track};

	fn memory() -> Store<InMemory> {
		Store::new(InMemory::new(), "rec")
	}

	fn frame(timestamp: u64, payload: &'static [u8]) -> Frame {
		Frame {
			timestamp,
			payload: Bytes::from_static(payload),
		}
	}

	fn one_group(sequence: u64, payload: &'static [u8]) -> Object {
		Object {
			groups: vec![Group {
				sequence,
				frames: vec![frame(sequence, payload)],
			}],
		}
	}

	fn two_groups() -> Object {
		Object {
			groups: vec![
				Group {
					sequence: 5,
					frames: vec![frame(0, b"a")],
				},
				Group {
					sequence: 7,
					frames: vec![frame(1, b"bb")],
				},
			],
		}
	}

	async fn names(store: &Store<InMemory>, spec: List<'_>) -> BTreeSet<String> {
		store
			.list(spec)
			.map_ok(|listed| listed.location.to_string())
			.try_collect()
			.await
			.unwrap()
	}

	#[tokio::test]
	async fn info_create_get_and_idempotent_retry() {
		let store = memory();
		let info = Info::new(1, 1_000).unwrap();
		store.put_info("catalog.json", &info).await.unwrap();
		assert_eq!(store.get_info("catalog.json").await.unwrap(), info);

		let path = store.path(&Key::info("catalog.json").unwrap()).unwrap();
		store
			.inner()
			.put(
				&path,
				br#"{ "timescale": 1000, "priority": 1, "version": 1 }"#.as_ref().into(),
			)
			.await
			.unwrap();
		store.put_info("catalog.json", &info).await.unwrap();
		let kept = ObjectStoreExt::get(store.inner(), &path)
			.await
			.unwrap()
			.bytes()
			.await
			.unwrap();
		assert_eq!(&kept[..], br#"{ "timescale": 1000, "priority": 1, "version": 1 }"#);
	}

	#[tokio::test]
	async fn info_property_mismatch_is_a_hard_error() {
		let store = memory();
		store.put_info("video", &Info::new(0, 1_000).unwrap()).await.unwrap();
		assert!(matches!(
			store.put_info("video", &Info::new(1, 1_000).unwrap()).await,
			Err(Error::Priority {
				existing: 0,
				intended: 1
			})
		));
		assert!(matches!(
			store.put_info("video", &Info::new(0, 2_000).unwrap()).await,
			Err(Error::TimescaleMismatch {
				existing: 1000,
				intended: 2000
			})
		));
		assert_eq!(store.get_info("video").await.unwrap(), Info::new(0, 1_000).unwrap());
	}

	#[tokio::test]
	async fn groups_put_get_and_identical_collision() {
		let store = memory();
		let object = two_groups();
		let key = store.put_groups("video", &object).await.unwrap();
		assert_eq!(key, Key::groups("video", 7, 5).unwrap());
		assert_eq!(
			store.path(&key).unwrap().as_ref(),
			"rec/video/groups/0000000000000000007.0000000000000000005"
		);
		assert_eq!(store.get_groups("video", 7, 5).await.unwrap(), object);
		store.put_groups("video", &object).await.unwrap();
	}

	#[tokio::test]
	async fn groups_collision_with_different_bytes_fails() {
		let store = memory();
		store.put_groups("video", &two_groups()).await.unwrap();
		let other = Object {
			groups: vec![
				Group {
					sequence: 5,
					frames: vec![frame(0, b"X")],
				},
				Group {
					sequence: 7,
					frames: vec![frame(1, b"bb")],
				},
			],
		};
		assert!(matches!(
			store.put_groups("video", &other).await,
			Err(Error::Conflict(_))
		));
	}

	#[tokio::test]
	async fn segments_put_get_and_delete() {
		let store = memory();
		let object = one_group(0, b"tl");
		store.put_segments("timeline.z", 0, &object).await.unwrap();
		assert_eq!(store.get_segments("timeline.z", 0).await.unwrap(), object);
		store.delete(&Key::segments("timeline.z", 0).unwrap()).await.unwrap();
		assert!(matches!(
			store.get_segments("timeline.z", 0).await,
			Err(Error::NotFound(_))
		));
	}

	#[tokio::test]
	async fn listing_names_build_a_range_index() {
		let store = memory();
		store.put_info("video", &Info::new(0, 1).unwrap()).await.unwrap();
		store.put_groups("video", &one_group(1, b"a")).await.unwrap();
		store.put_groups("video", &one_group(3, b"b")).await.unwrap();
		store.put_segments("timeline.z", 2, &one_group(0, b"t")).await.unwrap();

		let listed: Vec<_> = store.list(List::default()).try_collect().await.unwrap();
		let mut keys: Vec<_> = listed.into_iter().map(|listed| listed.key).collect();
		keys.sort_by(|a, b| store.path(a).unwrap().as_ref().cmp(store.path(b).unwrap().as_ref()));
		assert_eq!(
			keys,
			vec![
				Key::segments("timeline.z", 2).unwrap(),
				Key::info("video").unwrap(),
				Key::groups("video", 1, 1).unwrap(),
				Key::groups("video", 3, 3).unwrap(),
			]
		);
	}

	#[tokio::test]
	async fn list_with_offset_finishes_before_the_caller_sorts() {
		let store = memory();
		for segment in [0u64, 2, 1] {
			store
				.put_segments("timeline.z", segment, &one_group(segment, b"t"))
				.await
				.unwrap();
		}
		let offset = store.path(&Key::segments("timeline.z", 0).unwrap()).unwrap();
		let prefix = store.segments_prefix("timeline.z").unwrap();
		let mut rest: Vec<_> = store
			.list(List {
				prefix: Some(&prefix),
				offset: Some(&offset),
			})
			.map_ok(|listed| match listed.key {
				Key::Segments { segment, .. } => segment,
				_ => panic!("expected a segment key"),
			})
			.try_collect()
			.await
			.unwrap();
		rest.sort();
		assert_eq!(rest, vec![1, 2]);
	}

	#[tokio::test]
	async fn percent_encoded_track_listing() {
		let store = memory();
		store.put_info("catalog.json", &Info::new(0, 1).unwrap()).await.unwrap();
		let locations = names(&store, List::default()).await;
		assert!(locations.contains(&format!("rec/{}/.info", encode_track("catalog.json").unwrap())));
	}

	#[tokio::test]
	async fn id_endpoints_are_valid_keys() {
		let store = memory();
		let object = one_group(ID_MAX, b"z");
		store.put_groups("v", &object).await.unwrap();
		store.put_segments("t", ID_MAX, &object).await.unwrap();
		assert_eq!(store.get_groups("v", ID_MAX, ID_MAX).await.unwrap(), object);
		assert_eq!(store.get_segments("t", ID_MAX).await.unwrap(), object);
	}

	#[tokio::test]
	async fn local_disk_roundtrip() {
		let dir = tempfile::tempdir().unwrap();
		let inner = object_store::local::LocalFileSystem::new_with_prefix(dir.path()).unwrap();
		let store = Store::new(inner, Path::from("rec"));
		let info = Info::new(3, 90_000).unwrap();
		store.put_info("audio", &info).await.unwrap();
		let object = two_groups();
		store.put_groups("audio", &object).await.unwrap();
		assert_eq!(store.get_info("audio").await.unwrap(), info);
		assert_eq!(store.get_groups("audio", 7, 5).await.unwrap(), object);
	}
}
