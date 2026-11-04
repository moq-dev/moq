use crate::container::{Container as ContainerTrait, Frame, Kind, fmp4, legacy, loc};

/// Runtime-dispatched wire format for a track described by a hang catalog.
///
/// Built from a track's audio or video configuration, including its container.
pub enum Container {
	/// varint timestamp + raw codec bitstream. The original hang wire format.
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

impl Container {
	/// The bytes preceding a frame's payload, when they depend on nothing but its timestamp, so the
	/// payload can stream in after them. `None` for CMAF, whose `moof` describes the payload.
	pub(crate) fn prefix(&self, timestamp: moq_net::Timestamp) -> crate::Result<Option<bytes::Bytes>> {
		match self {
			Self::Legacy(kind) => legacy::Wire(*kind).prefix(timestamp).map(Some),
			Self::Loc(kind) => loc::Wire(*kind).prefix(timestamp).map(Some),
			Self::Cmaf(_) => Ok(None),
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

/// Drop the protected video and audio renditions, logging each one (see [`protected`]).
///
/// Their samples are encrypted, and the hang spec has a consumer that cannot decrypt a rendition
/// treat it as one it cannot decode. Nothing here decrypts, and a re-encoded file would also lose
/// the `pssh`, `senc`, `saiz`, and `saio` a decrypting player needs, so every export, player, and
/// binding in this workspace runs this on each catalog snapshot instead of handing out
/// undecryptable media. A rendition is logged as a warning the first time and at debug level after.
pub fn retain_clear<E>(catalog: &mut hang::Catalog<E>) {
	catalog
		.video
		.renditions
		.retain(|name, config| clear("video", name, &config.drm, &config.container));
	catalog
		.audio
		.renditions
		.retain(|name, config| clear("audio", name, &config.drm, &config.container));
}

/// Whether a rendition's samples are encrypted: it lists `drm` entries, or its CMAF init holds an
/// encrypted (`encv`/`enca`) sample entry.
///
/// The init check catches a publisher that encrypts without signalling it, so a consumer never
/// reads such a rendition as clear and decodes ciphertext.
pub fn protected(drm: &[String], container: &hang::catalog::Container) -> bool {
	if !drm.is_empty() {
		return true;
	}
	match container {
		hang::catalog::Container::Cmaf { init, .. } => fmp4::cenc::init_sinf(init).is_some(),
		_ => false,
	}
}

fn clear(kind: &'static str, rendition: &str, drm: &[String], container: &hang::catalog::Container) -> bool {
	if !protected(drm, container) {
		return true;
	}
	// Every catalog snapshot runs this, so warn only the first time a rendition is dropped.
	static WARNED: std::sync::Mutex<std::collections::BTreeSet<(&'static str, String)>> =
		std::sync::Mutex::new(std::collections::BTreeSet::new());
	let first = {
		let mut warned = WARNED.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
		// Bounded, so a publisher minting fresh names cannot grow it without limit.
		if warned.len() >= 1024 {
			warned.clear();
		}
		warned.insert((kind, rendition.to_string()))
	};
	match first {
		true => tracing::warn!(
			rendition,
			kind,
			"ignoring encrypted rendition: decryption is not supported"
		),
		false => tracing::debug!(
			rendition,
			kind,
			"ignoring encrypted rendition: decryption is not supported"
		),
	}
	false
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

	fn decode(&self, frame: moq_net::frame::Frame) -> Result<Vec<Frame>, Self::Error> {
		match self {
			Self::Legacy(kind) => legacy::Wire(*kind).decode(frame),
			Self::Cmaf(cmaf) => Ok(cmaf.decode(frame)?),
			Self::Loc(kind) => loc::Wire(*kind).decode(frame),
		}
	}
}
