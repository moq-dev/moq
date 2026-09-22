use bitflags::bitflags;
use std::{fmt, mem};

use crate::fraction::Fraction;
use crate::parameters::Capabilities;
use crate::sys::*;

bitflags! {
	#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
	pub struct Modes: u32 {
		const HIGH_QUALITY      = 0x1000;
	}
}

impl From<u32> for Modes {
	fn from(caps: u32) -> Self {
		Self::from_bits_truncate(caps)
	}
}

impl From<Modes> for u32 {
	fn from(modes: Modes) -> Self {
		modes.bits()
	}
}

impl fmt::Display for Modes {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		fmt::Debug::fmt(self, f)
	}
}

#[derive(Debug, Copy, Clone)]
/// Streaming parameters (single-planar)
pub struct Parameters {
	pub capabilities: Capabilities,
	pub modes: Modes,
	pub interval: Fraction,
}

impl Parameters {
	pub fn new(frac: Fraction) -> Self {
		Parameters {
			capabilities: Capabilities::from(0),
			modes: Modes::from(0),
			interval: frac,
		}
	}

	pub fn with_fps(fps: u32) -> Self {
		Parameters {
			capabilities: Capabilities::from(0),
			modes: Modes::from(0),
			interval: Fraction::new(1, fps),
		}
	}
}

impl fmt::Display for Parameters {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		writeln!(f, "capabilities : {}", self.capabilities)?;
		writeln!(f, "modes        : {}", self.modes)?;
		writeln!(f, "interval     : {} [s]", self.interval)?;
		Ok(())
	}
}

impl From<v4l2_captureparm> for Parameters {
	fn from(params: v4l2_captureparm) -> Self {
		Self {
			capabilities: Capabilities::from(params.capability),
			modes: Modes::from(params.capturemode),
			interval: Fraction::from(params.timeperframe),
		}
	}
}

impl From<Parameters> for v4l2_captureparm {
	fn from(parameters: Parameters) -> Self {
		Self {
			capability: parameters.capabilities.into(),
			capturemode: parameters.modes.into(),
			timeperframe: parameters.interval.into(),
			..unsafe { mem::zeroed() }
		}
	}
}
