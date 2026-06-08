use std::ops::{Deref, DerefMut};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// An application's catalog extension: a plain serde struct of extra root sections that are
/// serialized as a flat union with the base [`hang::Catalog`].
///
/// Implement it (no methods) on a struct of your own sections, then publish/consume a
/// [`Catalog<YourExt>`]:
///
/// ```
/// use serde::{Deserialize, Serialize};
///
/// #[derive(Serialize, Deserialize, Clone, Default)]
/// struct Scte35Ext {
///     #[serde(skip_serializing_if = "Option::is_none")]
///     scte35: Option<Scte35>,
/// }
///
/// #[derive(Serialize, Deserialize, Clone, Default)]
/// struct Scte35 {
///     splice_id: u32,
/// }
///
/// impl moq_mux::catalog::hang::CatalogExt for Scte35Ext {}
/// ```
pub trait CatalogExt: Serialize + DeserializeOwned + Default + Clone + Send + 'static {}

/// The empty extension: a [`Catalog<NoExt>`] is just the base media catalog.
#[derive(Serialize, Deserialize, Clone, Default, Debug, PartialEq, Eq)]
pub struct NoExt {}

impl CatalogExt for NoExt {}

/// The base [`hang::Catalog`] plus an application extension `E`, serialized as a flat union of both
/// (the base media sections and the extension's sections share one JSON object on the wire).
///
/// Derefs to the base catalog, so the media fields are reachable directly (`catalog.video`); the
/// extension sections live under [`ext`](Self::ext) (`catalog.ext.scte35`). A base consumer that
/// reads a plain [`hang::Catalog`] simply ignores the extension sections.
#[derive(Serialize, Deserialize, Clone, Default, Debug, PartialEq)]
#[serde(bound(serialize = "E: Serialize", deserialize = "E: DeserializeOwned"))]
pub struct Catalog<E: CatalogExt = NoExt> {
	#[serde(flatten)]
	pub base: hang::Catalog,

	#[serde(flatten)]
	pub ext: E,
}

impl<E: CatalogExt> Deref for Catalog<E> {
	type Target = hang::Catalog;

	fn deref(&self) -> &hang::Catalog {
		&self.base
	}
}

impl<E: CatalogExt> DerefMut for Catalog<E> {
	fn deref_mut(&mut self) -> &mut hang::Catalog {
		&mut self.base
	}
}

// Lets the producer derive the MSF track from the base sections.
impl<E: CatalogExt> AsRef<hang::Catalog> for Catalog<E> {
	fn as_ref(&self) -> &hang::Catalog {
		&self.base
	}
}

#[cfg(test)]
mod test {
	use std::task::Poll;

	use serde::{Deserialize, Serialize};

	use super::*;

	#[derive(Serialize, Deserialize, Clone, Default, PartialEq, Debug)]
	struct Scte35Ext {
		#[serde(skip_serializing_if = "Option::is_none")]
		scte35: Option<Scte35>,
	}

	#[derive(Serialize, Deserialize, Clone, Default, PartialEq, Debug)]
	struct Scte35 {
		splice_id: u32,
	}

	impl CatalogExt for Scte35Ext {}

	#[test]
	fn extension_roundtrip() {
		let mut broadcast = moq_net::Broadcast::new().produce();
		let mut producer =
			crate::catalog::hang::Producer::with_catalog(&mut broadcast, Catalog::<Scte35Ext>::default()).unwrap();
		let mut consumer = producer.consume().unwrap();

		// The media pipeline sets a base section (flat, via deref); the app adds its own extension.
		// Sequential locks compose because each starts from the producer's retained catalog.
		producer.lock().user = Some(hang::catalog::User {
			name: Some("alice".to_string()),
			..Default::default()
		});
		producer.lock().ext.scte35 = Some(Scte35 { splice_id: 42 });

		let waiter = kio::Waiter::noop();
		let mut latest = None;
		while let Poll::Ready(Ok(Some(catalog))) = consumer.poll_next(&waiter) {
			latest = Some(catalog);
		}

		let catalog = latest.expect("catalog published");
		assert_eq!(catalog.user.as_ref().unwrap().name.as_deref(), Some("alice"));
		assert_eq!(catalog.ext.scte35, Some(Scte35 { splice_id: 42 }));
	}
}
