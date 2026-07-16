//! Hand-written HLS / LL-HLS media playlist generation.
//!
//! `m3u8-rs` can parse classic playlists but cannot emit the LL-HLS tags
//! (`EXT-X-PART`, `EXT-X-PART-INF`, `EXT-X-SERVER-CONTROL`,
//! `EXT-X-PRELOAD-HINT`), so the export playlists are written by hand. URIs are
//! relative to the media playlist (`/<broadcast>/<rendition>/media.m3u8`), so
//! they resolve against the rendition directory.

use std::fmt::Write;

use super::store::Snapshot;

/// LL-HLS compatibility version: required for `EXT-X-PART` and friends.
const VERSION: u32 = 9;

/// Index of the oldest segment whose parts still belong in the playlist.
///
/// A player only fetches parts near the live edge, and HLS requires them to be listed
/// only within three target durations of it. Listing parts for the whole (16s+) window
/// instead just bloats every reload with URIs nobody requests.
fn oldest_with_parts(snapshot: &Snapshot) -> usize {
	let keep = (snapshot.target_duration * 3) as f64;

	let mut duration = 0.0;
	let mut oldest = snapshot.segments.len();
	for (position, segment) in snapshot.segments.iter().enumerate().rev() {
		if duration >= keep {
			break;
		}
		duration += segment.duration;
		oldest = position;
	}

	oldest
}

