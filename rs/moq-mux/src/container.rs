use std::task::Poll;

use bytes::{Bytes, BytesMut};

pub type Timestamp = moq_lite::Timescale<1_000_000>;

/// A media frame with a timestamp and codec-specific payload.
#[derive(Clone, Debug)]
pub struct Frame {
	/// The presentation timestamp for this frame.
	pub timestamp: Timestamp,

	/// The encoded media data for this frame.
	pub payload: Bytes,
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

/// Trait for reading/writing media frames from/to moq-lite groups.
///
/// Different container formats encode timestamps and payloads differently:
/// - Legacy (hang): VarInt timestamp prefix + raw codec bitstream
/// - CMAF: moof+mdat atoms with timestamp in tfdt
pub trait Container {
	type Error: Into<Error>;

	/// Write a frame to a group.
	fn write(&self, group: &mut moq_lite::GroupProducer, frame: &Frame) -> Result<(), Self::Error>;

	/// Poll-read the next frame from a group. Returns None when the group is finished.
	fn poll_read(
		&self,
		group: &mut moq_lite::GroupConsumer,
		waiter: &conducer::Waiter,
	) -> Poll<Result<Option<Frame>, Self::Error>>;
}

/// hang Legacy format: VarInt timestamp prefix + raw codec bitstream.
pub struct Legacy;

impl Container for Legacy {
	type Error = moq_lite::Error;

	fn write(&self, group: &mut moq_lite::GroupProducer, frame: &Frame) -> Result<(), Self::Error> {
		let mut header = BytesMut::new();
		frame.timestamp.encode(&mut header).map_err(moq_lite::Error::from)?;

		let size = header.len() + frame.payload.len();
		let mut writer = group.create_frame(size.into())?;
		writer.write(header.freeze())?;
		writer.write(frame.payload.clone())?;
		writer.finish()?;

		Ok(())
	}

