use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

/// How a rendition's decoded picture maps onto the viewer's field of view.
///
/// Absent means [`Rectilinear`](Self::Rectilinear). An unrecognized `kind` decodes to
/// [`Projection::Unknown`] instead of failing, and a consumer treats it as absent.
#[derive(Debug, Clone, PartialEq, Default)]
#[non_exhaustive]
pub enum Projection {
	/// A flat picture.
	#[default]
	Rectilinear,

	/// Longitude linear across the width and latitude down the height, the full 180 degrees,
	/// surrounding a viewer at its centre.
	Equirectangular {
		/// The degrees of longitude the picture spans, centred straight ahead, in (0, 360].
		/// Absent means 360; VR180 is 180.
		fov: Option<f64>,
	},

	/// A projection this build does not recognize, preserved verbatim.
	Unknown(UnknownProjection),
}

/// The raw JSON of a projection whose `kind` is not recognized.
///
/// Kept intact so a relay or transcoder that reparses and republishes a catalog round-trips it.
#[derive(Debug, Clone, PartialEq)]
pub struct UnknownProjection(serde_json::Map<String, serde_json::Value>);

impl UnknownProjection {
	/// The `kind` as it appeared on the wire.
	pub fn kind(&self) -> &str {
		// Deserialize refuses a projection without a string kind, so this is never empty.
		self.0
			.get("kind")
			.and_then(serde_json::Value::as_str)
			.unwrap_or_default()
	}

	/// The full JSON object, including `kind`.
	pub fn fields(&self) -> &serde_json::Map<String, serde_json::Value> {
		&self.0
	}
}

impl Projection {
	/// Refuse a field of view outside (0, 360] degrees.
	pub fn check(&self) -> crate::Result<()> {
		if let Self::Equirectangular { fov: Some(fov) } = self
			&& !(*fov > 0.0 && *fov <= 360.0)
		{
			return Err(crate::Error::InvalidFieldOfView(*fov));
		}
		Ok(())
	}
}

/// The projections this build knows, split out so the tagged representation stays derived.
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum Known {
	Rectilinear,
	Equirectangular {
		#[serde(default)]
		fov: Option<f64>,
	},
}

impl Serialize for Projection {
	fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		let known = match self {
			Self::Rectilinear => Known::Rectilinear,
			Self::Equirectangular { fov } => Known::Equirectangular { fov: *fov },
			Self::Unknown(unknown) => return unknown.0.serialize(serializer),
		};
		known.serialize(serializer)
	}
}

impl<'de> Deserialize<'de> for Projection {
	fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		let object = serde_json::Map::deserialize(deserializer)?;

		let Some(kind) = object.get("kind").and_then(serde_json::Value::as_str) else {
			return Err(de::Error::custom("projection kind must be a string"));
		};

		// Only route kinds we know to the derived enum, so a malformed known projection is still
		// a hard error instead of silently becoming Unknown.
		let projection = match kind {
			"rectilinear" | "equirectangular" => {
				match Known::deserialize(serde_json::Value::Object(object)).map_err(de::Error::custom)? {
					Known::Rectilinear => Self::Rectilinear,
					Known::Equirectangular { fov } => Self::Equirectangular { fov },
				}
			}
			_ => Self::Unknown(UnknownProjection(object)),
		};
		projection.check().map_err(de::Error::custom)?;
		Ok(projection)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn round_trips() {
		for (json, projection) in [
			(r#"{"kind":"rectilinear"}"#, Projection::Rectilinear),
			(
				r#"{"kind":"equirectangular"}"#,
				Projection::Equirectangular { fov: None },
			),
			(
				r#"{"kind":"equirectangular","fov":180.0}"#,
				Projection::Equirectangular { fov: Some(180.0) },
			),
		] {
			let parsed: Projection = serde_json::from_str(json).unwrap();
			assert_eq!(parsed, projection);
			assert_eq!(serde_json::to_string(&parsed).unwrap(), json);
		}
	}

	#[test]
	fn unknown_round_trips() {
		let json = r#"{"face":"front","kind":"cubemap"}"#;
		let parsed: Projection = serde_json::from_str(json).unwrap();
		let Projection::Unknown(unknown) = &parsed else {
			panic!("expected unknown: {parsed:?}");
		};
		assert_eq!(unknown.kind(), "cubemap");
		assert_eq!(serde_json::to_string(&parsed).unwrap(), json);
	}

	#[test]
	fn unknown_fields_are_ignored() {
		// A newer publisher may add a parameter to a known kind; this reader must not refuse it.
		let parsed: Projection = serde_json::from_str(r#"{"kind":"rectilinear","fov":90}"#).unwrap();
		assert_eq!(parsed, Projection::Rectilinear);
	}

	#[test]
	fn malformed_known_is_refused() {
		serde_json::from_str::<Projection>(r#"{"fov":180}"#).unwrap_err();
		serde_json::from_str::<Projection>(r#"{"kind":5}"#).unwrap_err();
		serde_json::from_str::<Projection>(r#"{"kind":"equirectangular","fov":"wide"}"#).unwrap_err();
		for fov in ["0", "-90", "360.5"] {
			let json = format!(r#"{{"kind":"equirectangular","fov":{fov}}}"#);
			serde_json::from_str::<Projection>(&json).unwrap_err();
		}
	}

	#[test]
	fn fov_must_be_in_range() {
		for fov in [0.0, -90.0, 360.5, f64::NAN, f64::INFINITY] {
			Projection::Equirectangular { fov: Some(fov) }.check().unwrap_err();
		}
		for fov in [None, Some(0.5), Some(180.0), Some(360.0)] {
			Projection::Equirectangular { fov }.check().unwrap();
		}
	}
}
