//! Content protection: the DRM systems that encrypt a broadcast's CMAF renditions.
//!
//! The catalog's root `drm` section maps an entry ID to an [`Entry`], and each protected video or
//! audio rendition lists the IDs of the entries that protect it in its `drm` field. A rendition
//! that lists none is clear. Every entry names one DRM system, one Common Encryption
//! [`Scheme`], and one default key, so each maps onto one of draft-ietf-moq-cmsf's
//! `contentProtections` and a rendition's IDs onto its `contentProtectionRefIDs`. A CMSF entry
//! naming several default keys has no hang form, so it reads as [`Entry::Unknown`].
//!
//! An entry this build cannot read decodes to [`Entry::Unknown`] instead of failing the catalog,
//! and a rendition listing any entry is protected whether or not its entries are readable.
//!
//! ```json
//! {
//!   "drm": {
//!     "1": {
//!       "system": "1077efec-c0b2-4d02-ace3-3c1e52e2fb4b",
//!       "scheme": "cbcs",
//!       "kid": "a1b2c3d4-e5f6-0718-293a-4b5c6d7e8f90",
//!       "pssh": "AAAANHBzc2gB...",
//!       "license": { "url": "https://license.example.com/clearkey" }
//!     }
//!   },
//!   "video": { "renditions": { "hd": { "codec": "avc1.64001f", "drm": ["1"], "...": "..." } } }
//! }
//! ```

use bytes::Bytes;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_with::serde_as;

/// Re-export of the [`uuid`] crate's UUID, which system and key IDs are.
pub use uuid::Uuid;

/// Well-known DRM system IDs, as draft-ietf-moq-cmsf lists them.
pub mod system {
	/// Google Widevine.
	pub const WIDEVINE: uuid::Uuid = uuid::uuid!("edef8ba9-79d6-4ace-a3c8-27dcd51d21ed");
	/// Microsoft PlayReady.
	pub const PLAYREADY: uuid::Uuid = uuid::uuid!("9a04f079-9840-4286-ab92-e65be0885f95");
	/// Apple FairPlay Streaming.
	pub const FAIRPLAY: uuid::Uuid = uuid::uuid!("94ce86fb-07ff-4f43-adb8-93d2fa968ca2");
	/// W3C ClearKey, the Common System ID: for testing, since the key travels in the clear.
	pub const CLEARKEY: uuid::Uuid = uuid::uuid!("1077efec-c0b2-4d02-ace3-3c1e52e2fb4b");
}

/// One root `drm` entry: a DRM system this build reads, or one it does not.
#[expect(
	clippy::large_enum_variant,
	reason = "a catalog holds a few entries, nearly all known, so boxing would only add an allocation and a deref to the common case"
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
	/// An entry this build reads.
	Known(System),

	/// An entry this build cannot read, such as one with a malformed UUID or URL, preserved
	/// verbatim. The renditions listing it are still protected.
	Unknown(UnknownEntry),
}

impl From<System> for Entry {
	fn from(system: System) -> Self {
		Self::Known(system)
	}
}

impl Serialize for Entry {
	fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		match self {
			Self::Known(system) => system.serialize(serializer),
			Self::Unknown(unknown) => unknown.0.serialize(serializer),
		}
	}
}

impl<'de> Deserialize<'de> for Entry {
	fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		let object = serde_json::Map::deserialize(deserializer)?;
		let value = serde_json::Value::Object(object);
		match System::deserialize(&value) {
			Ok(system) => Ok(Self::Known(system)),
			Err(_) => match value {
				serde_json::Value::Object(object) => Ok(Self::Unknown(UnknownEntry(object))),
				_ => unreachable!("built from an object"),
			},
		}
	}
}

/// The raw JSON of a `drm` entry this build cannot read.
///
/// Kept intact so a relay or transcoder that reparses and republishes a catalog round-trips the
/// entry rather than dropping it and leaving its renditions' IDs dangling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownEntry(serde_json::Map<String, serde_json::Value>);

impl UnknownEntry {
	/// An entry holding `fields` verbatim.
	pub fn new(fields: serde_json::Map<String, serde_json::Value>) -> Self {
		Self(fields)
	}

	/// The full JSON object.
	pub fn fields(&self) -> &serde_json::Map<String, serde_json::Value> {
		&self.0
	}
}

