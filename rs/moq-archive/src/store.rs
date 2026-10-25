use bytes::Bytes;
use futures::StreamExt;
use futures::stream::BoxStream;
use object_store::path::Path;
use object_store::{ObjectMeta, ObjectStore, ObjectStoreExt, PutMode, PutPayload};

use crate::info::Info;
use crate::path::Key;
use crate::segment::Object;
use crate::{Error, Result};

/// Recording object listing types, relative to the recording prefix supplied to [`Store::new`].
pub(crate) mod list {
	use object_store::path::Path;

	use crate::path;
	use crate::{Key, Result};

	/// A recording-relative object listing query.
	#[derive(Debug, Clone, Default, PartialEq, Eq)]
	pub struct Query {
		pub(crate) prefix: Option<Path>,
		pub(crate) offset: Option<Path>,
	}

	impl Query {
		/// List segment objects for one track.
		pub fn segments(track: &str) -> Result<Self> {
			Ok(Self {
				prefix: Some(path::segments_prefix(&Path::ROOT, track)?),
				..Self::default()
			})
		}

		/// Start strictly after this recording object.
		pub fn after(mut self, key: &Key) -> Result<Self> {
			self.offset = Some(key.path(&Path::ROOT)?);
			Ok(self)
		}
	}

	/// One recording object returned by a listing.
	#[derive(Debug, Clone, PartialEq, Eq)]
	pub struct Entry {
		/// Parsed recording key.
		pub key: Key,
		/// Object size in bytes.
		pub size: u64,
	}
}

use list::{Entry, Query};

/// Versioned recording objects on a generic [`ObjectStore`].
#[derive(Clone, Debug)]
pub struct Store<T> {
	inner: T,
	prefix: Path,
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
	pub(crate) fn inner(&self) -> &T {
		&self.inner
	}

	/// The recording prefix.
	pub fn prefix(&self) -> &Path {
		&self.prefix
	}

	/// Encode `key` under this store's prefix.
	pub(crate) fn path(&self, key: &Key) -> Result<Path> {
		key.path(&self.prefix)
	}

