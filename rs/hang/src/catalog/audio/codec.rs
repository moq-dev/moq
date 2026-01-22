use crate::Error;

use super::*;
use derive_more::{Display, From};
use std::str::FromStr;

/// Supported audio codec mimetypes.
#[derive(Debug, Clone, PartialEq, Eq, Display, From)]
#[non_exhaustive]
pub enum AudioCodec {
	/// AAC codec with profile information
	AAC(AAC),

	/// Opus codec (no mimetype parameters)
	#[display("opus")]
	Opus,

	/// Unknown or unsupported codec with original string
	#[display("{_0}")]
	Unknown(String),
}

impl FromStr for AudioCodec {
	type Err = Error;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		if s.starts_with("mp4a.40.") {
			return AAC::from_str(s).map(Into::into);
		} else if s == "opus" {
			return Ok(Self::Opus);
		}

		Ok(Self::Unknown(s.to_string()))
	}
}