	fn poll_read(
		&self,
		group: &mut moq_lite::GroupConsumer,
		waiter: &conducer::Waiter,
	) -> Poll<Result<Option<Frame>, Self::Error>> {
		use std::task::ready;

		let Some(data) = ready!(group.poll_read_frame(waiter)?) else {
			return Poll::Ready(Ok(None));
		};

		let mut buf = data.as_ref();
		let timestamp = Timestamp::decode(&mut buf)?;
		let payload = data.slice((data.len() - buf.len())..);

		Poll::Ready(Ok(Some(Frame { timestamp, payload })))
	}
}

/// CMAF format: moof+mdat atoms with timestamp in tfdt.
#[cfg(feature = "mp4")]
#[derive(Debug, thiserror::Error)]
pub enum CmafError {
	#[error("mp4: {0}")]
	Mp4(#[from] mp4_atom::Error),

	#[error("moq: {0}")]
	Moq(#[from] moq_lite::Error),

	#[error("timestamp overflow")]
	TimestampOverflow(#[from] moq_lite::TimeOverflow),

	#[error("no traf in moof")]
	NoTraf,

	#[error("no tfdt in traf")]
	NoTfdt,

	#[error("no moof found in CMAF frame data")]
	NoMoof,

	#[error("no mdat found in CMAF frame data")]
	NoMdat,

	#[error("no tracks in moov")]
	NoTracks,

	#[error("multiple tracks in moov, use Trak instead")]
	MultipleTracks,
}

#[cfg(feature = "mp4")]
impl From<CmafError> for Error {
	fn from(e: CmafError) -> Self {
		match e {
			CmafError::Mp4(e) => Error::Mp4(e),
			CmafError::Moq(e) => Error::Moq(e),
			e => Error::Other(e.to_string()),
		}
	}
}

#[cfg(feature = "mp4")]
fn cmaf_decode(data: Bytes, timescale: u64) -> Result<Frame, CmafError> {
	use mp4_atom::DecodeMaybe;

	let mut cursor = std::io::Cursor::new(&data);
	let mut timestamp = None;
	let mut mdat_payload = None;

	while let Some(atom) = mp4_atom::Any::decode_maybe(&mut cursor)? {
		match atom {
			mp4_atom::Any::Moof(moof) => {
				let traf = moof.traf.first().ok_or(CmafError::NoTraf)?;
				let tfdt = traf.tfdt.as_ref().ok_or(CmafError::NoTfdt)?;
				timestamp = Some(Timestamp::from_scale(tfdt.base_media_decode_time, timescale)?);
			}
			mp4_atom::Any::Mdat(mdat) => {
				mdat_payload = Some(Bytes::from(mdat.data));
			}
			_ => {}
		}
	}

	let timestamp = timestamp.ok_or(CmafError::NoMoof)?;
	let payload = mdat_payload.ok_or(CmafError::NoMdat)?;

	Ok(Frame { timestamp, payload })
}

#[cfg(feature = "mp4")]
fn cmaf_encode(
	group: &mut moq_lite::GroupProducer,
	frame: &Frame,
	timescale: u64,
	track_id: u32,
	sequence_number: u32,
) -> Result<(), CmafError> {
	use mp4_atom::Encode;

	let dts = frame.timestamp.as_micros() as u64 * timescale / 1_000_000;

	let moof = mp4_atom::Moof {
		mfhd: mp4_atom::Mfhd { sequence_number },
		traf: vec![mp4_atom::Traf {
			tfhd: mp4_atom::Tfhd {
				track_id,
				..Default::default()
			},
			tfdt: Some(mp4_atom::Tfdt {
				base_media_decode_time: dts,
			}),
			trun: vec![mp4_atom::Trun {
				data_offset: Some(0), // placeholder, will be fixed
				entries: vec![mp4_atom::TrunEntry {
					size: Some(frame.payload.len() as u32),
					..Default::default()
				}],
			}],
			..Default::default()
		}],
	};

	// First pass: calculate moof size
	let mut buf = Vec::new();
	moof.encode(&mut buf)?;
	let moof_size = buf.len();

	// Second pass: set data_offset to point past moof + mdat header (8 bytes)
	let data_offset = (moof_size + 8) as i32;
	let moof = mp4_atom::Moof {
		traf: vec![mp4_atom::Traf {
			trun: vec![mp4_atom::Trun {
				data_offset: Some(data_offset),
				..moof.traf.into_iter().next().unwrap().trun.into_iter().next().unwrap()
			}],
			..mp4_atom::Traf {
				tfhd: mp4_atom::Tfhd {
					track_id,
					..Default::default()
				},
				tfdt: Some(mp4_atom::Tfdt {
					base_media_decode_time: dts,
				}),
				..Default::default()
			}
		}],
		..mp4_atom::Moof {
			mfhd: mp4_atom::Mfhd { sequence_number },
			..Default::default()
		}
	};

	buf.clear();
	moof.encode(&mut buf)?;

	let mdat = mp4_atom::Mdat {
		data: frame.payload.to_vec(),
	};
	mdat.encode(&mut buf)?;

	let total_size = buf.len();
	let mut writer = group.create_frame(total_size.into())?;
	writer.write(Bytes::from(buf))?;
	writer.finish()?;

	Ok(())
}

#[cfg(feature = "mp4")]
impl Container for mp4_atom::Trak {
	type Error = CmafError;

	fn write(&self, group: &mut moq_lite::GroupProducer, frame: &Frame) -> Result<(), Self::Error> {
		let timescale = self.mdia.mdhd.timescale as u64;
		let track_id = self.tkhd.track_id;
		// Use group sequence as a simple sequence number
		cmaf_encode(group, frame, timescale, track_id, 0)
	}

	fn poll_read(
		&self,
		group: &mut moq_lite::GroupConsumer,
		waiter: &conducer::Waiter,
	) -> Poll<Result<Option<Frame>, Self::Error>> {
		use std::task::ready;

		let Some(data) = ready!(group.poll_read_frame(waiter)?) else {
			return Poll::Ready(Ok(None));
		};

		let timescale = self.mdia.mdhd.timescale as u64;
		Poll::Ready(Ok(Some(cmaf_decode(data, timescale)?)))
	}
}

#[cfg(feature = "mp4")]
impl Container for mp4_atom::Moov {
	type Error = CmafError;

	fn write(&self, group: &mut moq_lite::GroupProducer, frame: &Frame) -> Result<(), Self::Error> {
		let trak = match self.trak.as_slice() {
			[trak] => trak,
			[] => return Err(CmafError::NoTracks),
			_ => return Err(CmafError::MultipleTracks),
		};
		trak.write(group, frame)
	}

	fn poll_read(
		&self,
		group: &mut moq_lite::GroupConsumer,
		waiter: &conducer::Waiter,
	) -> Poll<Result<Option<Frame>, Self::Error>> {
		let trak = match self.trak.as_slice() {
			[trak] => trak,
			[] => return Poll::Ready(Err(CmafError::NoTracks)),
			_ => return Poll::Ready(Err(CmafError::MultipleTracks)),
		};
		trak.poll_read(group, waiter)
	}
}

#[cfg(feature = "mp4")]
impl Container for hang::catalog::VideoConfig {
	type Error = Error;

	fn write(&self, group: &mut moq_lite::GroupProducer, frame: &Frame) -> Result<(), Self::Error> {
		match &self.container {
			hang::catalog::Container::Legacy => Legacy.write(group, frame).map_err(Into::into),
			hang::catalog::Container::Cmaf { timescale, track_id } => {
				cmaf_encode(group, frame, *timescale, *track_id, 0).map_err(Into::into)
			}
		}
	}

	fn poll_read(
		&self,
		group: &mut moq_lite::GroupConsumer,
		waiter: &conducer::Waiter,
	) -> Poll<Result<Option<Frame>, Self::Error>> {
		match &self.container {
			hang::catalog::Container::Legacy => Legacy.poll_read(group, waiter).map(|r| r.map_err(Into::into)),
			hang::catalog::Container::Cmaf { timescale, .. } => {
				use std::task::ready;

				let Some(data) = ready!(group.poll_read_frame(waiter).map(|r| r)?) else {
					return Poll::Ready(Ok(None));
				};

				Poll::Ready(cmaf_decode(data, *timescale).map(Some).map_err(Into::into))
			}
		}
	}
}

#[cfg(feature = "mp4")]
impl Container for hang::catalog::AudioConfig {
	type Error = Error;

	fn write(&self, group: &mut moq_lite::GroupProducer, frame: &Frame) -> Result<(), Self::Error> {
		match &self.container {
			hang::catalog::Container::Legacy => Legacy.write(group, frame).map_err(Into::into),
			hang::catalog::Container::Cmaf { timescale, track_id } => {
				cmaf_encode(group, frame, *timescale, *track_id, 0).map_err(Into::into)
			}
		}
	}

	fn poll_read(
		&self,
		group: &mut moq_lite::GroupConsumer,
		waiter: &conducer::Waiter,
	) -> Poll<Result<Option<Frame>, Self::Error>> {
		match &self.container {
			hang::catalog::Container::Legacy => Legacy.poll_read(group, waiter).map(|r| r.map_err(Into::into)),
			hang::catalog::Container::Cmaf { timescale, .. } => {
				use std::task::ready;

				let Some(data) = ready!(group.poll_read_frame(waiter).map(|r| r)?) else {
					return Poll::Ready(Ok(None));
				};

				Poll::Ready(cmaf_decode(data, *timescale).map(Some).map_err(Into::into))
			}
		}
	}
}
