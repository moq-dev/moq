//! The request paths an export serves, parsed.

use percent_encoding::percent_decode_str;

use super::Kind;
use super::segments;

/// A parsed request path: the broadcast, and the resource under it.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Route {
	/// The broadcast path, percent-decoded. An embedder that scopes its origin rewrites
	/// this relative to that scope before looking it up.
	pub broadcast: String,
	/// The resource requested under the broadcast.
	pub resource: Resource,
}

/// A resource under a broadcast.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Resource {
	/// `master.m3u8`: the HLS multivariant playlist of the instance serving the broadcast now.
	Master,
	/// `manifest.mpd`: the DASH manifest of the instance serving the broadcast now.
	Manifest,
	/// `@{instance}/{kind}/{rendition}/media.m3u8`: a rendition's HLS media playlist.
	#[non_exhaustive]
	Media {
		/// The publisher instance the URL names, without its `@`.
		instance: String,
		/// The rendition's kind.
		kind: Kind,
		/// The rendition's name, percent-decoded.
		rendition: String,
	},
	/// `@{instance}/{kind}/{rendition}/init.{hash}.mp4`: a rendition's CMAF init segment,
	/// answered by [`Rendition::init`](super::Rendition::init).
	#[non_exhaustive]
	Init {
		/// The publisher instance the URL names, without its `@`.
		instance: String,
		/// The rendition's kind.
		kind: Kind,
		/// The rendition's name, percent-decoded.
		rendition: String,
		/// The hash of the init bytes the URL names.
		hash: String,
	},
	/// `@{instance}/{kind}/{rendition}/seg/{segment}.m4s`: a media segment, answered by
	/// [`Rendition::segment`](super::Rendition::segment).
	#[non_exhaustive]
	Segment {
		/// The publisher instance the URL names, without its `@`.
		instance: String,
		/// The rendition's kind.
		kind: Kind,
		/// The rendition's name, percent-decoded.
		rendition: String,
		/// The segment the file name addresses.
		segment: segments::Id,
	},
}

impl Route {
	/// Parse a request path such as `/project/live/@{instance}/video/hd/media.m3u8`, or `None`
	/// when it addresses nothing.
	///
	/// The broadcast occupies every segment before the resource suffix, which for every
	/// resource but the master and manifest starts with the `@{instance}` segment. Each segment is
	/// percent-decoded, and one that decodes to a `/` is refused, so an encoded separator
	/// cannot smuggle a different broadcast past a policy check on [`broadcast`](Self::broadcast).
	pub fn parse(path: &str) -> Option<Self> {
		let parts = path
			.strip_prefix('/')?
			.split('/')
			.map(|part| {
				percent_decode_str(part)
					.decode_utf8()
					.ok()
					.map(|part| part.into_owned())
			})
			.collect::<Option<Vec<_>>>()?;
		if parts.iter().any(String::is_empty) {
			return None;
		}

		let (broadcast, resource) = match parts.as_slice() {
			[broadcast @ .., file] if file == "master.m3u8" => (broadcast, Resource::Master),
			[broadcast @ .., file] if file == "manifest.mpd" => (broadcast, Resource::Manifest),
			[broadcast @ .., instance, kind, rendition, file] if file == "media.m3u8" => (
				broadcast,
				Resource::Media {
					instance: instance_id(instance)?,
					kind: Kind::parse(kind)?,
					rendition: rendition.clone(),
				},
			),
			[broadcast @ .., instance, kind, rendition, file] if init_hash(file).is_some() => (
				broadcast,
				Resource::Init {
					instance: instance_id(instance)?,
					kind: Kind::parse(kind)?,
					rendition: rendition.clone(),
					hash: init_hash(file)?.to_string(),
				},
			),
			[broadcast @ .., instance, kind, rendition, directory, file] if directory == "seg" => (
				broadcast,
				Resource::Segment {
					instance: instance_id(instance)?,
					kind: Kind::parse(kind)?,
					rendition: rendition.clone(),
					segment: segments::Id::parse(file.strip_suffix(".m4s")?)?,
				},
			),
			_ => return None,
		};

		if broadcast.is_empty() || broadcast.iter().any(|part| part.contains('/')) {
			return None;
		}
		Some(Self {
			broadcast: broadcast.join("/"),
			resource,
		})
	}
}

/// The publisher instance an `@{instance}` path segment names.
fn instance_id(part: &str) -> Option<String> {
	part.strip_prefix('@')
		.filter(|instance| !instance.is_empty())
		.map(str::to_string)
}

/// The content hash in an `init.{hash}.mp4` file name.
fn init_hash(file: &str) -> Option<&str> {
	file.strip_prefix("init.")?
		.strip_suffix(".mp4")
		.filter(|hash| !hash.is_empty())
}

#[cfg(test)]
mod tests {
	use super::*;
	use segments::{At, Id};

