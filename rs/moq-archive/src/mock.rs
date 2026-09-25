//! A test object store: any inner store plus an operation log, injected failures, and
//! S3-style paginated listing.

use std::sync::{Arc, Mutex};

use futures::stream::BoxStream;
use futures::{StreamExt, TryStreamExt};
use object_store::list::{PaginatedListOptions, PaginatedListResult, PaginatedListStore};
use object_store::memory::InMemory;
use object_store::path::Path;
use object_store::{
	CopyOptions, GetOptions, GetResult, ListResult, MultipartUpload, ObjectMeta, ObjectStore, PutMultipartOptions,
	PutOptions, PutPayload, PutResult,
};

/// S3's ListObjectsV2 page limit.
const MAX_KEYS: usize = 1000;

/// One call made against a [`Mock`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Op {
	Get(String),
	Put(String),
	Delete(String),
	/// A streaming or paginated listing, with its prefix and exclusive offset.
	List { prefix: String, offset: Option<String> },
}

#[derive(Debug, Default)]
struct State {
	ops: Vec<Op>,
	/// PUTs whose path contains any of these fail.
	fail_puts: Vec<String>,
	/// GETs whose path contains any of these return Not Found.
	hide_gets: Vec<String>,
	/// Streaming listings end with an error after every entry.
	fail_lists: bool,
	/// Streaming listings yield entries in descending order, like a backend that promises none.
	unordered: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct Mock {
	inner: Arc<dyn ObjectStore>,
	state: Arc<Mutex<State>>,
}

impl Mock {
	pub fn new(inner: impl ObjectStore) -> Self {
		Self {
			inner: Arc::new(inner),
			state: Default::default(),
		}
	}

	pub fn memory() -> Self {
		Self::new(InMemory::new())
	}

	/// Share this store's objects, without its log or failures.
	pub fn fork(&self) -> Self {
		Self {
			inner: self.inner.clone(),
			state: Default::default(),
		}
	}

	/// Every call since the last take.
	pub fn take(&self) -> Vec<Op> {
		std::mem::take(&mut self.state().ops)
	}

	/// Paths of every GET since the last take.
	pub fn gets(&self) -> Vec<String> {
		self.take()
			.into_iter()
			.filter_map(|op| match op {
				Op::Get(path) => Some(path),
				_ => None,
			})
			.collect()
	}

	pub fn fail_puts(&self, pattern: &str) -> &Self {
		self.state().fail_puts.push(pattern.to_string());
		self
	}

	pub fn hide_gets(&self, pattern: &str) -> &Self {
		self.state().hide_gets.push(pattern.to_string());
		self
	}

	pub fn fail_lists(&self) -> &Self {
		self.state().fail_lists = true;
		self
	}

	pub fn unordered(&self) -> &Self {
		self.state().unordered = true;
		self
	}

	/// Clear every injected failure.
	pub fn heal(&self) {
		let mut state = self.state();
		state.fail_puts.clear();
		state.hide_gets.clear();
		state.fail_lists = false;
	}

	fn state(&self) -> std::sync::MutexGuard<'_, State> {
		self.state.lock().unwrap()
	}

	fn log(&self, op: Op) {
		self.state().ops.push(op);
	}

	fn listed(&self, prefix: Option<&Path>, offset: Option<&Path>) -> BoxStream<'static, object_store::Result<ObjectMeta>> {
		self.log(Op::List {
			prefix: prefix.map(ToString::to_string).unwrap_or_default(),
			offset: offset.map(ToString::to_string),
		});
		let listed = match offset {
			Some(offset) => self.inner.list_with_offset(prefix, offset),
			None => self.inner.list(prefix),
		};
		let state = self.state();
		let listed = match state.unordered {
			true => futures::stream::once(async move {
				let mut metas: Vec<ObjectMeta> = listed.try_collect().await?;
				metas.sort_by(|a, b| b.location.cmp(&a.location));
				Ok::<_, object_store::Error>(futures::stream::iter(metas.into_iter().map(Ok)))
			})
			.try_flatten()
			.boxed(),
			false => listed,
		};
		match state.fail_lists {
			true => listed
				.chain(futures::stream::once(async { Err(unsupported("list")) }))
				.boxed(),
			false => listed,
		}
	}
}

