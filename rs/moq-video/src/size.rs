use crate::Error;

/// A frame resolution in pixels.
///
/// Names the pair that [`decode::Config::resize`](crate::decode::Config::resize)
/// and [`decode::Frame::resize`](crate::decode::Frame::resize) both take, so
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

	/// Reject anything the I420 pipeline can't represent.
	///
	/// I420 chroma is subsampled 2x2, so every stage (encode, decode, resize)
	/// needs even, non-zero dimensions. Checking here keeps the rule in one place
	/// instead of re-deriving it at each boundary.
	pub(crate) fn validate(&self, what: &str) -> Result<(), Error> {
		if self.width == 0 || self.height == 0 {
			return Err(Error::Codec(anyhow::anyhow!(
				"{what} {self}: dimensions must be non-zero"
			)));
		}
		if self.width % 2 != 0 || self.height % 2 != 0 {
			return Err(Error::Codec(anyhow::anyhow!("{what} {self}: dimensions must be even")));
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

	#[test]
	fn display_reads_as_a_resolution() {
		assert_eq!(Size::new(1920, 1080).to_string(), "1920x1080");
	}
}
