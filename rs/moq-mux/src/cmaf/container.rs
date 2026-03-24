use std::task::Poll;

use bytes::Bytes;

use crate::cmaf::CmafError;
use crate::container::Container;
use crate::frame::{Frame, Timestamp};

pub(crate) fn cmaf_decode(data: Bytes, timescale: u64) -> Result<Frame, CmafError> {
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

pub(crate) fn cmaf_encode(
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
				data_offset: Some(0), // placeholder
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
				data_offset: Some(data_offset),
				entries: vec![mp4_atom::TrunEntry {
					size: Some(frame.payload.len() as u32),
					..Default::default()
				}],
			}],
			..Default::default()
		}],
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

impl Container for mp4_atom::Trak {
	type Error = CmafError;

	fn write(&self, group: &mut moq_lite::GroupProducer, frame: &Frame) -> Result<(), Self::Error> {
		let timescale = self.mdia.mdhd.timescale as u64;
		let track_id = self.tkhd.track_id;
		cmaf_encode(group, frame, timescale, track_id, 0)
	}

	fn poll_read(
		&self,
		group: &mut moq_lite::GroupConsumer,
		waiter: &conducer::Waiter,
	) -> Poll<Result<Option<Frame>, Self::Error>> {
		use std::task::ready;

		let Some(data) = ready!(group.poll_read_frame(waiter).map_err(CmafError::from)?) else {
			return Poll::Ready(Ok(None));
		};

		let timescale = self.mdia.mdhd.timescale as u64;
		Poll::Ready(Ok(Some(cmaf_decode(data, timescale)?)))
	}
}

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

impl Container for hang::catalog::VideoConfig {
	type Error = CmafError;

	fn write(&self, group: &mut moq_lite::GroupProducer, frame: &Frame) -> Result<(), Self::Error> {
		match &self.container {
			hang::catalog::Container::Legacy => crate::hang::Legacy.write(group, frame).map_err(CmafError::from),
			hang::catalog::Container::Cmaf { timescale, track_id } => {
				cmaf_encode(group, frame, *timescale, *track_id, 0)
			}
		}
	}

	fn poll_read(
		&self,
		group: &mut moq_lite::GroupConsumer,
		waiter: &conducer::Waiter,
	) -> Poll<Result<Option<Frame>, Self::Error>> {
		match &self.container {
			hang::catalog::Container::Legacy => crate::hang::Legacy
				.poll_read(group, waiter)
				.map(|r| r.map_err(CmafError::from)),
			hang::catalog::Container::Cmaf { timescale, .. } => {
				use std::task::ready;

				let Some(data) = ready!(group.poll_read_frame(waiter).map_err(CmafError::from)?) else {
					return Poll::Ready(Ok(None));
				};

				Poll::Ready(cmaf_decode(data, *timescale).map(Some))
			}
		}
	}
}

impl Container for hang::catalog::AudioConfig {
	type Error = CmafError;

	fn write(&self, group: &mut moq_lite::GroupProducer, frame: &Frame) -> Result<(), Self::Error> {
		match &self.container {
			hang::catalog::Container::Legacy => crate::hang::Legacy.write(group, frame).map_err(CmafError::from),
			hang::catalog::Container::Cmaf { timescale, track_id } => {
				cmaf_encode(group, frame, *timescale, *track_id, 0)
			}
		}
	}

	fn poll_read(
		&self,
		group: &mut moq_lite::GroupConsumer,
		waiter: &conducer::Waiter,
	) -> Poll<Result<Option<Frame>, Self::Error>> {
		match &self.container {
			hang::catalog::Container::Legacy => crate::hang::Legacy
				.poll_read(group, waiter)
				.map(|r| r.map_err(CmafError::from)),
			hang::catalog::Container::Cmaf { timescale, .. } => {
				use std::task::ready;

				let Some(data) = ready!(group.poll_read_frame(waiter).map_err(CmafError::from)?) else {
					return Poll::Ready(Ok(None));
				};

				Poll::Ready(cmaf_decode(data, *timescale).map(Some))
			}
		}
	}
}
