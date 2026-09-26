use std::num::NonZeroUsize;

use bytes::Bytes;
use futures::{StreamExt, TryStreamExt};
use futures::stream::BoxStream;
use object_store::list::{PaginatedListOptions, PaginatedListResult, PaginatedListStore};
use object_store::path::Path;
use object_store::{ListResult, ObjectMeta, ObjectStore, ObjectStoreExt, PutMode, PutPayload};

use crate::info::Info;
use crate::path::Key;
use crate::segment::Object;
use crate::{Error, Result};

/// Recording object listing types.
///
/// Listing scopes and offsets are relative to the recording prefix supplied to
/// [`super::Store::new`]. The same [`Query`] can drive streaming and paginated
/// listing. A page's [`Page::next`] query retains the opaque backend token and
/// must be passed to [`super::Store::list_paginated`], not [`super::Store::list`].
///
/// ```
/// use std::num::NonZeroUsize;
///
/// use moq_archive::store::list::Query;
///
/// let query = Query::segments("video.timeline.z")?
///     .page_size(NonZeroUsize::new(100).unwrap());
/// # Ok::<(), moq_archive::Error>(())
/// ```
pub mod list {
	use std::num::NonZeroUsize;

	use object_store::path::Path;

	use crate::path;
	use crate::{Key, Result};

	/// A recording-relative object listing query.
	#[derive(Debug, Clone, Default, PartialEq, Eq)]
	pub struct Query {
		pub(crate) prefix: Option<Path>,
		pub(crate) offset: Option<Path>,
		pub(crate) max_keys: Option<NonZeroUsize>,
		pub(crate) page_token: Option<String>,
	}

	impl Query {
		/// List every object in the recording.
		pub fn new() -> Self {
			Self::default()
		}

		fn prefix(mut self, prefix: impl Into<Path>) -> Self {
			self.prefix = Some(prefix.into());
			self.page_token = None;
			self
		}

		fn offset(mut self, offset: impl Into<Path>) -> Self {
			self.offset = Some(offset.into());
			self.page_token = None;
			self
		}

		/// Request at most this many entries per page.
		pub fn page_size(mut self, max_keys: NonZeroUsize) -> Self {
			self.max_keys = Some(max_keys);
			self.page_token = None;
			self
		}

		/// List every object for one track.
		pub fn track(track: &str) -> Result<Self> {
			Ok(Self::new().prefix(path::track_prefix(track)?))
		}

		/// List segment objects for one track.
		pub fn segments(track: &str) -> Result<Self> {
			Ok(Self::new().prefix(path::segments_prefix(&Path::ROOT, track)?))
		}

		/// Start strictly after this recording object.
		pub fn after(self, key: &Key) -> Result<Self> {
			Ok(self.offset(key.path(&Path::ROOT)?))
		}

		pub(crate) fn next(&self, page_token: String) -> Self {
			let mut next = self.clone();
			next.page_token = Some(page_token);
			next
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

	/// One page of recording objects.
	#[derive(Debug, Clone)]
	pub struct Page {
		/// Entries in the backend's order.
		pub entries: Vec<Entry>,
		/// The same query advanced to the next page, if any.
		/// Pass it to `Store::list_paginated`; `Store::list` rejects it.
		pub next: Option<Query>,
	}
}

use list::{Entry, Page, Query};

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

	/// Create the object at `segments/<segment>`. A collision is accepted only when the bytes match.
	pub async fn put_segments(&self, track: &str, segment: u64, object: &Object) -> Result<Key> {
		let key = Key::segments(track, segment)?;
		self.put_segment(&key, object.encode()?).await?;
		Ok(key)
	}

	/// Fetch and decode the object at `segments/<segment>`.
	pub async fn get_segments(&self, track: &str, segment: u64) -> Result<Object> {
		let path = self.path(&Key::segments(track, segment)?)?;
		Object::decode(self.get_bytes(&path).await?)
	}

