//! Content protection: the DRM systems that encrypt a broadcast's CMAF renditions.
//!
//! The catalog's root `drm` section maps an entry ID to an [`Entry`], and each protected video or
//! audio rendition lists the IDs of the entries that protect it in its `drm` field. A rendition
//! that lists none is clear. Every entry names one DRM system, one Common Encryption
//! [`Scheme`], and one default key, so the entries map 1:1 onto draft-ietf-moq-cmsf's
//! `contentProtections` and a rendition's IDs onto its `contentProtectionRefIDs`.
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
use serde::{Deserialize, Serialize};
use serde_with::{base64::Base64, serde_as};

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

	/// The system ID a short name (`widevine`, `playready`, `fairplay`, `clearkey`) stands for.
	pub fn named(name: &str) -> Option<uuid::Uuid> {
		Some(match name.to_ascii_lowercase().as_str() {
			"widevine" => WIDEVINE,
			"playready" => PLAYREADY,
			"fairplay" => FAIRPLAY,
			"clearkey" => CLEARKEY,
			_ => return None,
		})
	}
}

/// One DRM system protecting renditions under one scheme and one default key.
///
/// The scheme, key, system, and `pssh` are the media's own (the init segment's `schm`, `tenc`,
/// and `pssh` boxes); the license and certificate servers are the publisher's, which the media
/// does not carry.
///
/// Marked `#[non_exhaustive]` so optional fields can be added later. External callers build one
/// with [`Entry::new`].
#[serde_as]
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Entry {
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
	#[serde_as(as = "Option<Base64>")]
	pub pssh: Option<Bytes>,

	/// Where to request a license.
	#[serde(default)]
	pub license: Option<Server>,

	/// Where to fetch the server certificate, which systems such as FairPlay need first.
	#[serde(default)]
	pub certificate: Option<Server>,
}

impl Entry {
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

	/// Whether `other` describes the same system, scheme, and key, which renditions share.
	pub fn same_key(&self, other: &Self) -> bool {
		self.system == other.system && self.scheme == other.scheme && self.kid == other.kid
	}
}

/// A UUID in its hyphenated form only, as draft-ietf-moq-cmsf writes one, refusing the simple,
/// braced, and URN forms [`uuid::Uuid`]'s own decoder also takes.
struct Hyphenated;

impl serde_with::SerializeAs<uuid::Uuid> for Hyphenated {
	fn serialize_as<S: serde::Serializer>(source: &uuid::Uuid, serializer: S) -> Result<S::Ok, S::Error> {
		source.hyphenated().serialize(serializer)
	}
}

impl<'de> serde_with::DeserializeAs<'de, uuid::Uuid> for Hyphenated {
	fn deserialize_as<D: serde::Deserializer<'de>>(deserializer: D) -> Result<uuid::Uuid, D::Error> {
		let text = String::deserialize(deserializer)?;
		match text.len() {
			36 => uuid::Uuid::try_parse(&text).map_err(serde::de::Error::custom),
			_ => Err(serde::de::Error::custom(format_args!(
				"expected a hyphenated UUID: {text:?}"
			))),
		}
	}
}

/// The Common Encryption scheme (ISO/IEC 23001-7) of a protected rendition.
///
/// draft-ietf-moq-cmsf allows only these two; a catalog naming another is refused.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Scheme {
	/// AES-CTR, full sample or subsample.
	Cenc,
	/// AES-CBC pattern encryption with a constant IV, which FairPlay and most hardware decode.
	Cbcs,
}

impl Scheme {
	/// The scheme's four-character code, as `schm` carries it.
	pub fn as_str(&self) -> &'static str {
		match self {
			Self::Cenc => "cenc",
			Self::Cbcs => "cbcs",
		}
	}
}

impl std::fmt::Display for Scheme {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.write_str(self.as_str())
	}
}

impl std::str::FromStr for Scheme {
	type Err = crate::Error;

	fn from_str(s: &str) -> crate::Result<Self> {
		match s {
			"cenc" => Ok(Self::Cenc),
			"cbcs" => Ok(Self::Cbcs),
			other => Err(crate::Error::UnsupportedScheme(other.to_string())),
		}
	}
}

/// The servers a publisher supplies for one DRM system, which the media does not carry.
///
/// An importer copies them into each [`Entry`] of that system.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Servers {
	/// Where to request a license.
	pub license: Option<Server>,
	/// Where to fetch the server certificate.
	pub certificate: Option<Server>,
}

impl Servers {
	/// A license server and no certificate server.
	pub fn license(url: url::Url) -> Self {
		Self {
			license: Some(Server::new(url)),
			certificate: None,
		}
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
		let entry: Entry = serde_json::from_value(json.clone()).unwrap();
		assert_eq!(entry.system, system::CLEARKEY);
		assert_eq!(entry.scheme, Scheme::Cbcs);
		assert_eq!(entry.pssh.as_deref(), Some(&[0u8, 0, 0][..]));
		assert_eq!(entry.license.as_ref().unwrap().kind.as_deref(), Some("EME-1.0"));
		assert_eq!(serde_json::to_value(&entry).unwrap(), json);
	}

	#[test]
	fn another_scheme_is_refused() {
		let json = serde_json::json!({
			"system": "1077efec-c0b2-4d02-ace3-3c1e52e2fb4b",
			"scheme": "cens",
			"kid": "a1b2c3d4-e5f6-0718-293a-4b5c6d7e8f90",
		});
		assert!(serde_json::from_value::<Entry>(json).is_err());
	}

	#[test]
	fn short_system_names_resolve() {
		assert_eq!(system::named("Widevine"), Some(system::WIDEVINE));
		assert_eq!(system::named("primetime"), None);
	}
}