fn unsupported(operation: &str) -> object_store::Error {
	object_store::Error::NotImplemented {
		operation: operation.into(),
		implementer: "Mock".into(),
	}
}

impl std::fmt::Display for Mock {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "Mock({})", self.inner)
	}
}

#[async_trait::async_trait]
impl ObjectStore for Mock {
	async fn put_opts(&self, location: &Path, payload: PutPayload, opts: PutOptions) -> object_store::Result<PutResult> {
		self.log(Op::Put(location.to_string()));
		if self.state().fail_puts.iter().any(|p| location.as_ref().contains(p)) {
			return Err(unsupported("put"));
		}
		self.inner.put_opts(location, payload, opts).await
	}

	async fn put_multipart_opts(
		&self,
		location: &Path,
		opts: PutMultipartOptions,
	) -> object_store::Result<Box<dyn MultipartUpload>> {
		self.inner.put_multipart_opts(location, opts).await
	}

	async fn get_opts(&self, location: &Path, options: GetOptions) -> object_store::Result<GetResult> {
		self.log(Op::Get(location.to_string()));
		if self.state().hide_gets.iter().any(|p| location.as_ref().contains(p)) {
			return Err(object_store::Error::NotFound {
				path: location.to_string(),
				source: "hidden".into(),
			});
		}
		self.inner.get_opts(location, options).await
	}

	fn delete_stream(
		&self,
		locations: BoxStream<'static, object_store::Result<Path>>,
	) -> BoxStream<'static, object_store::Result<Path>> {
		let state = self.state.clone();
		let locations = locations
			.inspect_ok(move |path| state.lock().unwrap().ops.push(Op::Delete(path.to_string())))
			.boxed();
		self.inner.delete_stream(locations)
	}

	fn list(&self, prefix: Option<&Path>) -> BoxStream<'static, object_store::Result<ObjectMeta>> {
		self.listed(prefix, None)
	}

	fn list_with_offset(&self, prefix: Option<&Path>, offset: &Path) -> BoxStream<'static, object_store::Result<ObjectMeta>> {
		self.listed(prefix, Some(offset))
	}

	async fn list_with_delimiter(&self, prefix: Option<&Path>) -> object_store::Result<ListResult> {
		self.inner.list_with_delimiter(prefix).await
	}

	async fn copy_opts(&self, from: &Path, to: &Path, options: CopyOptions) -> object_store::Result<()> {
		self.inner.copy_opts(from, to, options).await
	}
}

/// ListObjectsV2 semantics: a raw string prefix, lexical order, an exclusive `start-after`
/// offset, at most 1000 keys, and an opaque continuation token that resumes after the last key.
#[async_trait::async_trait]
impl PaginatedListStore for Mock {
	async fn list_paginated(
		&self,
		prefix: Option<&str>,
		opts: PaginatedListOptions,
	) -> object_store::Result<PaginatedListResult> {
		if opts.delimiter.is_some() {
			return Err(unsupported("delimiter"));
		}
		self.log(Op::List {
			prefix: prefix.unwrap_or_default().to_string(),
			offset: opts.offset.clone(),
		});

		let mut metas: Vec<ObjectMeta> = self.inner.list(None).try_collect().await?;
		metas.sort_by(|a, b| a.location.as_ref().cmp(b.location.as_ref()));
		let after = opts.page_token.or(opts.offset);
		metas.retain(|meta| {
			let key = meta.location.as_ref();
			prefix.is_none_or(|prefix| key.starts_with(prefix)) && after.as_deref().is_none_or(|after| key > after)
		});

		let take = opts.max_keys.unwrap_or(MAX_KEYS).clamp(1, MAX_KEYS);
		let page_token = (metas.len() > take).then(|| metas[take - 1].location.to_string());
		metas.truncate(take);
		Ok(PaginatedListResult {
			result: ListResult {
				objects: metas,
				common_prefixes: Vec::new(),
				extensions: Default::default(),
			},
			page_token,
		})
	}
}
