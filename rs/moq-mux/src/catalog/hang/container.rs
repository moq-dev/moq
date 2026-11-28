use crate::container::{Container as ContainerTrait, Frame, Kind, cts, fmp4, legacy, loc, raw};

/// Runtime-dispatched wire format for a track described by a hang catalog.
///
/// Built from a track's audio or video configuration, including its container.
pub enum Container {
	/// varint timestamp + raw codec bitstream. The original hang wire format. Its frames carry no
	/// duration, so audio built from [`Kind`] alone reads none.
	Legacy(Kind),
	/// [`Legacy`](Self::Legacy) audio whose codec states each packet's duration, which its reads
	/// carry. Built from an [`AudioConfig`](hang::catalog::AudioConfig) whose codec does.
	LegacyAudio(legacy::Audio),
	/// ISO-BMFF moof+mdat fragments. The wrapped [`fmp4::Wire`] holds
	/// the track's `trak` box so per-frame writes and reads have the
	/// timescale and track id available.
	Cmaf(fmp4::Wire),
	/// Low Overhead Container. One LOC frame per moq frame.
	Loc(Kind),
	/// Zero-byte header: raw codec bitstream timed by the MoQ transport timestamp (DTS == PTS).
	Raw(Kind),
	/// [`Raw`](Self::Raw) audio whose codec states each packet's duration.
	RawAudio(legacy::Audio),
	/// QUIC varint composition time offset (in track timescale) + raw codec bitstream, timed by
	/// the MoQ transport timestamp as its decode time.
	Cts(Kind),
	/// [`Cts`](Self::Cts) audio whose codec states each packet's duration.
	CtsAudio(legacy::Audio),
}

impl Container {
	/// Configure a wire format for an explicitly named media role.
	pub fn new(container: &hang::catalog::Container, kind: Kind) -> crate::Result<Self> {
		match container {
			hang::catalog::Container::Legacy => Ok(Self::Legacy(kind)),
			hang::catalog::Container::Cmaf { init, .. } => Ok(Self::Cmaf(fmp4::Wire::from_init(init)?)),
			hang::catalog::Container::Loc => Ok(Self::Loc(kind)),
			hang::catalog::Container::Raw => Ok(Self::Raw(kind)),
			hang::catalog::Container::Cts => Ok(Self::Cts(kind)),
			hang::catalog::Container::Unknown(unknown) => Err(crate::Error::unsupported_container(unknown)),
		}
	}
}

impl TryFrom<&hang::catalog::VideoConfig> for Container {
	type Error = crate::Error;
	fn try_from(config: &hang::catalog::VideoConfig) -> crate::Result<Self> {
		Self::new(&config.container, Kind::Video)
	}
}

impl TryFrom<&hang::catalog::AudioConfig> for Container {
	type Error = crate::Error;
	fn try_from(config: &hang::catalog::AudioConfig) -> crate::Result<Self> {
		match (&config.container, legacy::Audio::new(config)) {
			(hang::catalog::Container::Legacy, Some(audio)) => Ok(Self::LegacyAudio(audio)),
			(hang::catalog::Container::Raw, Some(audio)) => Ok(Self::RawAudio(audio)),
			(hang::catalog::Container::Cts, Some(audio)) => Ok(Self::CtsAudio(audio)),
			(container, _) => Self::new(container, Kind::Audio),
		}
	}
}

impl TryFrom<&crate::catalog::VideoHint> for Container {
	type Error = crate::Error;
	fn try_from(hint: &crate::catalog::VideoHint) -> crate::Result<Self> {
		Self::new(&hint.container, Kind::Video)
	}
}

impl Container {
	/// The bytes preceding a frame's payload, when they depend on nothing but its timestamp, so the
	/// payload can stream in after them. `None` for CMAF and CTS.
	pub(crate) fn prefix(&self, timestamp: moq_net::Timestamp) -> crate::Result<Option<bytes::Bytes>> {
		match self {
			Self::Legacy(kind) => legacy::Wire(*kind).prefix(timestamp).map(Some),
			Self::LegacyAudio(_) => legacy::Wire(Kind::Audio).prefix(timestamp).map(Some),
			Self::Loc(kind) => loc::Wire(*kind).prefix(timestamp).map(Some),
			Self::Raw(kind) => Ok(Some(raw::Wire(*kind).prefix())),
			Self::RawAudio(_) => Ok(Some(raw::Wire(Kind::Audio).prefix())),
			Self::Cmaf(_) | Self::Cts(_) | Self::CtsAudio(_) => Ok(None),
		}
	}
}

/// Whether a rendition's frames can be parsed by this build, logging the ones dropped.
///
/// The hang spec requires a consumer to ignore a rendition whose container `kind` it does not
/// recognize, so filter on this instead of failing the entire broadcast.
pub(crate) fn supported(rendition: &str, container: &hang::catalog::Container) -> bool {
	match container {
		hang::catalog::Container::Unknown(unknown) => {
			tracing::warn!(rendition, kind = unknown.kind(), "ignoring unknown container");
			false
		}
		_ => true,
	}
}

