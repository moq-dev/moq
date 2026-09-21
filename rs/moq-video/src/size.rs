use crate::{Error, Rate};

/// A frame resolution in pixels.
///
/// Names the pair that [`decode::Config::scale_hint`](crate::decode::Config::scale_hint)
/// and [`Frame::resize`](crate::Frame::resize) both take, so
/// width and height can't be swapped at a call site.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Size {
	/// Width in pixels.
	pub width: u32,
	/// Height in pixels.
	pub height: u32,
}

impl Size {
	/// A size of `width` x `height` pixels.
	pub fn new(width: u32, height: u32) -> Self {
		Self { width, height }
	}

	/// Total pixels. Can't overflow: the widest `u32` square still fits a `u64`.
	pub fn pixels(&self) -> u64 {
		self.width as u64 * self.height as u64
	}

	pub(crate) fn byte_len(&self, bytes_per_pixel: usize, what: &str) -> Result<usize, Error> {
		usize::try_from(self.pixels())
			.ok()
			.and_then(|pixels| pixels.checked_mul(bytes_per_pixel))
			.ok_or_else(|| Error::Codec(anyhow::anyhow!("{what} {self}: byte length is too large to represent")))
	}

	/// Reject anything the I420 pipeline can't represent.
	///
	/// I420 chroma is subsampled 2x2, so every stage (encode, decode, resize)
	/// needs even, non-zero dimensions. Checking here keeps the rule in one place
	/// instead of re-deriving it at each boundary.
	pub(crate) fn validate(&self, what: &str) -> Result<(), Error> {
		self.validate_nonzero(what)?;
		if !self.width.is_multiple_of(2) || !self.height.is_multiple_of(2) {
			return Err(Error::Codec(anyhow::anyhow!("{what} {self}: dimensions must be even")));
		}
		Ok(())
	}

	/// Reject a size whose derived quantities cannot be computed at all.
	///
	/// Dimensions arrive as a pair of `u32`, so their product reaches ~1.8e19
	/// before anything else looks at them. That overflows the two things every
	/// encoder derives from a size: the default bitrate (pixels x framerate) and
	/// a packed frame's byte count (up to 4 bytes per pixel for RGBA). Both would
	/// panic, which for a binding is an aborted host process rather than an
	/// error return, so refuse the config here instead.
	///
	/// This is arithmetic, not policy: it rejects only what cannot be represented,
	/// leaving "no encoder handles a frame that large" to the backend.
	pub(crate) fn validate_encodable(&self, what: &str, framerate: Rate) -> Result<(), Error> {
		let pixels = self.pixels();
		let representable = u128::from(pixels)
			.checked_mul(u128::from(framerate.numerator()))
			.is_some_and(|value| value / u128::from(framerate.denominator()) <= u128::from(u64::MAX))
			&& usize::try_from(pixels).is_ok_and(|pixels| pixels.checked_mul(4).is_some());

		if !representable {
			return Err(Error::Codec(anyhow::anyhow!(
				"{what} {self} at {framerate}fps: dimensions too large to represent"
			)));
		}
		Ok(())
	}

	/// The half of [`Size::validate`] that is not about chroma, for the surfaces
	/// that hold RGB and so tolerate odd dimensions (a render target sized to a
	/// window).
	pub(crate) fn validate_nonzero(&self, what: &str) -> Result<(), Error> {
		if self.width == 0 || self.height == 0 {
			return Err(Error::Codec(anyhow::anyhow!(
				"{what} {self}: dimensions must be non-zero"
			)));
		}
		Ok(())
	}
}

impl std::fmt::Display for Size {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{}x{}", self.width, self.height)
	}
}

impl From<(u32, u32)> for Size {
	fn from((width, height): (u32, u32)) -> Self {
		Self::new(width, height)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn validate_rejects_odd_and_zero() {
		assert!(Size::new(320, 240).validate("frame").is_ok());
		assert!(Size::new(0, 240).validate("frame").is_err());
		assert!(Size::new(320, 0).validate("frame").is_err());
		assert!(Size::new(321, 240).validate("frame").is_err());
		assert!(Size::new(320, 241).validate("frame").is_err());
	}

	/// Regression: `u32` dimensions can reach a pixel count whose derived
	/// quantities overflow. A binding hands these straight through, and a panic
	/// there aborts the host process, so the size has to be refused first.
	#[test]
	fn validate_encodable_rejects_unrepresentable_sizes() {
		// A frame nobody can encode, but whose arithmetic still fits: the backend
		// decides, not us.
		let rate = Rate::new(30, 1).unwrap();
		assert!(Size::new(65534, 65534).validate_encodable("frame", rate).is_ok());

		// pixels x framerate overflows u64.
		assert!(
			Size::new(u32::MAX - 1, u32::MAX - 1)
				.validate_encodable("frame", rate)
				.is_err()
		);
		// ...and it is the product that matters, not either side alone.
		assert!(Size::new(u32::MAX - 1, 2).validate_encodable("frame", rate).is_ok());
		let extreme = Rate::new(crate::rate::MAX_FRAMES_PER_SECOND, 1).unwrap();
		assert!(
			Size::new(u32::MAX - 1, u32::MAX - 1)
				.validate_encodable("frame", extreme)
				.is_err()
		);
	}

	#[test]
	fn display_reads_as_a_resolution() {
		assert_eq!(Size::new(1920, 1080).to_string(), "1920x1080");
	}
}