/// One DRM system protecting renditions under one scheme and one default key.
///
/// The scheme, key, system, and `pssh` are the media's own (the init segment's `schm`, `tenc`,
/// and `pssh` boxes); the license and certificate servers are the publisher's, which the media
/// does not carry.
///
/// Marked `#[non_exhaustive]` so optional fields can be added later. External callers build one
/// with [`System::new`].
#[serde_as]
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct System {
	/// The DRM system ID, as a UUID string (see [`system`]).
	#[serde_as(as = "Hyphenated")]
	pub system: uuid::Uuid,

	/// How the samples are encrypted.
	pub scheme: Scheme,

	/// The default key ID (`tenc`'s `default_KID`), as a UUID string.
	#[serde_as(as = "Hyphenated")]
	pub kid: uuid::Uuid,

	/// The system's whole `pssh` box from the init segment, base64: what the player hands its
	/// DRM system to request a license.
	#[serde(default)]
	#[serde_as(as = "Option<Padded>")]
	pub pssh: Option<Bytes>,

	/// Where to request a license.
	#[serde(default)]
	pub license: Option<Server>,

	/// Where to fetch the server certificate, which systems such as FairPlay need first.
	#[serde(default)]
	pub certificate: Option<Server>,
}

impl System {
	/// An entry with no `pssh` and no servers.
	pub fn new(system: uuid::Uuid, scheme: Scheme, kid: uuid::Uuid) -> Self {
		Self {
			system,
			scheme,
			kid,
			pssh: None,
			license: None,
			certificate: None,
		}
	}
}

/// A UUID in its hyphenated form only, as draft-ietf-moq-cmsf writes one, refusing the simple,
/// braced, and URN forms [`uuid::Uuid`]'s own decoder also takes.
struct Hyphenated;

impl serde_with::SerializeAs<uuid::Uuid> for Hyphenated {
	fn serialize_as<S: Serializer>(source: &uuid::Uuid, serializer: S) -> Result<S::Ok, S::Error> {
		source.hyphenated().serialize(serializer)
	}
}

impl<'de> serde_with::DeserializeAs<'de, uuid::Uuid> for Hyphenated {
	fn deserialize_as<D: Deserializer<'de>>(deserializer: D) -> Result<uuid::Uuid, D::Error> {
		let text = String::deserialize(deserializer)?;
		match text.len() {
			36 => uuid::Uuid::try_parse(&text).map_err(serde::de::Error::custom),
			_ => Err(serde::de::Error::custom(format_args!(
				"expected a hyphenated UUID: {text:?}"
			))),
		}
	}
}

/// Standard base64 with its padding, refusing the unpadded form serde_with's own decoder also
/// takes, as JS `z.base64()` and CMSF do.
struct Padded;

impl serde_with::SerializeAs<Bytes> for Padded {
	fn serialize_as<S: Serializer>(source: &Bytes, serializer: S) -> Result<S::Ok, S::Error> {
		use base64::Engine;
		serializer.serialize_str(&base64::engine::general_purpose::STANDARD.encode(source))
	}
}

impl<'de> serde_with::DeserializeAs<'de, Bytes> for Padded {
	fn deserialize_as<D: Deserializer<'de>>(deserializer: D) -> Result<Bytes, D::Error> {
		use base64::Engine;
		let text = String::deserialize(deserializer)?;
		let bytes = base64::engine::general_purpose::STANDARD.decode(text);
		bytes.map(Bytes::from).map_err(serde::de::Error::custom)
	}
}

/// The Common Encryption scheme (ISO/IEC 23001-7) of a protected rendition.
///
/// draft-ietf-moq-cmsf allows `cenc` and `cbcs`; any other is kept as [`Other`](Self::Other), so
/// a scheme a later draft adds leaves the rest of the catalog readable.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Scheme {
	/// AES-CTR, full sample or subsample.
	Cenc,
	/// AES-CBC pattern encryption with a constant IV, which FairPlay and most hardware decode.
	Cbcs,
	/// A scheme this build does not know, such as `cens` or `cbc1`.
	Other(String),
}

impl Scheme {
	/// The scheme's four-character code, as `schm` carries it.
	pub fn as_str(&self) -> &str {
		match self {
			Self::Cenc => "cenc",
			Self::Cbcs => "cbcs",
			Self::Other(other) => other,
		}
	}
}