	/// Each recorded track's timeline track, found by listing the `.info` objects a [`Writer`]
	/// creates: a timeline is named by [`hang::timeline::default_name`].
	///
	/// For replaying a recording without its catalog; a catalog's `archive` entry names the same map.
	///
	/// [`Writer`]: crate::Writer
	pub async fn timelines(&self) -> Result<std::collections::BTreeMap<String, String>> {
		let entries: Vec<Entry> = self.list(&Query::new()).try_collect().await?;
		Ok(entries
			.into_iter()
			.filter_map(|entry| match entry.key {
				Key::Info { track } => {
					let indexed = track.strip_suffix(hang::timeline::SUFFIX)?.to_string();
					Some((indexed, track))
				}
				_ => None,
			})
			.collect())
	}

	/// Delete the object at `key`.
	pub async fn delete(&self, key: &Key) -> Result<()> {
		let path = self.path(key)?;
		self.inner.delete(&path).await?;
		Ok(())
	}

	/// Stream matching entries. Order is unspecified and `max_keys` is ignored.
	/// Continuation queries from `Page::next` fail; pass them to `list_paginated`.
	pub fn list(&self, query: &Query) -> BoxStream<'static, Result<Entry>> {
		if query.page_token.is_some() {
			return futures::stream::once(async { Err(Error::Pagination) }).boxed();
		}
		let prefix = self.list_path(query.prefix.as_ref());
		let offset = query.offset.as_ref().map(|offset| self.recording_path(offset));
		let store_prefix = self.prefix.clone();
		let stream = match offset.as_ref() {
			Some(offset) => self.inner.list_with_offset(prefix.as_ref(), offset),
			None => self.inner.list(prefix.as_ref()),
		};
		stream.map(move |item| Entry::from_meta(&store_prefix, item?)).boxed()
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

	fn recording_path(&self, relative: &Path) -> Path {
		let mut path = self.prefix.clone();
		path.extend(relative.parts());
		path
	}

	fn list_path(&self, relative: Option<&Path>) -> Option<Path> {
		let path = relative.map_or_else(|| self.prefix.clone(), |relative| self.recording_path(relative));
		(!path.as_ref().is_empty()).then_some(path)
	}

	fn paginated_prefix(&self, relative: Option<&Path>) -> Option<String> {
		self.list_path(relative).map(|path| format!("{path}/"))
	}

	fn paginated_options(&self, query: &Query) -> PaginatedListOptions {
		PaginatedListOptions {
			offset: query
				.offset
				.as_ref()
				.map(|offset| self.recording_path(offset).to_string()),
			max_keys: query.max_keys.map(NonZeroUsize::get),
			page_token: query.page_token.clone(),
			..Default::default()
		}
	}

	fn page(&self, query: &Query, result: PaginatedListResult) -> Result<Page> {
		let PaginatedListResult { result, page_token } = result;
		let entries = self.entries(result)?;
		let next = page_token.map(|token| query.next(token));
		Ok(Page { entries, next })
	}

