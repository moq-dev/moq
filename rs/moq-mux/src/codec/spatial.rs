//! The catalog projection every spherical video source resolves to.
//!
//! The codec bitstream never signals a projection or stereo packing; containers do (ISOBMFF's
//! Spherical Video V2 `sv3d` and `st3d` boxes, Matroska's `Projection` and `StereoMode`). An absent
//! catalog field claims flat, mono video, so an importer refuses what the catalog cannot describe
//! rather than dropping it.

use hang::catalog::Projection;

/// The equirectangular projection a Spherical Video V2 `equi` box's bounds describe.
///
/// Each bound is the 0.32 fixed-point fraction of the projection its edge leaves uncovered. The
/// catalog's `fov` is a longitude span centred straight ahead over the full 180 degrees of
/// latitude, so asymmetric left and right bounds, which would recentre the picture, and any top or
/// bottom crop are refused. Full coverage leaves `fov` absent, which means 360 degrees.
pub(crate) fn equirectangular(top: u32, bottom: u32, left: u32, right: u32) -> Result<Projection, &'static str> {
	if top != 0 || bottom != 0 {
		return Err("equirectangular latitude crop");
	}
	if left != right {
		return Err("asymmetric equirectangular bounds");
	}
	// Both sides together must leave some longitude.
	if left >= 1 << 31 {
		return Err("equirectangular bounds leave no picture");
	}
	let fov = (left != 0).then(|| 360.0 * (1.0 - f64::from(left) / f64::from(1u32 << 31)));
	Ok(Projection::Equirectangular { fov })
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn bounds_set_the_field_of_view() {
		assert_eq!(
			equirectangular(0, 0, 0, 0),
			Ok(Projection::Equirectangular { fov: None })
		);
		let quarter = 1 << 30;
		assert_eq!(
			equirectangular(0, 0, quarter, quarter),
			Ok(Projection::Equirectangular { fov: Some(180.0) })
		);
		let Ok(Projection::Equirectangular { fov: Some(fov) }) = equirectangular(0, 0, (1 << 31) - 1, (1 << 31) - 1)
		else {
			panic!("expected a sliver");
		};
		assert!(fov > 0.0, "{fov}");
	}

	#[test]
	fn uncentred_or_cropped_bounds_are_refused() {
		let quarter = 1 << 30;
		equirectangular(0, 0, quarter, 0).unwrap_err();
		equirectangular(1, 0, 0, 0).unwrap_err();
		equirectangular(0, quarter, quarter, quarter).unwrap_err();
		equirectangular(0, 0, 1 << 31, 1 << 31).unwrap_err();
		equirectangular(0, 0, u32::MAX, u32::MAX).unwrap_err();
	}
}
