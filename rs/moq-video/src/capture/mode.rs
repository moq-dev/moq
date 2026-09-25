//! Mode choice and listing shared by the Linux camera backends (V4L2 and
//! PipeWire), so both land on the same mode for the same request.

use std::collections::{BTreeMap, BTreeSet};

use super::{Mode, Rate};
use crate::Size;

/// What the caller asked the camera for.
#[derive(Clone, Copy, Debug)]
pub(super) struct Request {
	pub size: Size,
	pub framerate: Option<Rate>,
}

/// One mode a camera can stream, as the selection weighs it.
#[derive(Clone, Copy, Debug)]
pub(super) struct Candidate {
	pub size: Size,
	pub framerate: Option<Rate>,
	/// What converting the format to I420 costs: lower is cheaper.
	pub cost: u8,
}

/// Pick the candidate nearest `want`: geometry first, then the rate nearest the
/// request, then the cheaper conversion. A size that cannot feed I420 is never
/// picked, and among equals the earliest candidate wins.
pub(super) fn nearest<T>(
	candidates: impl IntoIterator<Item = T>,
	want: Request,
	candidate: impl Fn(&T) -> Candidate,
) -> Option<T> {
	candidates
		.into_iter()
		.filter(|item| candidate(item).size.validate("camera resolution").is_ok())
		.min_by(|left, right| {
			let (left, right) = (candidate(left), candidate(right));
			distance(left.size, want.size)
				.cmp(&distance(right.size, want.size))
				.then_with(|| rate_distance(left.framerate, right.framerate, want.framerate))
				.then_with(|| left.cost.cmp(&right.cost))
		})
}

fn rate_distance(left: Option<Rate>, right: Option<Rate>, want: Option<Rate>) -> std::cmp::Ordering {
	let Some(want) = want else {
		return std::cmp::Ordering::Equal;
	};
	match (left, right) {
		(Some(left), Some(right)) => (left.as_f64() - want.as_f64())
			.abs()
			.total_cmp(&(right.as_f64() - want.as_f64()).abs()),
		(Some(_), None) => std::cmp::Ordering::Less,
		(None, Some(_)) => std::cmp::Ordering::Greater,
		(None, None) => std::cmp::Ordering::Equal,
	}
}

/// How far a mode lands from the requested geometry, summed over both
/// dimensions. Zero is an exact match.
fn distance(size: Size, want: Size) -> u64 {
	u64::from(size.width.abs_diff(want.width)) + u64::from(size.height.abs_diff(want.height))
}

/// Merge the sizes and rates a camera reports into [`Mode`]s, largest first
/// with each mode's rates highest first. The encoder sees I420 whatever format
/// carried a mode, so the format is not something a caller can act on.
pub(super) fn modes(reported: impl IntoIterator<Item = (Size, Vec<Rate>)>) -> Vec<Mode> {
	let mut sizes: BTreeMap<(u32, u32), BTreeSet<Rate>> = BTreeMap::new();
	for (size, rates) in reported {
		sizes.entry((size.width, size.height)).or_default().extend(rates);
	}
	let mut modes: Vec<Mode> = sizes
		.into_iter()
		.map(|((width, height), framerates)| Mode {
			width,
			height,
			// Highest first, so the rate a caller most often wants is the one it
			// reads without scanning.
			framerates: framerates.into_iter().rev().collect(),
		})
		.collect();
	modes.sort_by_key(|mode| std::cmp::Reverse(u64::from(mode.width) * u64::from(mode.height)));
	modes
}

#[cfg(test)]
mod tests {
	use super::*;

	fn rate(fps: u32) -> Rate {
		Rate::new(fps, 1).unwrap()
	}

	#[test]
	fn rate_scoring_preserves_fractional_precision_and_handles_unknown_rates() {
		let ntsc = Some(Rate::new(60000, 1001).unwrap());
		let thirty = Some(rate(30));
		assert!(rate_distance(ntsc, thirty, Some(rate(60))).is_lt());
		assert!(rate_distance(thirty, ntsc, Some(rate(30))).is_lt());
		assert!(rate_distance(ntsc, None, Some(rate(60))).is_lt());
		assert!(rate_distance(ntsc, thirty, None).is_eq());
	}

	/// Distance is symmetric in the two dimensions and zero only on an exact hit,
	/// so a mode that overshoots is no better than one that undershoots by as much.
	#[test]
	fn distance_is_zero_only_on_an_exact_match() {
		let want = Size::new(1280, 720);
		assert_eq!(distance(Size::new(1280, 720), want), 0);
		assert_eq!(distance(Size::new(1280, 600), want), 120);
		assert_eq!(distance(Size::new(1280, 840), want), 120);
	}

	#[test]
	fn nearest_prefers_geometry_then_rate_then_cost_then_order() {
		let candidate = |width, height, fps: Option<u32>, cost| Candidate {
			size: Size::new(width, height),
			framerate: fps.map(rate),
			cost,
		};
		let want = |framerate| Request {
			size: Size::new(1280, 720),
			framerate,
		};
		let candidates = [
			candidate(640, 480, Some(30), 0),
			candidate(1280, 720, Some(30), 1),
			candidate(1280, 720, Some(15), 0),
			candidate(1280, 720, Some(30), 0),
			candidate(1281, 720, Some(30), 0),
		];
		let pick = |framerate| nearest(candidates, want(framerate), |c| *c).unwrap();

		let chosen = pick(Some(rate(30)));
		assert_eq!((chosen.framerate, chosen.cost), (Some(rate(30)), 0));
		// With no rate requested, the first of the cheapest exact sizes wins.
		assert_eq!(pick(None).framerate, Some(rate(15)));
		// An odd size is never picked, even when it is the only exact one.
		assert!(nearest([candidate(1281, 721, None, 0)], want(None), |c| *c).is_none());
	}

	#[test]
	fn modes_merge_sizes_and_sort_largest_and_fastest_first() {
		let modes = modes([
			(Size::new(640, 480), vec![rate(30)]),
			(Size::new(1280, 720), vec![rate(15)]),
			(Size::new(640, 480), vec![rate(60), rate(30)]),
		]);
		assert_eq!(
			modes,
			[
				Mode {
					width: 1280,
					height: 720,
					framerates: vec![rate(15)],
				},
				Mode {
					width: 640,
					height: 480,
					framerates: vec![rate(60), rate(30)],
				},
			]
		);
	}
}
