use bytes::Bytes;
use object_store::path::Path;
use object_store::{ObjectStore, ObjectStoreExt, PutMode, PutPayload};

use crate::info::Info;
use crate::layout;
use crate::{Error, Result};

/// A broadcast's archive: an [`ObjectStore`] and the key prefix it is stored under.
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

	/// Create a track's `.info`, or accept an existing object with the same parsed properties.
	///
	/// An existing recording in an older format is refused with [`Error::Version`]: a
	/// [`Consumer`](crate::Consumer) still reads it, but nothing appends to it.
	pub(crate) async fn put_info(&self, track: &str, info: &Info) -> Result<()> {
		let path = self.info_path(track)?;
		let bytes = info.encode()?;
		match self
			.inner
			.put_opts(&path, PutPayload::from(bytes), PutMode::Create.into())
			.await
		{
			Ok(_) => Ok(()),
			Err(object_store::Error::AlreadyExists { .. }) => {
				let existing = Info::decode(&self.get_bytes(&path).await?)?;
				if existing.version != info.version {
					return Err(Error::Version(existing.version));
				}
				if existing.priority != info.priority {
					return Err(Error::Priority {
						existing: existing.priority,
						intended: info.priority,
					});
				}
				if existing.timescale != info.timescale {
					return Err(Error::TimescaleMismatch {
						existing: existing.timescale,
						intended: info.timescale,
					});
				}
				Ok(())
			}
			Err(err) => Err(err.into()),
		}
	}

	/// Fetch and validate a track's `.info`.
	#[cfg(test)]
	pub(crate) async fn get_info(&self, track: &str) -> Result<Info> {
		Info::decode(&self.get_bytes(&self.info_path(track)?).await?)
	}

	fn info_path(&self, track: &str) -> Result<Path> {
		crate::path::push(&layout::track_dir(&self.prefix, track)?, layout::TRACK_INFO)
	}

	async fn get_bytes(&self, path: &Path) -> Result<Bytes> {
		Ok(self.inner.get(path).await?.bytes().await?)
	}
}

#[cfg(test)]
mod tests {
	use object_store::memory::InMemory;

	use super::*;

	fn memory() -> Store<InMemory> {
		Store::new(InMemory::new(), "rec")
	}

	#[tokio::test]
	async fn info_create_get_and_idempotent_retry() {
		let store = memory();
		let info = Info::new(1, Some(1_000)).unwrap();
		store.put_info("catalog.json", &info).await.unwrap();
		assert_eq!(store.get_info("catalog.json").await.unwrap(), info);

		let path = Path::from("rec/catalog%2Ejson/.info");
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
			let path = Path::from(format!("rec/{track}/.info"));
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
}