impl From<&str> for Scheme {
	fn from(s: &str) -> Self {
		match s {
			"cenc" => Self::Cenc,
			"cbcs" => Self::Cbcs,
			other => Self::Other(other.to_string()),
		}
	}
}

impl std::fmt::Display for Scheme {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.write_str(self.as_str())
	}
}

impl Serialize for Scheme {
	fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		serializer.serialize_str(self.as_str())
	}
}

impl<'de> Deserialize<'de> for Scheme {
	fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		Ok(Self::from(String::deserialize(deserializer)?.as_str()))
	}
}

/// A license or certificate server.
///
/// Marked `#[non_exhaustive]` so optional fields can be added later. External callers build one
/// with [`Server::new`].
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Server {
	/// The server's URL.
	pub url: url::Url,

	/// The protocol a license server speaks (e.g. `EME-1.0`), or the MIME type of a certificate
	/// (e.g. `application/pkcs7-mime`).
	#[serde(default, rename = "type")]
	pub kind: Option<String>,
}

impl Server {
	/// A server at `url`, of no declared type.
	pub fn new(url: url::Url) -> Self {
		Self { url, kind: None }
	}
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn an_entry_round_trips_with_hang_member_names() {
		let json = serde_json::json!({
			"system": "1077efec-c0b2-4d02-ace3-3c1e52e2fb4b",
			"scheme": "cbcs",
			"kid": "a1b2c3d4-e5f6-0718-293a-4b5c6d7e8f90",
			"pssh": "AAAA",
			"license": { "url": "https://license.example.com/", "type": "EME-1.0" },
		});
		let Entry::Known(system) = serde_json::from_value(json.clone()).unwrap() else {
			panic!("expected a known entry");
		};
		assert_eq!(system.system, system::CLEARKEY);
		assert_eq!(system.scheme, Scheme::Cbcs);
		assert_eq!(system.pssh.as_deref(), Some(&[0u8, 0, 0][..]));
		assert_eq!(system.license.as_ref().unwrap().kind.as_deref(), Some("EME-1.0"));
		assert_eq!(serde_json::to_value(Entry::Known(system)).unwrap(), json);
	}

	/// A scheme a later draft adds is kept, so the entry stays readable.
	#[test]
	fn another_scheme_is_kept() {
		let json = serde_json::json!({
			"system": "1077efec-c0b2-4d02-ace3-3c1e52e2fb4b",
			"scheme": "cens",
			"kid": "a1b2c3d4-e5f6-0718-293a-4b5c6d7e8f90",
		});
		let Entry::Known(system) = serde_json::from_value(json.clone()).unwrap() else {
			panic!("expected a known entry");
		};
		assert_eq!(system.scheme, Scheme::Other("cens".into()));
		assert_eq!(serde_json::to_value(Entry::Known(system)).unwrap(), json);
	}

	/// An entry this build cannot read is preserved verbatim rather than failing.
	#[test]
	fn an_unreadable_entry_round_trips() {
		for json in [
			// A UUID without hyphens.
			serde_json::json!({ "system": "1077efecc0b24d02ace33c1e52e2fb4b", "scheme": "cenc", "kid": "a1b2c3d4-e5f6-0718-293a-4b5c6d7e8f90" }),
			// A license URL that does not parse.
			serde_json::json!({ "system": "1077efec-c0b2-4d02-ace3-3c1e52e2fb4b", "scheme": "cenc", "kid": "a1b2c3d4-e5f6-0718-293a-4b5c6d7e8f90", "license": { "url": "not a url" } }),
			// A pssh without its base64 padding, which `@moq/hang` refuses too.
			serde_json::json!({ "system": "1077efec-c0b2-4d02-ace3-3c1e52e2fb4b", "scheme": "cenc", "kid": "a1b2c3d4-e5f6-0718-293a-4b5c6d7e8f90", "pssh": "AAAAAA" }),
			// A future shape with no fields this build knows.
			serde_json::json!({ "keySystem": "future" }),
		] {
			let entry: Entry = serde_json::from_value(json.clone()).unwrap();
			assert!(matches!(entry, Entry::Unknown(_)), "{json}");
			assert_eq!(serde_json::to_value(&entry).unwrap(), json);
		}
	}

	/// An entry that is not even an object is malformed.
	#[test]
	fn a_non_object_entry_is_refused() {
		assert!(serde_json::from_value::<Entry>(serde_json::json!("cenc")).is_err());
	}
}
