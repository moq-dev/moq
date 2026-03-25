use std::task::Poll;

use bytes::Bytes;

use super::Error;
use crate::container::{Container, Frame, Timestamp};

pub(crate) fn decode(data: Bytes, timescale: u64) -> Result<Vec<Frame>, Error> {
	use mp4_atom::DecodeMaybe;

	let mut cursor = std::io::Cursor::new(&data);
	let mut moof = None;
	let mut mdat_data = None;

	while let Some(atom) = mp4_atom::Any::decode_maybe(&mut cursor)? {
		match atom {
			mp4_atom::Any::Moof(m) => moof = Some(m),
			mp4_atom::Any::Mdat(m) => mdat_data = Some(m.data),
			_ => {}
		}
	}

	let moof = moof.ok_or(Error::NoMoof)?;
	let mdat_data = mdat_data.ok_or(Error::NoMdat)?;
	let traf = moof.traf.first().ok_or(Error::NoTraf)?;
	let tfdt = traf.tfdt.as_ref().ok_or(Error::NoTfdt)?;
	let base_dts = tfdt.base_media_decode_time;

	let default_size = traf.tfhd.default_sample_size;
	let default_duration = traf.tfhd.default_sample_duration;

	let mut frames = Vec::new();
	let mut offset = 0usize;
	let mut dts = base_dts;

	for trun in &traf.trun {
		for entry in &trun.entries {
			let size = entry.size.or(default_size).unwrap_or(0) as usize;
			let end = offset + size;

			if end > mdat_data.len() {
				return Ok(frames);
			}

			let pts = dts as i64 + entry.cts.unwrap_or(0) as i64;
			let timestamp = Timestamp::from_scale(pts.max(0) as u64, timescale)?;
			let payload = Bytes::copy_from_slice(&mdat_data[offset..end]);
			let flags = entry.flags.unwrap_or(0);
			// depends_on_no_other (bits 24-25 == 0x2) means keyframe
			let keyframe = (flags >> 24) & 0x3 == 0x2;

			frames.push(Frame {
				timestamp,
				payload,
				keyframe,
			});

			offset = end;
			dts += entry.duration.or(default_duration).unwrap_or(0) as u64;
		}
	}

	Ok(frames)
}

pub(crate) fn encode(
	group: &mut moq_lite::GroupProducer,
	frames: &[Frame],
	timescale: u64,
	track_id: u32,
) -> Result<(), Error> {
	use mp4_atom::Encode;

	if frames.is_empty() {
		return Ok(());
	}

	let dts = (frames[0].timestamp.as_micros() * timescale as u128 / 1_000_000) as u64;
	let sequence_number = group.frame_count() as u32;

	let entries: Vec<_> = frames
		.iter()
		.map(|f| {
			let flags = if f.keyframe { 0x0200_0000 } else { 0x0001_0000 };
			mp4_atom::TrunEntry {
				size: Some(f.payload.len() as u32),
				flags: Some(flags),
				..Default::default()
			}
		})
		.collect();

	let mdat_data: Vec<u8> = frames.iter().flat_map(|f| f.payload.iter().copied()).collect();

	let build_moof = |data_offset| mp4_atom::Moof {
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
				entries: entries.clone(),
			}],
			..Default::default()
		}],
	};

	// First pass: calculate moof size
	let mut buf = Vec::new();
	build_moof(0).encode(&mut buf)?;
	let moof_size = buf.len();

	// Second pass: set data_offset to point past moof + mdat header (8 bytes)
	buf.clear();
	build_moof((moof_size + 8) as i32).encode(&mut buf)?;

	let mdat = mp4_atom::Mdat { data: mdat_data };
	mdat.encode(&mut buf)?;

	let mut writer = group.create_frame(buf.len().into())?;
	writer.write(Bytes::from(buf))?;
	writer.finish()?;

	Ok(())
}

impl Container for mp4_atom::Trak {
	type Error = Error;

	fn write(&self, group: &mut moq_lite::GroupProducer, frames: &[Frame]) -> Result<(), Self::Error> {
		let timescale = self.mdia.mdhd.timescale as u64;
		let track_id = self.tkhd.track_id;
		encode(group, frames, timescale, track_id)
	}

	fn poll_read(
		&self,
		group: &mut moq_lite::GroupConsumer,
		waiter: &conducer::Waiter,
	) -> Poll<Result<Option<Vec<Frame>>, Self::Error>> {
		use std::task::ready;

		let Some(data) = ready!(group.poll_read_frame(waiter)?) else {
			return Poll::Ready(Ok(None));
		};

		let timescale = self.mdia.mdhd.timescale as u64;
		Poll::Ready(Ok(Some(decode(data, timescale)?)))
	}
}

impl Container for mp4_atom::Moov {
	type Error = Error;

	fn write(&self, group: &mut moq_lite::GroupProducer, frames: &[Frame]) -> Result<(), Self::Error> {
		let trak = match self.trak.as_slice() {
			[trak] => trak,
			[] => return Err(Error::NoTracks),
			_ => return Err(Error::MultipleTracks),
		};
		trak.write(group, frames)
	}

	fn poll_read(
		&self,
		group: &mut moq_lite::GroupConsumer,
		waiter: &conducer::Waiter,
	) -> Poll<Result<Option<Vec<Frame>>, Self::Error>> {
		let trak = match self.trak.as_slice() {
			[trak] => trak,
			[] => return Poll::Ready(Err(Error::NoTracks)),
			_ => return Poll::Ready(Err(Error::MultipleTracks)),
		};
		trak.poll_read(group, waiter)
	}
}