/// Render a media playlist for one rendition from a [`Snapshot`].
pub fn render_media(snapshot: &Snapshot) -> String {
	// PART-HOLD-BACK must be at least 3x the part target (HLS spec).
	let part_hold_back = snapshot.part_target * 3.0;

	let mut out = String::new();
	let _ = writeln!(out, "#EXTM3U");
	let _ = writeln!(out, "#EXT-X-VERSION:{VERSION}");
	let _ = writeln!(out, "#EXT-X-TARGETDURATION:{}", snapshot.target_duration);
	let _ = writeln!(
		out,
		"#EXT-X-SERVER-CONTROL:CAN-BLOCK-RELOAD=YES,PART-HOLD-BACK={part_hold_back:.3}"
	);
	let _ = writeln!(out, "#EXT-X-PART-INF:PART-TARGET={:.3}", snapshot.part_target);
	let _ = writeln!(out, "#EXT-X-MEDIA-SEQUENCE:{}", snapshot.media_sequence);
	if snapshot.discontinuity_sequence > 0 {
		let _ = writeln!(out, "#EXT-X-DISCONTINUITY-SEQUENCE:{}", snapshot.discontinuity_sequence);
	}
	let _ = writeln!(out, "#EXT-X-MAP:URI=\"init.mp4\"");

	let oldest_with_parts = oldest_with_parts(snapshot);

	for (position, segment) in snapshot.segments.iter().enumerate() {
		if segment.discontinuity {
			let _ = writeln!(out, "#EXT-X-DISCONTINUITY");
		}
		if position >= oldest_with_parts {
			for (index, part) in segment.parts.iter().enumerate() {
				let independent = if part.independent { ",INDEPENDENT=YES" } else { "" };
				let _ = writeln!(
					out,
					"#EXT-X-PART:DURATION={:.5},URI=\"part/{}/{}.m4s\"{}",
					part.duration, segment.sequence, index, independent
				);
			}
		}
		if segment.complete {
			let _ = writeln!(out, "#EXTINF:{:.5},", segment.duration);
			let _ = writeln!(out, "seg/{}.m4s", segment.sequence);
		}
	}

	if snapshot.finished {
		let _ = writeln!(out, "#EXT-X-ENDLIST");
	} else {
		// Hint the next part at the live edge so the player can pre-request it.
		let (sequence, index) = match snapshot.segments.last() {
			Some(last) if !last.complete => (last.sequence, last.parts.len()),
			_ => (snapshot.next_sequence, 0),
		};
		let _ = writeln!(out, "#EXT-X-PRELOAD-HINT:TYPE=PART,URI=\"part/{sequence}/{index}.m4s\"");
	}

	out
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::export::store::{PartMeta, SegmentMeta};

	fn part(duration: f64, independent: bool) -> PartMeta {
		PartMeta { duration, independent }
	}

	#[test]
	fn renders_ll_hls_tags() {
		let snapshot = Snapshot {
			init_ready: true,
			part_target: 0.5,
			target_duration: 1,
			media_sequence: 10,
			discontinuity_sequence: 0,
			next_sequence: 12,
			segments: vec![
				SegmentMeta {
					sequence: 10,
					parts: vec![part(0.5, true), part(0.5, false)],
					duration: 1.0,
					complete: true,
					discontinuity: false,
				},
				SegmentMeta {
					sequence: 11,
					parts: vec![part(0.5, true)],
					duration: 0.5,
					complete: false,
					discontinuity: false,
				},
			],
			finished: false,
		};

		let out = render_media(&snapshot);

		assert!(out.starts_with("#EXTM3U\n#EXT-X-VERSION:9\n"));
		assert!(!out.contains("#EXT-X-DISCONTINUITY"));
		assert!(out.contains("#EXT-X-TARGETDURATION:1\n"));
		// PART-HOLD-BACK must be >= 3x PART-TARGET.
		assert!(out.contains("PART-HOLD-BACK=1.500"));
		assert!(out.contains("CAN-BLOCK-RELOAD=YES"));
		assert!(out.contains("#EXT-X-PART-INF:PART-TARGET=0.500\n"));
		assert!(out.contains("#EXT-X-MEDIA-SEQUENCE:10\n"));
		assert!(out.contains("#EXT-X-MAP:URI=\"init.mp4\"\n"));
		// First part of the complete segment is independent; the second is not.
		assert!(out.contains("#EXT-X-PART:DURATION=0.50000,URI=\"part/10/0.m4s\",INDEPENDENT=YES\n"));
		assert!(out.contains("#EXT-X-PART:DURATION=0.50000,URI=\"part/10/1.m4s\"\n"));
		assert!(!out.contains("part/10/1.m4s\",INDEPENDENT"));
		// Completed segment gets an EXTINF + segment URI.
		assert!(out.contains("#EXTINF:1.00000,\nseg/10.m4s\n"));
		// Live edge: preload hint points at the next (not-yet-present) part.
		assert!(out.contains("#EXT-X-PRELOAD-HINT:TYPE=PART,URI=\"part/11/1.m4s\"\n"));
		assert!(!out.contains("#EXT-X-ENDLIST"));
	}

	#[test]
	fn finished_playlist_has_endlist_and_no_preload() {
		let snapshot = Snapshot {
			init_ready: true,
			part_target: 1.0,
			target_duration: 1,
			media_sequence: 0,
			discontinuity_sequence: 0,
			next_sequence: 1,
			segments: vec![SegmentMeta {
				sequence: 0,
				parts: vec![part(1.0, true)],
				duration: 1.0,
				complete: true,
				discontinuity: false,
			}],
			finished: true,
		};

		let out = render_media(&snapshot);
		assert!(out.contains("#EXT-X-ENDLIST\n"));
		assert!(!out.contains("#EXT-X-PRELOAD-HINT"));
	}

	#[test]
	fn discontinuity_precedes_resumed_segment() {
		let snapshot = Snapshot {
			init_ready: true,
			part_target: 1.0,
			target_duration: 1,
			media_sequence: 0,
			discontinuity_sequence: 0,
			next_sequence: 2,
			segments: vec![
				SegmentMeta {
					sequence: 0,
					parts: vec![part(1.0, true)],
					duration: 1.0,
					complete: true,
					discontinuity: false,
				},
				// First segment after a resume: tagged discontinuous.
				SegmentMeta {
					sequence: 1,
					parts: vec![part(1.0, true)],
					duration: 1.0,
					complete: true,
					discontinuity: true,
				},
			],
			finished: false,
		};

		let out = render_media(&snapshot);
		// The tag precedes seg 1's parts, not seg 0's.
		let disc = out.find("#EXT-X-DISCONTINUITY").expect("discontinuity tag");
		let seg0 = out.find("part/0/0.m4s").expect("seg 0 part");
		let seg1 = out.find("part/1/0.m4s").expect("seg 1 part");
		assert!(
			seg0 < disc && disc < seg1,
			"discontinuity must sit between seg 0 and seg 1"
		);
	}

	/// Parts far behind the live edge are dropped, but their segments stay playable.
	#[test]
	fn trims_parts_far_behind_the_live_edge() {
		let segments = (0..10)
			.map(|sequence| SegmentMeta {
				sequence,
				parts: vec![part(1.0, true)],
				duration: 1.0,
				complete: true,
				discontinuity: false,
			})
			.collect();
		let snapshot = Snapshot {
			init_ready: true,
			part_target: 1.0,
			target_duration: 1,
			media_sequence: 0,
			discontinuity_sequence: 0,
			next_sequence: 10,
			segments,
			finished: false,
		};

		let out = render_media(&snapshot);

		// TARGETDURATION is 1, so only the last 3 seconds of media keep their parts.
		assert!(!out.contains("part/6/0.m4s"));
		assert!(out.contains("part/7/0.m4s"));
		assert!(out.contains("part/9/0.m4s"));
		// Every segment is still listed and playable, parts or not.
		assert!(out.contains("\nseg/0.m4s\n"));
		assert!(out.contains("\nseg/9.m4s\n"));
	}

	#[test]
	fn emits_discontinuity_sequence_when_nonzero() {
		let snapshot = Snapshot {
			init_ready: true,
			part_target: 1.0,
			target_duration: 6,
			media_sequence: 8,
			discontinuity_sequence: 2,
			next_sequence: 9,
			segments: vec![SegmentMeta {
				sequence: 8,
				parts: vec![part(1.0, true)],
				duration: 1.0,
				complete: true,
				discontinuity: false,
			}],
			finished: false,
		};

		let out = render_media(&snapshot);

		assert!(out.contains("#EXT-X-TARGETDURATION:6\n"));
		assert!(out.contains("#EXT-X-DISCONTINUITY-SEQUENCE:2\n"));
	}
}
