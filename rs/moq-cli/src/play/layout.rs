//! Where the video sits inside the window.

/// The viewport (x, y, width, height) for a `video`-sized picture letterboxed
/// into a `window`-sized surface, scaled to fit without stretching.
pub(super) fn fit(window: (u32, u32), video: (u32, u32)) -> (f32, f32, f32, f32) {
	let scale = (window.0 as f32 / video.0 as f32).min(window.1 as f32 / video.1 as f32);
	let width = video.0 as f32 * scale;
	let height = video.1 as f32 * scale;
	(
		(window.0 as f32 - width) / 2.0,
		(window.1 as f32 - height) / 2.0,
		width,
		height,
	)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn letterboxes_without_changing_aspect_ratio() {
		let assert_near = |actual: (f32, f32, f32, f32), expected: (f32, f32, f32, f32)| {
			for (actual, expected) in [actual.0, actual.1, actual.2, actual.3]
				.into_iter()
				.zip([expected.0, expected.1, expected.2, expected.3])
			{
				assert!((actual - expected).abs() < 0.01, "{actual} != {expected}");
			}
		};
		assert_near(fit((1000, 1000), (1920, 1080)), (0.0, 218.75, 1000.0, 562.5));
		assert_near(fit((1920, 1080), (1000, 1000)), (420.0, 0.0, 1080.0, 1080.0));
	}
}