impl ContainerTrait for Container {
	type Error = crate::Error;

	fn end(&self, frame: &Frame) -> Option<moq_net::Timestamp> {
		match self {
			Self::Legacy(kind) => legacy::Wire(*kind).end(frame),
			Self::LegacyAudio(_) => legacy::Wire(Kind::Audio).end(frame),
			Self::Cmaf(cmaf) => cmaf.end(frame),
			Self::Loc(kind) => loc::Wire(*kind).end(frame),
			Self::Raw(kind) => raw::Wire(*kind).end(frame),
			Self::RawAudio(_) => raw::Wire(Kind::Audio).end(frame),
			Self::Cts(kind) => cts::Wire(*kind).end(frame),
			Self::CtsAudio(_) => cts::Wire(Kind::Audio).end(frame),
		}
	}

	fn kind(&self) -> Kind {
		match self {
			Self::Legacy(kind) | Self::Loc(kind) | Self::Raw(kind) | Self::Cts(kind) => *kind,
			Self::LegacyAudio(_) | Self::RawAudio(_) | Self::CtsAudio(_) => Kind::Audio,
			Self::Cmaf(wire) => <fmp4::Wire as ContainerTrait>::kind(wire),
		}
	}

	fn writable(&self) -> Result<(), Self::Error> {
		match self {
			Self::Cts(kind) => cts::Wire(*kind).writable(),
			Self::CtsAudio(_) => cts::Wire(Kind::Audio).writable(),
			_ => Ok(()),
		}
	}

	fn decode_order(&self) -> bool {
		matches!(
			self,
			Self::Raw(_) | Self::RawAudio(_) | Self::Cts(_) | Self::CtsAudio(_)
		)
	}

	fn check(&self, frame: &Frame) -> Result<(), Self::Error> {
		match self {
			Self::Raw(kind) => raw::Wire(*kind).check(frame),
			Self::RawAudio(_) => raw::Wire(Kind::Audio).check(frame),
			Self::Cts(kind) => cts::Wire(*kind).check(frame),
			Self::CtsAudio(_) => cts::Wire(Kind::Audio).check(frame),
			_ => Ok(()),
		}
	}

	fn finish_group(
		&self,
		group: &mut crate::container::Writer<'_>,
		end: Option<moq_net::Timestamp>,
	) -> Result<(), Self::Error> {
		match self {
			Self::Legacy(kind) => legacy::Wire(*kind).finish_group(group, end),
			Self::LegacyAudio(_) => legacy::Wire(Kind::Audio).finish_group(group, end),
			Self::Cmaf(wire) => wire.finish_group(group, end).map_err(Into::into),
			Self::Loc(kind) => loc::Wire(*kind).finish_group(group, end),
			Self::Raw(kind) => raw::Wire(*kind).finish_group(group, end),
			Self::RawAudio(_) => raw::Wire(Kind::Audio).finish_group(group, end),
			Self::Cts(kind) => cts::Wire(*kind).finish_group(group, end),
			Self::CtsAudio(_) => cts::Wire(Kind::Audio).finish_group(group, end),
		}
	}

	fn write(&self, group: &mut crate::container::Writer<'_>, frames: &[Frame]) -> Result<(), Self::Error> {
		match self {
			Self::Legacy(kind) => legacy::Wire(*kind).write(group, frames),
			Self::LegacyAudio(_) => legacy::Wire(Kind::Audio).write(group, frames),
			Self::Cmaf(cmaf) => cmaf.write(group, frames).map_err(Into::into),
			Self::Loc(kind) => loc::Wire(*kind).write(group, frames),
			Self::Raw(kind) => raw::Wire(*kind).write(group, frames),
			Self::RawAudio(_) => raw::Wire(Kind::Audio).write(group, frames),
			Self::Cts(kind) => cts::Wire(*kind).write(group, frames),
			Self::CtsAudio(_) => cts::Wire(Kind::Audio).write(group, frames),
		}
	}

	fn decode(&self, frame: moq_net::frame::Frame) -> Result<Vec<Frame>, Self::Error> {
		match self {
			Self::Legacy(kind) => legacy::Wire(*kind).decode(frame),
			Self::LegacyAudio(audio) => audio.decode(frame),
			Self::Cmaf(cmaf) => Ok(cmaf.decode(frame)?),
			Self::Loc(kind) => loc::Wire(*kind).decode(frame),
			Self::Raw(kind) => raw::Wire(*kind).decode(frame),
			Self::RawAudio(audio) => raw::decode_audio(audio, frame),
			Self::Cts(kind) => cts::Wire(*kind).decode(frame),
			Self::CtsAudio(audio) => cts::decode_audio(audio, frame),
		}
	}
}