	/// Create `.info`, or accept an existing object with the same parsed properties.
	///
	/// An existing recording in an older format is refused with [`Error::Version`]: a reader
	/// still reads it, but nothing appends to it.
	pub(crate) async fn put_info(&self, track: &str, info: &Info) -> Result<Key> {
		let key = Key::info(track)?;
		let path = self.path(&key)?;
		let bytes = info.encode()?;
		match self.create(&path, bytes).await? {
			Create::Created => Ok(key),
			Create::Exists => {
				let existing = self.get_bytes(&path).await?;
				let parsed = Info::decode(&existing)?;
				if parsed.version != info.version {
					return Err(Error::Version(parsed.version));
				}
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
	pub(crate) async fn get_info(&self, track: &str) -> Result<Info> {
		let path = self.path(&Key::info(track)?)?;
		Info::decode(&self.get_bytes(&path).await?)
	}

	/// Create the object at `segments/<segment>`. A collision is accepted only when the bytes match.
	#[cfg(test)]
	pub(crate) async fn put_segments(&self, track: &str, segment: u64, object: &Object) -> Result<Key> {
		let key = Key::segments(track, segment)?;
		self.put_segment(&key, object.encode()?).await?;
		Ok(key)
	}

	/// Fetch and decode the object at `segments/<segment>`, whose frames store timestamps when
	/// `timed`, as the track's `.info` says.
	pub(crate) async fn get_segments(&self, track: &str, segment: u64, timed: bool) -> Result<Object> {
		let path = self.path(&Key::segments(track, segment)?)?;
		Object::decode(self.get_bytes(&path).await?, timed)
	}

	/// Each recorded track's timeline track, found by listing the recording's track directories:
	/// a timeline is named by [`hang::timeline::default_name`]. One listing of the
	/// prefix's directories plus a HEAD per timeline, so following a long recording doesn't list
	/// its objects. A timeline whose `.info` isn't stored yet, such as one still enrolling, is left
	/// out until it is.
	///
	/// For replaying a recording without its catalog; a catalog's `archive` entry names the same map.
	pub async fn timelines(&self) -> Result<std::collections::BTreeMap<String, String>> {
		let listed = self.inner.list_with_delimiter(self.list_path(None).as_ref()).await?;
		let mut timelines = std::collections::BTreeMap::new();
		for directory in listed.common_prefixes {
			let encoded = directory
				.filename()
				.ok_or_else(|| Error::Directory(directory.to_string()))?;
			let track = crate::path::decode_track(encoded)?;
			let Some(indexed) = track.strip_suffix(hang::timeline::SUFFIX) else {
				continue;
			};
			match self.inner.head(&self.path(&Key::info(&track)?)?).await {
				Ok(_) => timelines.insert(indexed.to_string(), track),
				Err(object_store::Error::NotFound { .. }) => continue,
				Err(err) => return Err(err.into()),
			};
		}
		Ok(timelines)
	}

	/// Stream matching entries, in unspecified order.
	pub(crate) fn list(&self, query: &Query) -> BoxStream<'static, Result<Entry>> {
		let prefix = self.list_path(query.prefix.as_ref());
		let offset = query.offset.as_ref().map(|offset| self.recording_path(offset));
		let store_prefix = self.prefix.clone();
		let stream = match offset.as_ref() {
			Some(offset) => self.inner.list_with_offset(prefix.as_ref(), offset),
			None => self.inner.list(prefix.as_ref()),
		};
		stream.map(move |item| Entry::from_meta(&store_prefix, item?)).boxed()
	}

	#[cfg(test)]
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

	fn recording_path(&self, relative: &Path) -> Path {
		let mut path = self.prefix.clone();
		path.extend(relative.parts());
		path
	}

	fn list_path(&self, relative: Option<&Path>) -> Option<Path> {
		let path = relative.map_or_else(|| self.prefix.clone(), |relative| self.recording_path(relative));
		(!path.as_ref().is_empty()).then_some(path)
	}
}

impl Entry {
	fn from_meta(prefix: &Path, meta: ObjectMeta) -> Result<Self> {
		let key = Key::parse(prefix, &meta.location)?;
		Ok(Self { key, size: meta.size })
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
	use object_store::ObjectStoreExt;
	use object_store::memory::InMemory;
	use object_store::path::Path;

	use super::*;
	use crate::ID_MAX;
	use crate::mock::Mock;
	use crate::path::encode_track;
	use crate::segment::{Frame, Group};

	fn memory() -> Store<InMemory> {
		Store::new(InMemory::new(), "rec")
	}

	fn frame(timestamp: u64, payload: &'static [u8]) -> Frame {
		Frame {
			timestamp: Some(timestamp),
			payload: Bytes::from_static(payload),
		}
	}

	fn one_group(sequence: u64, payload: &'static [u8]) -> Object {
		Object::new(vec![Group {
			sequence,
			frames: vec![frame(sequence, payload)],
		}])
	}

	fn two_groups() -> Object {
		Object::new(vec![
			Group {
				sequence: 5,
				frames: vec![frame(0, b"a")],
			},
			Group {
				sequence: 6,
				frames: vec![frame(1, b"bb")],
			},
		])
	}

	async fn names(store: &Store<InMemory>, query: &Query) -> BTreeSet<String> {
		store
			.list(query)
			.map_ok(|entry| entry.key.path(&Path::ROOT).unwrap().to_string())
			.try_collect()
			.await
			.unwrap()
	}

	#[tokio::test]
	async fn info_create_get_and_idempotent_retry() {
		let store = memory();
		let info = Info::new(1, Some(1_000)).unwrap();
		store.put_info("catalog.json", &info).await.unwrap();
		assert_eq!(store.get_info("catalog.json").await.unwrap(), info);

		let path = store.path(&Key::info("catalog.json").unwrap()).unwrap();
		store
			.inner()
			.put(
				&path,
				br#"{ "timescale": 1000, "priority": 1, "version": 3 }"#.as_ref().into(),
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
		assert_eq!(&kept[..], br#"{ "timescale": 1000, "priority": 1, "version": 3 }"#);
	}

	#[tokio::test]
	async fn malformed_or_unsupported_existing_info_is_refused_and_kept() {
		let store = memory();
		let info = Info::new(0, Some(1_000)).unwrap();
		for (track, existing, check) in [
			(
				"v1",
				&br#"{"version":1,"priority":0,"timescale":1000}"#[..],
				(|err| matches!(err, Error::Version(1))) as fn(&Error) -> bool,
			),
			("v2", br#"{"version":2,"priority":128,"timescale":1000}"#, |err| {
				matches!(err, Error::Version(2))
			}),
			("junk", b"not json", |err| matches!(err, Error::Json(_))),
			("zero", br#"{"version":3,"priority":0,"timescale":0}"#, |err| {
				matches!(err, Error::Timescale(0))
			}),
		] {
			let path = store.path(&Key::info(track).unwrap()).unwrap();
			store.inner().put(&path, existing.to_vec().into()).await.unwrap();
			let err = store.put_info(track, &info).await.unwrap_err();
			assert!(check(&err), "{track}: {err}");
			let kept = store.inner().get(&path).await.unwrap().bytes().await.unwrap();
			assert_eq!(&kept[..], existing, "{track} is not rewritten");
		}
	}

	#[tokio::test]
	async fn info_property_mismatch_is_a_hard_error() {
		let store = memory();
		store
			.put_info("video", &Info::new(0, Some(1_000)).unwrap())
			.await
			.unwrap();
		assert!(matches!(
			store.put_info("video", &Info::new(1, Some(1_000)).unwrap()).await,
			Err(Error::Priority {
				existing: 0,
				intended: 1
			})
		));
		assert!(matches!(
			store.put_info("video", &Info::new(0, Some(2_000)).unwrap()).await,
			Err(Error::TimescaleMismatch {
				existing: Some(1000),
				intended: Some(2000)
			})
		));
		assert_eq!(
			store.get_info("video").await.unwrap(),
			Info::new(0, Some(1_000)).unwrap()
		);
	}

	#[tokio::test]
	async fn segments_put_get_and_identical_collision() {
		let store = memory();
		let object = two_groups();
		let key = store.put_segments("video", 3, &object).await.unwrap();
		assert_eq!(key, Key::segments("video", 3).unwrap());
		assert_eq!(
			store.path(&key).unwrap().as_ref(),
			"rec/video/segments/0000000000000000003"
		);
		assert_eq!(store.get_segments("video", 3, true).await.unwrap(), object);
		store.put_segments("video", 3, &object).await.unwrap();

		let other = Object::new(vec![Group {
			sequence: 5,
			frames: vec![frame(0, b"X")],
		}]);
		assert!(matches!(
			store.put_segments("video", 3, &other).await,
			Err(Error::Conflict(_))
		));
	}

	#[tokio::test]
	async fn listing_names_every_object() {
		let store = memory();
		store.put_info("video", &Info::new(0, Some(1)).unwrap()).await.unwrap();
		store.put_segments("video", 1, &one_group(1, b"a")).await.unwrap();
		store.put_segments("video", 3, &one_group(3, b"b")).await.unwrap();
		store.put_segments("timeline.z", 2, &one_group(0, b"t")).await.unwrap();

		let listed: Vec<_> = store.list(&Query::default()).try_collect().await.unwrap();
		let mut keys: Vec<_> = listed.into_iter().map(|listed| listed.key).collect();
		keys.sort_by(|a, b| store.path(a).unwrap().as_ref().cmp(store.path(b).unwrap().as_ref()));
		assert_eq!(
			keys,
			vec![
				Key::segments("timeline.z", 2).unwrap(),
				Key::info("video").unwrap(),
				Key::segments("video", 1).unwrap(),
				Key::segments("video", 3).unwrap(),
			]
		);
	}

	#[tokio::test]
	async fn list_with_offset_finishes_before_the_caller_sorts() -> Result<()> {
		let store = memory();
		for segment in [0u64, 2, 1] {
			store
				.put_segments("timeline.z", segment, &one_group(segment, b"t"))
				.await
				.unwrap();
		}
		let query = Query::segments("timeline.z")?.after(&Key::segments("timeline.z", 0)?)?;
		let mut rest: Vec<_> = store
			.list(&query)
			.map_ok(|entry| match entry.key {
				Key::Segments { segment, .. } => segment,
				_ => panic!("expected a segment key"),
			})
			.try_collect()
			.await?;
		rest.sort();
		assert_eq!(rest, vec![1, 2]);
		Ok(())
	}

	#[tokio::test]
	async fn percent_encoded_track_listing() {
		let store = memory();
		store
			.put_info("catalog.json", &Info::new(0, Some(1)).unwrap())
			.await
			.unwrap();
		let locations = names(&store, &Query::default()).await;
		assert!(locations.contains(&format!("{}/.info", encode_track("catalog.json").unwrap())));
	}

	#[tokio::test]
	async fn empty_prefix_lists_the_whole_store() {
		let store = Store::new(Mock::memory(), "");
		store
			.put_info("catalog.json", &Info::new(0, Some(1)).unwrap())
			.await
			.unwrap();
		store.put_segments("video", 4, &one_group(4, b"a")).await.unwrap();

		let expected =
			std::collections::HashSet::from([Key::info("catalog.json").unwrap(), Key::segments("video", 4).unwrap()]);
		let streamed: std::collections::HashSet<Key> = store
			.list(&Query::default())
			.map_ok(|entry| entry.key)
			.try_collect()
			.await
			.unwrap();
		assert_eq!(streamed, expected);
	}

	#[tokio::test]
	async fn id_endpoints_are_valid_keys() {
		let store = memory();
		let object = one_group(ID_MAX, b"z");
		store.put_segments("t", ID_MAX, &object).await.unwrap();
		assert_eq!(store.get_segments("t", ID_MAX, true).await.unwrap(), object);
	}

	#[tokio::test]
	async fn local_disk_roundtrip() {
		let dir = tempfile::tempdir().unwrap();
		let inner = object_store::local::LocalFileSystem::new_with_prefix(dir.path()).unwrap();
		let store = Store::new(inner, Path::from("rec"));
		let info = Info::new(3, Some(90_000)).unwrap();
		store.put_info("audio", &info).await.unwrap();
		let object = two_groups();
		store.put_segments("audio", 0, &object).await.unwrap();
		assert_eq!(store.get_info("audio").await.unwrap(), info);
		assert_eq!(store.get_segments("audio", 0, true).await.unwrap(), object);
	}
}
