use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

/// How the coded picture maps onto the viewer's field of view.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Projection {
	/// A flat picture
	#[default]
	Rectilinear,

	/// Viewer is in the middle of wrapped sphere
	Equirectangular {
		/// Degrees of longitude the picture spans, in range 0..360
		fov: Option<f64>,
	},

	Unknown(UnknownProjection),
}

#[derive(Debug, Clone, PartialEq)]
pub struct UnknownProjection(serde_json::Map<String, serde_json::Value>);

impl UnknownProjection {
	pub fn kind(&self) -> Option<&str> {
		self.0.get("kind").and_then(serde_json::Value::as_str)
	}

	pub fn fields(&self) -> &serde_json::Map<String, serde_json::Value> {
		&self.0
	}
}

impl Projection {
	/// Check that projection is in range 0..360.
	pub(crate) fn check(&self) -> crate::Result<()> {
		if let Self::Equirectangular { fov: Some(fov) } = self
			&& !(fov.is_finite() && *fov > 0.0 && *fov <= 360.0)
		{
			return Err(crate::Error::InvalidFieldOfView(*fov));
		}
		Ok(())
	}
}

#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
enum Known {
	Rectilinear {},
	Equirectangular {
		#[serde(default)]
		fov: Option<f64>,
	},
}

impl Serialize for Projection {
	fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		let known = match self {
			Self::Rectilinear => Known::Rectilinear {},
			Self::Equirectangular { fov } => Known::Equirectangular { fov: *fov },
			Self::Unknown(unknown) => return unknown.0.serialize(serializer),
		};
		known.serialize(serializer)
	}
}

impl<'de> Deserialize<'de> for Projection {
	fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		let object = serde_json::Map::deserialize(deserializer)?;

		match object.get("kind").and_then(serde_json::Value::as_str) {
			Some("rectilinear" | "equirectangular") => {
				let known = Known::deserialize(serde_json::Value::Object(object)).map_err(de::Error::custom)?;
				Ok(match known {
					Known::Rectilinear {} => Self::Rectilinear,
					Known::Equirectangular { fov } => Self::Equirectangular { fov },
				})
			}
			_ => Ok(Self::Unknown(UnknownProjection(object))),
		}
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
		assert_eq!(unknown.kind(), Some("cubemap"));
		assert_eq!(serde_json::to_string(&parsed).unwrap(), json);
	}

	#[test]
	fn malformed_known_errors() {
		serde_json::from_str::<Projection>(r#"{"kind":"equirectangular","fov":"wide"}"#).unwrap_err();
		serde_json::from_str::<Projection>(r#"{"kind":"rectilinear","fov":90}"#).unwrap_err();
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
