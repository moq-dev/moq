use std::borrow::Cow;
use std::ops::{Deref, DerefMut};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use super::Media;

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

/// The base media sections plus an application extension `E`, serialized as a flat union (the
/// `video`/`audio` sections and the extension's sections share one JSON object on the wire).
///
/// `video` and `audio` are direct fields (`catalog.video`), and the catalog derefs to the
/// extension so its sections are reachable directly too (`catalog.scte35`, or `catalog.ext.scte35`
/// explicitly). A base consumer that reads a plain [`hang::Catalog`] simply ignores the extension.
#[derive(Serialize, Deserialize, Clone, Default, Debug, PartialEq)]
#[serde(bound(serialize = "E: Serialize", deserialize = "E: DeserializeOwned"))]
pub struct Catalog<E: CatalogExt = NoExt> {
	#[serde(default)]
	pub video: hang::catalog::Video,

	#[serde(default)]
	pub audio: hang::catalog::Audio,

	#[serde(flatten)]
	pub ext: E,
}

// Deref to the extension so its sections are reachable directly (the base media sections are
// already real fields, so they shadow this and stay accessible as `catalog.video`/`catalog.audio`).
impl<E: CatalogExt> Deref for Catalog<E> {
	type Target = E;

	fn deref(&self) -> &E {
		&self.ext
	}
}

impl<E: CatalogExt> DerefMut for Catalog<E> {
	fn deref_mut(&mut self) -> &mut E {
		&mut self.ext
	}
}

// Lets the producer derive the MSF track from the media sections.
impl<E: CatalogExt> Media for Catalog<E> {
	fn media(&self) -> Cow<'_, hang::Catalog> {
		Cow::Owned(hang::Catalog {
			video: self.video.clone(),
			audio: self.audio.clone(),
			..Default::default()
		})
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

		// The media pipeline sets a base section (flat field); the app adds its own extension.
		// Sequential locks compose because each starts from the producer's retained catalog.
		producer.lock().audio.renditions.insert(
			"audio0".to_string(),
			hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, 48_000, 2),
		);
		producer.lock().scte35 = Some(Scte35 { splice_id: 42 }); // flat, via deref to the extension

		let waiter = kio::Waiter::noop();
		let mut latest = None;
		while let Poll::Ready(Ok(Some(catalog))) = consumer.poll_next(&waiter) {
			latest = Some(catalog);
		}

		let catalog = latest.expect("catalog published");
		assert!(catalog.audio.renditions.contains_key("audio0"));
		assert_eq!(catalog.scte35, Some(Scte35 { splice_id: 42 }));
	}
}