	fn entries(&self, result: ListResult) -> Result<Vec<Entry>> {
		if let Some(prefix) = result.common_prefixes.first() {
			return Err(Error::Directory(prefix.to_string()));
		}
		result
			.objects
			.into_iter()
			.map(|meta| Entry::from_meta(&self.prefix, meta))
			.collect()
	}
}

impl<T: ObjectStore + PaginatedListStore> Store<T> {
	/// List one page while keeping the backend continuation token inside the returned query.
	pub async fn list_paginated(&self, query: &Query) -> Result<Page> {
		let prefix = self.paginated_prefix(query.prefix.as_ref());
		let opts = self.paginated_options(query);
		let result = self.inner.list_paginated(prefix.as_deref(), opts).await?;
		self.page(query, result)
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
	use std::num::NonZeroUsize;

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
			timestamp,
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
		let info = Info::new(1, 1_000).unwrap();
		store.put_info("catalog.json", &info).await.unwrap();
		assert_eq!(store.get_info("catalog.json").await.unwrap(), info);

		let path = store.path(&Key::info("catalog.json").unwrap()).unwrap();
		store
			.inner()
			.put(
				&path,
				br#"{ "timescale": 1000, "priority": 1, "version": 2 }"#.as_ref().into(),
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
		assert_eq!(&kept[..], br#"{ "timescale": 1000, "priority": 1, "version": 2 }"#);
	}

	#[tokio::test]
	async fn malformed_or_unsupported_existing_info_is_refused_and_kept() {
		let store = memory();
		let info = Info::new(0, 1_000).unwrap();
		for (track, existing, check) in [
			(
				"v1",
				&br#"{"version":1,"priority":0,"timescale":1000}"#[..],
				(|err| matches!(err, Error::Version(1))) as fn(&Error) -> bool,
			),
			("junk", b"not json", |err| matches!(err, Error::Json(_))),
			("zero", br#"{"version":2,"priority":0,"timescale":0}"#, |err| {
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
	async fn segments_put_get_and_identical_collision() {
		let store = memory();
		let object = two_groups();
		let key = store.put_segments("video", 3, &object).await.unwrap();
		assert_eq!(key, Key::segments("video", 3).unwrap());
		assert_eq!(
			store.path(&key).unwrap().as_ref(),
			"rec/video/segments/0000000000000000003"
		);
		assert_eq!(store.get_segments("video", 3).await.unwrap(), object);
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
	async fn listing_names_every_object() {
		let store = memory();
		store.put_info("video", &Info::new(0, 1).unwrap()).await.unwrap();
		store.put_segments("video", 1, &one_group(1, b"a")).await.unwrap();
		store.put_segments("video", 3, &one_group(3, b"b")).await.unwrap();
		store.put_segments("timeline.z", 2, &one_group(0, b"t")).await.unwrap();

		let listed: Vec<_> = store.list(&Query::new()).try_collect().await.unwrap();
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
		store.put_info("catalog.json", &Info::new(0, 1).unwrap()).await.unwrap();
		let locations = names(&store, &Query::new()).await;
		assert!(locations.contains(&format!("{}/.info", encode_track("catalog.json").unwrap())));
	}

	#[tokio::test]
	async fn listing_query_is_recording_relative() {
		let store = memory();
		store.put_info("video", &Info::new(0, 1).unwrap()).await.unwrap();
		store.put_segments("video", 1, &one_group(1, b"a")).await.unwrap();
		store.put_info("video-alt", &Info::new(0, 1).unwrap()).await.unwrap();

		let query = Query::track("video").unwrap();
		let paths = names(&store, &query).await;
		assert_eq!(
			paths,
			BTreeSet::from([
				"video/.info".to_string(),
				"video/segments/0000000000000000001".to_string(),
			])
		);
		assert_eq!(
			store.paginated_prefix(query.prefix.as_ref()).as_deref(),
			Some("rec/video/")
		);
	}

	#[tokio::test]
	async fn paginated_recording_prefix_excludes_siblings() {
		let store = memory();
		store.put_info("video", &Info::new(0, 1).unwrap()).await.unwrap();
		store
			.inner()
			.put(
				&Path::from("rec-other/video/.info"),
				Bytes::from_static(b"sibling").into(),
			)
			.await
			.unwrap();

		let prefix = store.paginated_prefix(None).unwrap();
		assert_eq!(prefix, "rec/");
		let objects = store
			.inner()
			.list(None)
			.try_filter(|meta| futures::future::ready(meta.location.as_ref().starts_with(&prefix)))
			.try_collect()
			.await
			.unwrap();
		let entries = store
			.entries(ListResult {
				objects,
				common_prefixes: Vec::new(),
				extensions: Default::default(),
			})
			.unwrap();
		assert_eq!(entries.len(), 1);
		assert_eq!(entries[0].key, Key::info("video").unwrap());
	}

	#[tokio::test]
	async fn paginated_query_preserves_every_supported_option() {
		let store = memory();
		let query = Query::segments("timeline.z")
			.unwrap()
			.after(&Key::segments("timeline.z", 1).unwrap())
			.unwrap()
			.page_size(NonZeroUsize::new(2).unwrap());
		let options = store.paginated_options(&query);
		assert_eq!(
			store.paginated_prefix(query.prefix.as_ref()).as_deref(),
			Some("rec/timeline%2Ez/segments/")
		);
		assert_eq!(
			options.offset.as_deref(),
			Some("rec/timeline%2Ez/segments/0000000000000000001")
		);
		assert_eq!(options.max_keys, Some(2));
		assert_eq!(options.page_token, None);

		let page = store
			.page(
				&query,
				PaginatedListResult {
					result: ListResult {
						objects: Vec::new(),
						common_prefixes: Vec::new(),
						extensions: Default::default(),
					},
					page_token: Some("opaque".to_string()),
				},
			)
			.unwrap();
		let next = page.next.unwrap();
		assert_eq!(next.prefix, query.prefix);
		assert_eq!(next.offset, query.offset);
		assert_eq!(next.max_keys, query.max_keys);
		assert_eq!(store.paginated_options(&next).page_token.as_deref(), Some("opaque"));

		let changed = next.page_size(NonZeroUsize::new(3).unwrap());
		assert_eq!(store.paginated_options(&changed).page_token, None);
	}

	#[tokio::test]
	async fn paginated_pages_match_streaming_results() {
		let store = memory();
		for segment in 0..3 {
			store
				.put_segments("timeline.z", segment, &one_group(segment, b"t"))
				.await
				.unwrap();
		}
		let query = Query::segments("timeline.z")
			.unwrap()
			.after(&Key::segments("timeline.z", 0).unwrap())
			.unwrap()
			.page_size(NonZeroUsize::new(1).unwrap());
		let streamed = names(&store, &query).await;
		let mut metas: Vec<_> = store
			.inner()
			.list(store.list_path(query.prefix.as_ref()).as_ref())
			.try_collect()
			.await
			.unwrap();
		metas.sort_by(|a, b| a.location.cmp(&b.location));
		metas.retain(|meta| meta.location > store.recording_path(query.offset.as_ref().unwrap()));

		let mut paged = BTreeSet::new();
		let mut current = query;
		for (index, meta) in metas.iter().cloned().enumerate() {
			let page = store
				.page(
					&current,
					PaginatedListResult {
						result: ListResult {
							objects: vec![meta],
							common_prefixes: Vec::new(),
							extensions: Default::default(),
						},
						page_token: (index + 1 < metas.len()).then(|| format!("page-{index}")),
					},
				)
				.unwrap();
			paged.extend(
				page.entries
					.into_iter()
					.map(|entry| entry.key.path(&Path::ROOT).unwrap().to_string()),
			);
			if let Some(next) = page.next {
				current = next;
			}
		}
		assert_eq!(paged, streamed);
	}

	#[tokio::test]
	async fn streaming_rejects_continuation_queries() {
		let store = memory();
		store.put_segments("timeline.z", 0, &one_group(0, b"t")).await.unwrap();
		let query = Query::segments("timeline.z")
			.unwrap()
			.page_size(NonZeroUsize::new(1).unwrap());
		let next = query.next("0".to_string());
		let err = store.list(&next).try_collect::<Vec<_>>().await.unwrap_err();
		assert_eq!(err, Error::Pagination);
	}

	#[tokio::test]
	async fn paginated_listing_walks_pages_and_excludes_siblings() {
		let store = Store::new(Mock::memory(), "rec");
		for segment in 0..3 {
			store
				.put_segments("timeline.z", segment, &one_group(segment, b"t"))
				.await
				.unwrap();
		}
		store
			.inner()
			.put(
				&Path::from("rec-other/video/.info"),
				Bytes::from_static(b"sibling").into(),
			)
			.await
			.unwrap();

		let query = Query::segments("timeline.z")
			.unwrap()
			.after(&Key::segments("timeline.z", 0).unwrap())
			.unwrap()
			.page_size(NonZeroUsize::new(1).unwrap());
		let streamed: BTreeSet<String> = store
			.list(&query)
			.map_ok(|entry| entry.key.path(&Path::ROOT).unwrap().to_string())
			.try_collect()
			.await
			.unwrap();
		assert_eq!(streamed.len(), 2);

		let mut paged = BTreeSet::new();
		let mut current = Some(query);
		let mut pages = 0;
		while let Some(query) = current {
			let page = store.list_paginated(&query).await.unwrap();
			assert_eq!(page.entries.len(), 1);
			paged.extend(
				page.entries
					.into_iter()
					.map(|entry| entry.key.path(&Path::ROOT).unwrap().to_string()),
			);
			current = page.next;
			pages += 1;
			assert!(pages <= 2, "pagination did not terminate");
		}
		assert_eq!(pages, 2);
		assert_eq!(paged, streamed);

		// A recording-wide walk uses the trailing-slash scope, so the
		// `rec-other` sibling must not appear in any page.
		let query = Query::new().page_size(NonZeroUsize::new(1).unwrap());
		let streamed: BTreeSet<String> = store
			.list(&query)
			.map_ok(|entry| entry.key.path(&Path::ROOT).unwrap().to_string())
			.try_collect()
			.await
			.unwrap();
		assert_eq!(streamed.len(), 3);

		let mut paged = BTreeSet::new();
		let mut current = Some(query);
		let mut pages = 0;
		while let Some(query) = current {
			let page = store.list_paginated(&query).await.unwrap();
			assert_eq!(page.entries.len(), 1);
			paged.extend(
				page.entries
					.into_iter()
					.map(|entry| entry.key.path(&Path::ROOT).unwrap().to_string()),
			);
			current = page.next;
			pages += 1;
			assert!(pages <= 3, "pagination did not terminate");
		}
		assert_eq!(pages, 3);
		assert_eq!(paged, streamed);
	}

	#[tokio::test]
	async fn empty_prefix_lists_the_whole_store() {
		let store = Store::new(Mock::memory(), "");
		store.put_info("catalog.json", &Info::new(0, 1).unwrap()).await.unwrap();
		store.put_segments("video", 4, &one_group(4, b"a")).await.unwrap();
		assert_eq!(store.paginated_prefix(None), None);

		let expected =
			std::collections::HashSet::from([Key::info("catalog.json").unwrap(), Key::segments("video", 4).unwrap()]);
		let streamed: std::collections::HashSet<Key> = store
			.list(&Query::new())
			.map_ok(|entry| entry.key)
			.try_collect()
			.await
			.unwrap();
		assert_eq!(streamed, expected);
		let page = store.list_paginated(&Query::new()).await.unwrap();
		assert_eq!(
			page.entries
				.into_iter()
				.map(|entry| entry.key)
				.collect::<std::collections::HashSet<_>>(),
			expected
		);
		assert!(page.next.is_none());
		let page = store.list_paginated(&Query::segments("video").unwrap()).await.unwrap();
		assert_eq!(page.entries.len(), 1);
	}

	#[test]
	fn directory_results_are_not_silently_dropped() {
		let store = memory();
		let err = store
			.entries(ListResult {
				objects: Vec::new(),
				common_prefixes: vec![Path::from("rec/video")],
				extensions: Default::default(),
			})
			.unwrap_err();
		assert_eq!(err, Error::Directory("rec/video".to_string()));
	}

	#[tokio::test]
	async fn id_endpoints_are_valid_keys() {
		let store = memory();
		let object = one_group(ID_MAX, b"z");
		store.put_segments("t", ID_MAX, &object).await.unwrap();
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
		store.put_segments("audio", 0, &object).await.unwrap();
		assert_eq!(store.get_info("audio").await.unwrap(), info);
		assert_eq!(store.get_segments("audio", 0).await.unwrap(), object);
	}
}
