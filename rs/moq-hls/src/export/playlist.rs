//! Hand-written HLS media playlist generation.
//!
//! Each HLS record defines one complete segment with its own duration.
//! URLs carry the record track's tag and record number, and that number is the segment's
//! `EXT-X-MEDIA-SEQUENCE`, so every edge derives it alone. URIs are relative to the media playlist
//! (`/<broadcast>/<kind>/<rendition>/media.m3u8`), so they resolve against the rendition
//! directory.

use kio::clock::Wall;
use std::fmt::Write;
use std::sync::Arc;
use std::time::Duration;

/// fMP4 segments via `EXT-X-MAP` require protocol version 6.
const VERSION: u32 = 6;

/// `EXT-X-GAP` requires protocol version 8.
const VERSION_GAP: u32 = 8;

/// Everything a media playlist render needs; built by the crate-internal
/// `Rendition::playlist`.
pub(crate) struct Snapshot {
	/// `EXT-X-TARGETDURATION`, in whole seconds.
	pub target_duration: u64,
	/// `EXT-X-MEDIA-SEQUENCE` of the first listed segment.
	pub media_sequence: u64,
	/// `EXT-X-DISCONTINUITY-SEQUENCE` of the first listed segment.
	pub discontinuity_sequence: u64,
	/// Listed segments, oldest first.
	pub segments: Vec<Segment>,
	/// Whether the broadcast ended (`EXT-X-ENDLIST`).
	pub finished: bool,
	/// The publisher run every segment URL carries (`seg/{generation}.{tag}.{segment}.m4s`).
	pub generation: Option<Arc<str>>,
}

/// One listed segment.
pub(crate) struct Segment {
	/// The source segment number; the URI is `seg/{tag}.{segment}.m4s`.
	pub segment: u64,
	/// The [`tag`](super::segments::tag) of the record track numbering the segment.
	pub tag: String,
	/// `EXTINF` duration.
	pub duration: Duration,
	/// The rendition has no content for this span (`EXT-X-GAP`): the segment keeps its slot in
	/// the rendition's numbering, but a player should not request it.
	pub gap: bool,
	/// Content time jumped before this segment (`EXT-X-DISCONTINUITY`).
	pub discontinuity: bool,
	/// Wall-clock start (`EXT-X-PROGRAM-DATE-TIME`), when the catalog advertises its root
	/// broadcast clock. Rendered on the first segment and after each discontinuity, where the
	/// running sum of `EXTINF`s no longer gives it.
	pub program_date_time: Option<Wall>,
}