	#[test]
	fn parses_multisegment_broadcast_and_encoded_rendition() {
		assert_eq!(
			Route::parse("/project/live/@run/video/cam%231%2Fmain%3Falt/media.m3u8"),
			Some(Route {
				broadcast: "project/live".to_string(),
				resource: Resource::Media {
					instance: "run".to_string(),
					kind: Kind::Video,
					rendition: "cam#1/main?alt".to_string(),
				},
			})
		);
	}

	#[test]
	fn parses_encoded_dash_rendition_as_one_segment() {
		let parsed = |path| {
			let route = Route::parse(path)?;
			let rendition = match &route.resource {
				Resource::Media { rendition, .. }
				| Resource::Init { rendition, .. }
				| Resource::Segment { rendition, .. } => rendition.clone(),
				Resource::Master | Resource::Manifest => return None,
			};
			Some((route.broadcast, rendition))
		};

		// The manifest for broadcast `live` and rendition `video/1080p` emits these paths.
		// Unencoded, the same name names no instance.
		let cases = [
			("/live/@run/video/video%2F1080p/init.0123abcd.mp4", "video/1080p"),
			("/live/@run/video/video%2F1080p/seg/ab12cd34.t2000.m4s", "video/1080p"),
			("/live/@run/video/cam%3F1/init.0123abcd.mp4", "cam?1"),
			("/live/@run/video/cam%231/media.m3u8", "cam#1"),
			("/live/@run/audio/100%25/init.0123abcd.mp4", "100%"),
			("/live/@run/video/price%249/seg/ab12cd34.t0.m4s", "price$9"),
		];
		for (path, rendition) in cases {
			assert_eq!(parsed(path), Some(("live".into(), rendition.into())), "{path}");
		}
		assert_eq!(parsed("/live/@run/video/video/1080p/init.0123abcd.mp4"), None);
	}

	#[test]
	fn parses_all_resource_routes() {
		let resource = |path| Route::parse(path).map(|route| route.resource);
		let segment = |generation: Option<&str>, at| Resource::Segment {
			instance: "run".to_string(),
			kind: Kind::Video,
			rendition: "main".to_string(),
			segment: Id {
				generation: generation.map(str::to_string),
				tag: "ab12cd34".to_string(),
				at,
			},
		};
		assert_eq!(resource("/project/live/master.m3u8"), Some(Resource::Master));
		assert_eq!(resource("/project/live/manifest.mpd"), Some(Resource::Manifest));
		assert_eq!(
			resource("/project/live/@run/audio/main/init.0123abcd.mp4"),
			Some(Resource::Init {
				instance: "run".to_string(),
				kind: Kind::Audio,
				rendition: "main".to_string(),
				hash: "0123abcd".to_string(),
			})
		);
		assert_eq!(
			resource("/project/live/@run/video/main/seg/ab12cd34.42.m4s"),
			Some(segment(None, At::Sequence(42)))
		);
		assert_eq!(
			resource("/project/live/@run/video/main/seg/run-1.ab12cd34.42.m4s"),
			Some(segment(Some("run-1"), At::Sequence(42)))
		);
		assert_eq!(
			resource("/project/live/@run/video/main/seg/ab12cd34.t2000.m4s"),
			Some(segment(None, At::Pts(2000)))
		);
		assert_eq!(
			resource("/project/live/@run/video/main/seg/init.ab12cd34.t2000.m4s"),
			Some(segment(Some("init"), At::Pts(2000)))
		);
		assert!(Route::parse("/master.m3u8").is_none());
		assert!(
			Route::parse("/project/live/video/main/media.m3u8").is_none(),
			"a rendition URL names its instance"
		);
		assert!(Route::parse("/project/live/@/video/main/media.m3u8").is_none());
		assert!(Route::parse("/project/live/@run/video/main/unknown").is_none());
		assert!(Route::parse("/project/live/@run/data/main/media.m3u8").is_none());
		assert!(Route::parse("/project/live/@run/video/main/init.mp4").is_none());
		assert!(Route::parse("/project/live/@run/video/main/init..mp4").is_none());
		assert!(Route::parse("/project/live/@run/video/main/seg/ab12cd34.tx.m4s").is_none());
		assert!(
			Route::parse("/project/live/@run/video/main/seg/42.m4s").is_none(),
			"a segment URL names its record track"
		);
		assert!(Route::parse("/project/live/@run/video/main/seg/1.ts").is_none());
	}

	#[test]
	fn rejects_empty_path_segments() {
		assert!(Route::parse("/project//live/master.m3u8").is_none());
	}

	#[test]
	fn rejects_encoded_broadcast_separators() {
		assert!(Route::parse("/project/private%2F/master.m3u8").is_none());
		assert!(Route::parse("/project%2Fprivate/master.m3u8").is_none());
	}
}
