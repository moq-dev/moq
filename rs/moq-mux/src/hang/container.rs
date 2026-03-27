use std::task::Poll;

use bytes::Buf;

use crate::container::{Container, Frame};

/// hang Legacy format: VarInt timestamp prefix + raw codec bitstream.
///
/// Each moq-lite frame contains exactly one media frame.
pub struct Legacy;

impl Container for Legacy {
	type Error = hang::Error;

	fn write(&self, group: &mut moq_lite::GroupProducer, frames: &[Frame]) -> Result<(), Self::Error> {
		for frame in frames {
			let hang_frame = hang::container::Frame {
				timestamp: frame.timestamp,
				payload: frame.payload.clone().into(),
			};
			hang_frame.encode(group)?;
		}
		Ok(())
	}

	fn poll_read(
		&self,
		group: &mut moq_lite::GroupConsumer,
		waiter: &conducer::Waiter,
	) -> Poll<Result<Option<Vec<Frame>>, Self::Error>> {
		use std::task::ready;

		let Some(data) = ready!(group.poll_read_frame(waiter).map_err(hang::Error::from)?) else {
			return Poll::Ready(Ok(None));
		};

		let mut hang_frame = hang::container::Frame::decode(data)?;
		let payload = hang_frame.payload.copy_to_bytes(hang_frame.payload.remaining());

		Poll::Ready(Ok(Some(vec![Frame {
			timestamp: hang_frame.timestamp,
			payload,
			keyframe: false, // Legacy can't determine from data; consumer infers from group position
		}])))
	}
}

/// A container that dispatches between Legacy and CMAF based on the catalog config.
///
/// Constructed from a `VideoConfig` or `AudioConfig`, parsing init_data once upfront.
pub enum Media {
	Legacy,
	#[cfg(feature = "mp4")]
	Cmaf(Box<mp4_atom::Moov>),
}

#[cfg(feature = "mp4")]
impl TryFrom<&hang::catalog::Container> for Media {
	type Error = crate::Error;

	fn try_from(container: &hang::catalog::Container) -> Result<Self, Self::Error> {
		use base64::Engine;
		use mp4_atom::DecodeMaybe;

		match container {
			hang::catalog::Container::Legacy => Ok(Self::Legacy),
			hang::catalog::Container::Cmaf { init_data } => {
				let init_bytes = base64::engine::general_purpose::STANDARD
					.decode(init_data)
					.map_err(|e| crate::cmaf::Error::Mp4(mp4_atom::Error::Io(std::io::Error::other(e))))?;

				let mut cursor = std::io::Cursor::new(&init_bytes);
				while let Some(atom) = mp4_atom::Any::decode_maybe(&mut cursor).map_err(crate::cmaf::Error::from)? {
					if let mp4_atom::Any::Moov(moov) = atom {
						return Ok(Self::Cmaf(Box::new(moov)));
					}
				}

				Err(crate::cmaf::Error::NoMoov.into())
			}
		}
	}
}

impl Container for Media {
	type Error = crate::Error;

	fn write(&self, group: &mut moq_lite::GroupProducer, frames: &[Frame]) -> Result<(), Self::Error> {
		match self {
			Self::Legacy => Legacy.write(group, frames).map_err(Into::into),
			#[cfg(feature = "mp4")]
			Self::Cmaf(moov) => moov.write(group, frames).map_err(Into::into),
		}
	}

	fn poll_read(
		&self,
		group: &mut moq_lite::GroupConsumer,
		waiter: &conducer::Waiter,
	) -> Poll<Result<Option<Vec<Frame>>, Self::Error>> {
		match self {
			Self::Legacy => Legacy.poll_read(group, waiter).map(|r| r.map_err(Into::into)),
			#[cfg(feature = "mp4")]
			Self::Cmaf(moov) => moov.poll_read(group, waiter).map(|r| r.map_err(Into::into)),
		}
	}
}
