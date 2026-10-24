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
	/// `master.m3u8`: the HLS multivariant playlist.
	Master,
	/// `manifest.mpd`: the DASH manifest.
	Manifest,
	/// `{kind}/{rendition}/media.m3u8`: a rendition's HLS media playlist.
	#[non_exhaustive]
	Media {
		/// The rendition's kind.
		kind: Kind,
		/// The rendition's name, percent-decoded.
		rendition: String,
	},
	/// `{kind}/{rendition}/init.{hash}.mp4`: a rendition's CMAF init segment, answered by
	/// [`Rendition::init`](super::Rendition::init).
	#[non_exhaustive]
	Init {
		/// The rendition's kind.
		kind: Kind,
		/// The rendition's name, percent-decoded.
		rendition: String,
		/// The hash of the init bytes the URL names.
		hash: String,
	},
	/// `{kind}/{rendition}/seg/{segment}.m4s`: a media segment, answered by
	/// [`Rendition::segment`](super::Rendition::segment).
	#[non_exhaustive]
	Segment {
		/// The rendition's kind.
		kind: Kind,
		/// The rendition's name, percent-decoded.
		rendition: String,
		/// The segment the file name addresses.
		segment: segments::Id,
	},
}

impl Route {
	/// Parse a request path such as `/project/live/video/hd/media.m3u8`, or `None` when it
	/// addresses nothing.
	///
	/// The broadcast occupies every segment before the resource suffix. Each segment is
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
			[broadcast @ .., kind, rendition, file] if file == "media.m3u8" => (
				broadcast,
				Resource::Media {
					kind: Kind::parse(kind)?,
					rendition: rendition.clone(),
				},
			),
			[broadcast @ .., kind, rendition, file] if init_hash(file).is_some() => (
				broadcast,
				Resource::Init {
					kind: Kind::parse(kind)?,
					rendition: rendition.clone(),
					hash: init_hash(file)?.to_string(),
				},
			),
			[broadcast @ .., kind, rendition, directory, file] if directory == "seg" => (
				broadcast,
				Resource::Segment {
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
			Route::parse("/project/live/video/cam%231%2Fmain%3Falt/media.m3u8"),
			Some(Route {
				broadcast: "project/live".to_string(),
				resource: Resource::Media {
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
		// Unencoded, the same name is broadcast `live/video`, rendition `1080p`.
		let cases = [
			("/live/video/video%2F1080p/init.0123abcd.mp4", "video/1080p"),
			("/live/video/video%2F1080p/seg/ab12cd34.t2000.m4s", "video/1080p"),
			("/live/video/cam%3F1/init.0123abcd.mp4", "cam?1"),
			("/live/video/cam%231/media.m3u8", "cam#1"),
			("/live/audio/100%25/init.0123abcd.mp4", "100%"),
			("/live/video/price%249/seg/ab12cd34.t0.m4s", "price$9"),
		];
		for (path, rendition) in cases {
			assert_eq!(parsed(path), Some(("live".into(), rendition.into())), "{path}");
		}
		assert_eq!(
			parsed("/live/video/video/1080p/init.0123abcd.mp4"),
			Some(("live/video".into(), "1080p".into()))
		);
	}

	#[test]
	fn parses_all_resource_routes() {
		let resource = |path| Route::parse(path).map(|route| route.resource);
		let segment = |generation: Option<&str>, at| Resource::Segment {
			kind: Kind::Video,
			rendition: "main".to_string(),
			segment: Id {
				generation: generation.map(str::to_string),
				reference: "ab12cd34".to_string(),
				at,
			},
		};
		assert_eq!(resource("/project/live/master.m3u8"), Some(Resource::Master));
		assert_eq!(resource("/project/live/manifest.mpd"), Some(Resource::Manifest));
		assert_eq!(
			resource("/project/live/audio/main/init.0123abcd.mp4"),
			Some(Resource::Init {
				kind: Kind::Audio,
				rendition: "main".to_string(),
				hash: "0123abcd".to_string(),
			})
		);
		assert_eq!(
			resource("/project/live/video/main/seg/ab12cd34.42.m4s"),
			Some(segment(None, At::Sequence(42)))
		);
		assert_eq!(
			resource("/project/live/video/main/seg/run-1.ab12cd34.42.m4s"),
			Some(segment(Some("run-1"), At::Sequence(42)))
		);
		assert_eq!(
			resource("/project/live/video/main/seg/ab12cd34.t2000.m4s"),
			Some(segment(None, At::Pts(2000)))
		);
		assert_eq!(
			resource("/project/live/video/main/seg/init.ab12cd34.t2000.m4s"),
			Some(segment(Some("init"), At::Pts(2000)))
		);
		assert!(Route::parse("/master.m3u8").is_none());
		assert!(Route::parse("/project/live/video/main/unknown").is_none());
		assert!(Route::parse("/project/live/data/main/media.m3u8").is_none());
		assert!(Route::parse("/project/live/video/main/init.mp4").is_none());
		assert!(Route::parse("/project/live/video/main/init..mp4").is_none());
		assert!(Route::parse("/project/live/video/main/seg/ab12cd34.tx.m4s").is_none());
		assert!(
			Route::parse("/project/live/video/main/seg/42.m4s").is_none(),
			"a segment URL names its reference"
		);
		assert!(Route::parse("/project/live/video/main/seg/1.ts").is_none());
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