/// Render a media playlist for one rendition from a [`Snapshot`], mapping `init.{init}.mp4`.
///
/// `query` is an optional query string (without the leading `?`, e.g. `jwt=<token>`)
/// appended to every child URL (the init map and each segment), so a stock player that
/// does not replay request headers still carries a credential on its follow-up requests.
pub(crate) fn render_media(snapshot: &Snapshot, init: &str, query: Option<&str>) -> String {
	let suffix = query.map(|q| format!("?{q}")).unwrap_or_default();
	let generation = snapshot
		.generation
		.as_deref()
		.map(|g| format!("{g}."))
		.unwrap_or_default();

	// EXT-X-GAP needs a newer protocol version; only require it when actually used.
	let version = if snapshot.segments.iter().any(|s| s.gap) {
		VERSION_GAP
	} else {
		VERSION
	};

	let mut out = String::new();
	let _ = writeln!(out, "#EXTM3U");
	let _ = writeln!(out, "#EXT-X-VERSION:{version}");
	let _ = writeln!(out, "#EXT-X-TARGETDURATION:{}", snapshot.target_duration);
	let _ = writeln!(out, "#EXT-X-MEDIA-SEQUENCE:{}", snapshot.media_sequence);
	if snapshot.discontinuity_sequence > 0 {
		let _ = writeln!(out, "#EXT-X-DISCONTINUITY-SEQUENCE:{}", snapshot.discontinuity_sequence);
	}
	let _ = writeln!(out, "#EXT-X-MAP:URI=\"init.{init}.mp4{suffix}\"");

	for (index, segment) in snapshot.segments.iter().enumerate() {
		if segment.discontinuity {
			let _ = writeln!(out, "#EXT-X-DISCONTINUITY");
		}
		if (index == 0 || segment.discontinuity)
			&& let Some(pdt) = segment.program_date_time
		{
			let _ = writeln!(
				out,
				"#EXT-X-PROGRAM-DATE-TIME:{}",
				humantime::format_rfc3339_millis(pdt.into())
			);
		}
		if segment.gap {
			let _ = writeln!(out, "#EXT-X-GAP");
		}
		let _ = writeln!(out, "#EXTINF:{:.5},", segment.duration.as_secs_f64());
		let _ = writeln!(out, "seg/{generation}{}.{}.m4s{suffix}", segment.tag, segment.segment);
	}

	if snapshot.finished {
		let _ = writeln!(out, "#EXT-X-ENDLIST");
	}

	out
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use super::*;

	#[test]
	fn renders_live_playlist() {
		let snapshot = Snapshot {
			target_duration: 2,
			media_sequence: 10,
			discontinuity_sequence: 0,
			segments: vec![
				Segment {
					segment: 10,
					tag: "ab12cd34".into(),
					duration: Duration::from_secs(2),
					gap: false,
					discontinuity: false,
					program_date_time: Some(Wall::UNIX_EPOCH + Duration::from_millis(1_751_846_400_123)),
				},
				Segment {
					segment: 11,
					tag: "ab12cd34".into(),
					duration: Duration::from_millis(1960),
					gap: false,
					discontinuity: false,
					program_date_time: Some(Wall::UNIX_EPOCH + Duration::from_millis(1_751_846_402_123)),
				},
			],
			finished: false,
			generation: None,
		};

		let out = render_media(&snapshot, "0123abcd", None);
		assert!(out.starts_with("#EXTM3U\n#EXT-X-VERSION:6\n"));
		assert!(out.contains("#EXT-X-TARGETDURATION:2\n"));
		assert!(out.contains("#EXT-X-MEDIA-SEQUENCE:10\n"));
		assert!(out.contains("#EXT-X-MAP:URI=\"init.0123abcd.mp4\"\n"));
		assert!(out.contains("#EXT-X-PROGRAM-DATE-TIME:2025-07-07T00:00:00.123Z\n"));
		assert_eq!(out.matches("PROGRAM-DATE-TIME").count(), 1, "only the first: {out}");
		assert!(out.contains("#EXTINF:2.00000,\nseg/ab12cd34.10.m4s\n"));
		assert!(out.contains("#EXTINF:1.96000,\nseg/ab12cd34.11.m4s\n"));
		assert!(!out.contains("#EXT-X-ENDLIST"));

		// A credential rides every child URL so a header-less player keeps sending it.
		let signed = render_media(&snapshot, "0123abcd", Some("jwt=abc.def"));
		assert!(signed.contains("#EXT-X-MAP:URI=\"init.0123abcd.mp4?jwt=abc.def\"\n"));
		assert!(signed.contains("\nseg/ab12cd34.10.m4s?jwt=abc.def\n"));
		assert!(signed.contains("\nseg/ab12cd34.11.m4s?jwt=abc.def\n"));
	}

	// Content that stops and resumes later restates its wall clock: the stop's zero duration
	// carries none of the jump.
	#[test]
	fn a_discontinuity_restates_the_program_date_time() {
		let at = |millis| Some(Wall::UNIX_EPOCH + Duration::from_millis(millis));
		let segment = |segment, duration, gap, discontinuity, program_date_time| Segment {
			segment,
			tag: "ab12cd34".into(),
			duration: Duration::from_millis(duration),
			gap,
			discontinuity,
			program_date_time,
		};
		let snapshot = Snapshot {
			target_duration: 2,
			media_sequence: 0,
			discontinuity_sequence: 0,
			segments: vec![
				segment(0, 2_000, false, false, at(0)),
				segment(1, 0, true, false, at(2_000)),
				segment(2, 2_000, false, true, at(10_000)),
				segment(3, 2_000, false, false, at(12_000)),
			],
			finished: false,
			generation: None,
		};

		let out = render_media(&snapshot, "0123abcd", None);
		assert!(
			out.contains(
				"seg/ab12cd34.1.m4s\n#EXT-X-DISCONTINUITY\n#EXT-X-PROGRAM-DATE-TIME:1970-01-01T00:00:10.000Z\n#EXTINF:2.00000,\nseg/ab12cd34.2.m4s\n"
			),
			"{out}"
		);
		assert_eq!(out.matches("PROGRAM-DATE-TIME").count(), 2, "{out}");
	}

	#[test]
	fn generation_rides_every_segment_url() {
		let snapshot = Snapshot {
			target_duration: 2,
			media_sequence: 0,
			discontinuity_sequence: 0,
			segments: vec![Segment {
				segment: 0,
				tag: "ab12cd34".into(),
				duration: Duration::from_secs(2),
				gap: false,
				discontinuity: false,
				program_date_time: None,
			}],
			finished: false,
			generation: Some("run-7".into()),
		};

		let out = render_media(&snapshot, "0123abcd", Some("jwt=x"));
		assert!(out.contains("#EXT-X-MAP:URI=\"init.0123abcd.mp4?jwt=x\"\n"));
		assert!(out.contains("\nseg/run-7.ab12cd34.0.m4s?jwt=x\n"));
	}

	#[test]
	fn finished_playlist_has_endlist() {
		let snapshot = Snapshot {
			target_duration: 4,
			media_sequence: 0,
			discontinuity_sequence: 0,
			segments: vec![Segment {
				segment: 0,
				tag: "ab12cd34".into(),
				duration: Duration::from_secs(4),
				gap: false,
				discontinuity: false,
				program_date_time: None,
			}],
			finished: true,
			generation: None,
		};

		let out = render_media(&snapshot, "0123abcd", None);
		assert!(out.contains("#EXT-X-ENDLIST\n"));
		assert!(!out.contains("PROGRAM-DATE-TIME"));
	}
}
