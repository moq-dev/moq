use buf_list::BufList;

pub type Timestamp = moq_lite::Timescale<1_000_000>;

/// Trait for parsing container-formatted frame data.
///
/// Different container formats encode timestamps differently:
/// - Legacy (hang): VarInt timestamp prefix, stripped from payload
/// - CMAF: timestamp in moof tfdt, payload passed through unchanged
pub trait ContainerFormat {
	type Error: Into<Error>;

	/// Parse timestamp from raw frame data.
	///
	/// Returns (timestamp, payload) where payload may have the timestamp
	/// stripped (Legacy) or be the full original data (CMAF passthrough).
	fn parse(&self, payload: BufList) -> Result<(Timestamp, BufList), Self::Error>;
}

/// Errors from container format operations.
#[derive(Debug, thiserror::Error)]
pub enum Error {
	#[error("moq: {0}")]
	Moq(#[from] moq_lite::Error),

	#[cfg(feature = "mp4")]
	#[error("mp4: {0}")]
	Mp4(#[from] mp4_atom::Error),

	#[error("{0}")]
	Other(String),
}

/// hang Legacy format: VarInt timestamp prefix.
pub struct Legacy;

impl ContainerFormat for Legacy {
	type Error = moq_lite::Error;

	fn parse(&self, mut payload: BufList) -> Result<(Timestamp, BufList), Self::Error> {
		let timestamp = Timestamp::decode(&mut payload)?;
		Ok((timestamp, payload))
	}
}

/// CMAF format: parse moof tfdt for timestamp, return full moof+mdat unchanged.
#[cfg(feature = "mp4")]
pub struct Cmaf {
	pub timescale: u64,
}

#[cfg(feature = "mp4")]
#[derive(Debug, thiserror::Error)]
pub enum CmafError {
	#[error("mp4: {0}")]
	Mp4(#[from] mp4_atom::Error),

	#[error("timestamp overflow")]
	TimestampOverflow(#[from] moq_lite::TimeOverflow),

	#[error("no traf in moof")]
	NoTraf,

	#[error("no tfdt in traf")]
	NoTfdt,

	#[error("no moof found in CMAF frame data")]
	NoMoof,
}

#[cfg(feature = "mp4")]
impl From<CmafError> for Error {
	fn from(e: CmafError) -> Self {
		match e {
			CmafError::Mp4(e) => Error::Mp4(e),
			e => Error::Other(e.to_string()),
		}
	}
}

#[cfg(feature = "mp4")]
impl ContainerFormat for Cmaf {
	type Error = CmafError;

	fn parse(&self, payload: BufList) -> Result<(Timestamp, BufList), Self::Error> {
		use mp4_atom::DecodeMaybe;

		// Collect payload into contiguous bytes for parsing
		let data: Vec<u8> = payload.iter().flat_map(|c| c.iter().copied()).collect();
		let mut cursor = std::io::Cursor::new(&data);

		while let Some(atom) = mp4_atom::Any::decode_maybe(&mut cursor)? {
			if let mp4_atom::Any::Moof(moof) = atom {
				let traf = moof.traf.first().ok_or(CmafError::NoTraf)?;
				let tfdt = traf.tfdt.as_ref().ok_or(CmafError::NoTfdt)?;
				let timestamp = Timestamp::from_scale(tfdt.base_media_decode_time, self.timescale)?;
				return Ok((timestamp, BufList::from_iter(vec![bytes::Bytes::from(data)])));
			}
		}

		Err(CmafError::NoMoof)
	}
}
