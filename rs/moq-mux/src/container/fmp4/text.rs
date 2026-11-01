use moq_net::{Timescale, Timestamp};
use mp4_atom::{Any, DecodeMaybe};

use super::{Error, edit::Edit, sample};
use crate::container::text::{self, Cue};
use crate::container::ttml;

/// How a track's samples are serialized.
pub(super) enum Format {
	/// WebVTT (`wvtt`) cue boxes, prefixed with the sample entry's header.
	Vtt { header: String },
	/// One TTML document per sample (`stpp`).
	Ttml,
}

/// Whether an `stpp` sample entry body (ISO/IEC 14496-12 XMLSubtitleSampleEntry) carries TTML text.
///
/// Image subsamples (auxiliary MIME types) and namespaces other than TTML are unsupported.
pub(super) fn stpp(body: &[u8]) -> Result<bool, Error> {
	let (header, strings) = body
		.split_at_checked(8)
		.ok_or(Error::InvalidSubtitle("truncated stpp sample entry"))?;
	if header[6..] != [0, 1] {
		return Err(Error::InvalidSubtitle("external subtitle data reference"));
	}
	// namespace, schema_location, auxiliary_mime_types; encoders may omit the trailing ones.
	let mut strings = strings.split(|&byte| byte == 0);
	let namespace = strings.next().unwrap_or_default();
	let namespace = std::str::from_utf8(namespace).map_err(|_| Error::InvalidSubtitle("invalid stpp namespace"))?;
	let _schema_location = strings.next();
	let mime = strings.next().unwrap_or_default();
	Ok(namespace.split_whitespace().any(|name| name == ttml::NAMESPACE) && mime.is_empty())
}

pub(super) struct Track {
	pub producer: crate::container::Producer<crate::catalog::hang::Container, hang::catalog::TextConfig>,
	pub format: Format,
	pub defaults: sample::Defaults,
	pub timescale: Timescale,
	/// The edit list, applied to every cue's presentation time.
	pub edit: Edit,
	/// The exclusive decode end of the previous fragment; the next one may not start before it.
	pub end_decode_time: Option<u64>,
	pub discontinuity: bool,
}

pub(super) struct Fragment<'a> {
	pub traf: &'a mp4_atom::Traf,
	pub data: &'a [u8],
	/// Resolve sample ranges shared with the moof's other tracks.
	pub resolver: &'a mut sample::Resolver,
}

impl Track {
	pub fn extract<E: crate::catalog::hang::CatalogExt>(
		&mut self,
		input: Fragment<'_>,
		timebase: &mut crate::catalog::Timebase<E>,
	) -> crate::Result<()> {
		let traf = input.traf;
		let mut dts = traf.tfdt.as_ref().ok_or(Error::MissingTfdt)?.base_media_decode_time;
		if self.end_decode_time.is_some_and(|end| dts < end) {
			return Err(Error::InvalidSubtitle("subtitle fragment overlaps the previous one").into());
		}
		if traf.trun.is_empty() {
			return Err(Error::MissingTrun.into());
		}
		if self.discontinuity {
			self.producer.discontinuity()?;
			self.discontinuity = false;
		}
		for resolved in input.resolver.resolve(traf, self.defaults)? {
			let sample::Resolved { sample, cts, range } = resolved?;
			let duration = sample.duration.ok_or(Error::MissingSampleDuration)?;
			let pts = dts.checked_add_signed(cts).ok_or(Error::PtsOverflow)?;
			let duration = Timestamp::new(duration as u64, self.timescale)?;
			let data = &input.data[range];
			dts = dts.checked_add(duration.value()).ok_or(Error::PtsOverflow)?;

			// A cue starting before the edit is dropped, like a media sample.
			let Some(presented) = self.edit.present(pts)? else {
				continue;
			};
			// TTML maps the document's media times from `source`, so they move with the edit too.
			let source = Timestamp::new(pts, self.timescale)?;
			let timestamp = timebase.shift(Timestamp::new(presented, self.timescale)?)?;
			let end = timestamp.checked_add(duration)?;
			let payload = match &self.format {
				Format::Vtt { header } => vtt(header, data, timestamp, end)?,
				// The frame timestamp is serialized in hang's scale, which the body's begin must match.
				Format::Ttml => ttml::rewrite(
					data,
					source,
					timestamp.convert(hang::container::TIMESCALE)?..end.convert(hang::container::TIMESCALE)?,
				)
				.map_err(Error::InvalidSubtitle)?
				.into(),
			};
			self.producer.write(crate::container::Frame {
				timestamp,
				duration: Some(duration),
				keyframe: true,
				payload,
			})?;
			self.producer.cut(None)?;
		}
		self.end_decode_time = Some(dts);
		Ok(())
	}
}

fn vtt(header: &str, mut data: &[u8], start: Timestamp, end: Timestamp) -> crate::Result<bytes::Bytes> {
	if data.is_empty() {
		return Err(Error::InvalidSubtitle("empty WebVTT sample").into());
	}
	let mut output = header.to_string();
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
				let mut origin = None;
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
						b"ctim" => &mut origin,
						_ => return Err(Error::InvalidSubtitle("unsupported WebVTT cue box").into()),
					};
					if field.is_some() {
						return Err(Error::InvalidSubtitle("duplicate WebVTT cue box").into());
					}
					*field =
						Some(String::from_utf8(value).map_err(|_| Error::InvalidSubtitle("invalid WebVTT UTF-8"))?);
				}
				let payload = payload.ok_or(Error::InvalidSubtitle("missing WebVTT cue payload"))?;
				// Inline timestamps sit on the source timeline, which only `ctim` anchors.
				let payload = if let Some(origin) = origin {
					let origin = text::timestamp(&origin).map_err(Error::InvalidSubtitle)?;
					text::rebase(&payload, start..end, origin).map_err(Error::InvalidSubtitle)?
				} else if text::has_timestamp(&payload) {
					return Err(Error::InvalidSubtitle("inline WebVTT timestamp has no cue origin").into());
				} else {
					payload
				};
				text::append(
					&mut output,
					Cue {
						start,
						end,
						body: &payload,
						id: id.as_deref().unwrap_or_default(),
						settings: settings.as_deref().unwrap_or_default(),
					},
				)
				.map_err(Error::InvalidSubtitle)?;
			}
			b"vtta" => {
				let value = std::str::from_utf8(&body).map_err(|_| Error::InvalidSubtitle("invalid WebVTT UTF-8"))?;
				if !value.is_empty() {
					text::comments(&mut output, value).map_err(Error::InvalidSubtitle)?;
				}
			}
			_ => return Err(Error::InvalidSubtitle("unsupported WebVTT sample box").into()),
		}
	}
	Ok(output.into())
}
