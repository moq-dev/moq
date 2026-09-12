use std::task::{Poll, ready};

use crate::container::{Container as ContainerTrait, Frame, Kind, fmp4, legacy, loc};

/// Runtime-dispatched wire format for a track described by a hang catalog.
///
/// Built from a track's audio or video configuration, including its container.
pub enum Container {
	/// VarInt timestamp + raw codec bitstream. The original hang wire format.
	Legacy(Kind),
	/// ISO-BMFF moof+mdat fragments. The wrapped [`fmp4::Wire`] holds
	/// the track's `trak` box so per-frame writes and reads have the
	/// timescale and track id available.
	Cmaf(fmp4::Wire),
	/// Low Overhead Container. One LOC frame per moq frame.
	Loc(Kind),
}

impl Container {
	/// Configure a wire format for an explicitly named media role.
	pub fn new(container: &hang::catalog::Container, kind: Kind) -> crate::Result<Self> {
		match container {
			hang::catalog::Container::Legacy => Ok(Self::Legacy(kind)),
			hang::catalog::Container::Cmaf { init, .. } => Ok(Self::Cmaf(fmp4::Wire::from_init(init)?)),
			hang::catalog::Container::Loc => Ok(Self::Loc(kind)),
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
		Self::new(&config.container, Kind::Audio)
	}
}

impl TryFrom<&crate::catalog::VideoHint> for Container {
	type Error = crate::Error;
	fn try_from(hint: &crate::catalog::VideoHint) -> crate::Result<Self> {
		Self::new(&hint.container, Kind::Video)
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
			Self::Cmaf(cmaf) => cmaf.end(frame),
			Self::Loc(kind) => loc::Wire(*kind).end(frame),
		}
	}

	fn kind(&self) -> Kind {
		match self {
			Self::Legacy(kind) | Self::Loc(kind) => *kind,
			Self::Cmaf(wire) => <fmp4::Wire as ContainerTrait>::kind(wire),
		}
	}

	fn finish_group(
		&self,
		group: &mut moq_net::group::Producer,
		end: Option<moq_net::Timestamp>,
	) -> Result<(), Self::Error> {
		match self {
			Self::Legacy(kind) => legacy::Wire(*kind).finish_group(group, end),
			Self::Cmaf(wire) => wire.finish_group(group, end).map_err(Into::into),
			Self::Loc(kind) => loc::Wire(*kind).finish_group(group, end),
		}
	}

	fn write(&self, group: &mut moq_net::group::Producer, frames: &[Frame]) -> Result<(), Self::Error> {
		match self {
			Self::Legacy(kind) => legacy::Wire(*kind).write(group, frames),
			Self::Cmaf(cmaf) => cmaf.write(group, frames).map_err(Into::into),
			Self::Loc(kind) => loc::Wire(*kind).write(group, frames),
		}
	}

	fn poll_read(
		&self,
		group: &mut moq_net::group::Consumer,
		waiter: &kio::Waiter,
	) -> Poll<Result<Option<Vec<Frame>>, Self::Error>> {
		match self {
			Self::Legacy(kind) => legacy::Wire(*kind).poll_read(group, waiter),
			Self::Cmaf(cmaf) => Poll::Ready(Ok(ready!(cmaf.poll_read(group, waiter))?)),
			Self::Loc(kind) => loc::Wire(*kind).poll_read(group, waiter),
		}
	}
}
