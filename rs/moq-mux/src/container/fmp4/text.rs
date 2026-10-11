use moq_net::{Timescale, Timestamp};
use mp4_atom::{Any, DecodeMaybe};

use super::{Error, sample};
use crate::container::text::{self, Cue};

pub(super) struct Track {
	pub producer: crate::container::Producer<crate::catalog::hang::Container, hang::catalog::TextConfig>,
	pub header: String,
	pub defaults: sample::Defaults,
	pub timescale: Timescale,
	pub last_decode_time: Option<u64>,
	pub discontinuity: bool,
}

pub(super) struct Fragment<'a> {
	pub traf: &'a mp4_atom::Traf,
	pub data: &'a [u8],
	pub moof_size: usize,
	pub header_size: usize,
}

impl Track {
	pub fn extract<E: crate::catalog::hang::CatalogExt>(
		&mut self,
		input: Fragment<'_>,
		timebase: &mut crate::catalog::Timebase<E>,
	) -> crate::Result<()> {
		let traf = input.traf;
		let mut dts = traf.tfdt.as_ref().ok_or(Error::MissingTfdt)?.base_media_decode_time;
		if let Some(previous) = self.last_decode_time
			&& dts <= previous
		{
			return Err(Error::NonMonotonicDecodeTime {
				track: traf.tfhd.track_id,
				decode_time: Timestamp::new(dts, self.timescale)?,
				previous: Timestamp::new(previous, self.timescale)?,
			}
			.into());
		}
		if traf.trun.is_empty() {
			return Err(Error::MissingTrun.into());
		}
		self.last_decode_time = Some(dts);
		if self.discontinuity {
			self.producer.discontinuity()?;
			self.discontinuity = false;
		}
		let base: usize = traf
			.tfhd
			.base_data_offset
			.unwrap_or_default()
			.try_into()
			.map_err(|_| Error::InvalidDataOffset)?;
		let mut offset = base;
		for trun in &traf.trun {
			if let Some(value) = trun.data_offset {
				let value: usize = value.try_into().map_err(|_| Error::InvalidDataOffset)?;
				offset = value
					.checked_sub(input.moof_size)
					.and_then(|value| value.checked_sub(input.header_size))
					.and_then(|value| value.checked_add(base))
					.ok_or(Error::InvalidDataOffset)?;
			}
			for entry in &trun.entries {
				let sample = self.defaults.resolve(&traf.tfhd, entry);
				let duration = sample.duration.ok_or(Error::MissingSampleDuration)?;
				let pts = dts
					.checked_add_signed(entry.cts.unwrap_or_default() as i64)
					.ok_or(Error::PtsOverflow)?;
				let timestamp = timebase.shift(Timestamp::new(pts, self.timescale)?)?;
				let duration = Timestamp::new(duration as u64, self.timescale)?;
				let end = timestamp.checked_add(duration)?;
				let next = offset
					.checked_add(sample.size as usize)
					.ok_or(Error::InvalidDataOffset)?;
				let data = input.data.get(offset..next).ok_or(Error::InvalidDataOffset)?;
				let payload = self.sample(data, timestamp, end)?;
				self.producer.write(crate::container::Frame {
					timestamp,
					duration: Some(duration),
					keyframe: true,
					payload,
				})?;
				self.producer.cut(None)?;
				offset = next;
				dts = dts.checked_add(duration.value()).ok_or(Error::PtsOverflow)?;
			}
		}
		Ok(())
	}

	fn sample(&self, mut data: &[u8], start: Timestamp, end: Timestamp) -> crate::Result<bytes::Bytes> {
		if data.is_empty() {
			return Err(Error::InvalidSubtitle("empty WebVTT sample").into());
		}
		let mut output = self.header.clone();
		while !data.is_empty() {
			let atom = Any::decode_maybe(&mut data)
				.map_err(Error::from)?
				.ok_or(Error::InvalidSubtitle("truncated WebVTT sample box"))?;
			let Any::Unknown(kind, body) = atom else {
				return Err(Error::InvalidSubtitle("unsupported WebVTT sample box").into());
			};
			match kind.as_ref() {
				b"vtte" if body.is_empty() => {}
				b"vttc" => {
					let mut body = body.as_slice();
					let mut payload = None;
					let mut id = None;
					let mut settings = None;
					while !body.is_empty() {
						let atom = Any::decode_maybe(&mut body)
							.map_err(Error::from)?
							.ok_or(Error::InvalidSubtitle("truncated WebVTT cue box"))?;
						let Any::Unknown(kind, value) = atom else {
							return Err(Error::InvalidSubtitle("unsupported WebVTT cue box").into());
						};
						let field = match kind.as_ref() {
							b"payl" => &mut payload,
							b"iden" => &mut id,
							b"sttg" => &mut settings,
							_ => return Err(Error::InvalidSubtitle("unsupported WebVTT cue box").into()),
						};
						if field.is_some() {
							return Err(Error::InvalidSubtitle("duplicate WebVTT cue box").into());
						}
						*field =
							Some(String::from_utf8(value).map_err(|_| Error::InvalidSubtitle("invalid WebVTT UTF-8"))?);
					}
					text::append(
						&mut output,
						Cue {
							start,
							end,
							body: payload
								.as_deref()
								.ok_or(Error::InvalidSubtitle("missing WebVTT cue payload"))?,
							id: id.as_deref().unwrap_or_default(),
							settings: settings.as_deref().unwrap_or_default(),
						},
					)
					.map_err(Error::InvalidSubtitle)?;
				}
				_ => return Err(Error::InvalidSubtitle("unsupported WebVTT sample box").into()),
			}
		}
		Ok(output.into())
	}
}
