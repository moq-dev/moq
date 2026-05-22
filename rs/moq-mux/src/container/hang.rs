use std::task::Poll;

use bytes::Buf;

use crate::container::{Cmaf, Container, Frame};

/// Catalog-driven [`Container`] for the hang protocol.
///
/// `Hang` is a runtime-dispatched [`Container`] that selects the wire format based on a
/// hang [`catalog::Container`](hang::catalog::Container). This lets callers carry a
/// single concrete type through their pipeline (e.g. [`Consumer<Hang>`](crate::container::Consumer))
/// instead of threading a generic parameter through user code.
///
/// - [`Hang::Legacy`]: VarInt timestamp prefix + raw codec bitstream, one media frame
///   per moq-lite frame. The original hang wire format.
/// - [`Hang::Cmaf`]: ISO-BMFF moof+mdat fragments, potentially multiple samples per
///   moq-lite frame. The contained [`Cmaf`] is parsed once from the catalog's init
///   segment via [`Cmaf::from_init`].
///
/// Build from a catalog entry with `Hang::try_from(&container)`.
pub enum Hang {
	/// VarInt timestamp prefix + raw codec bitstream. One media frame per moq-lite frame.
	Legacy,
	/// CMAF moof+mdat fragments. Wraps a parsed [`Cmaf`] (the track's `trak` box from the
	/// init segment) so per-frame writes/reads have the timescale and track id available.
	Cmaf(Cmaf),
}

impl TryFrom<&hang::catalog::Container> for Hang {
	type Error = crate::Error;

	fn try_from(container: &hang::catalog::Container) -> Result<Self, Self::Error> {
		match container {
			hang::catalog::Container::Legacy => Ok(Self::Legacy),
			hang::catalog::Container::Cmaf { init, .. } => Ok(Self::Cmaf(Cmaf::from_init(init)?)),
		}
	}
}

impl Container for Hang {
	type Error = crate::Error;

	fn write(&self, group: &mut moq_net::GroupProducer, frames: &[Frame]) -> Result<(), Self::Error> {
		match self {
			Self::Legacy => {
				for frame in frames {
					let hang_frame = hang::container::Frame {
						timestamp: frame.timestamp,
						payload: frame.payload.clone(),
					};
					hang_frame.encode(group)?;
				}
				Ok(())
			}
			Self::Cmaf(cmaf) => cmaf.write(group, frames).map_err(Into::into),
		}
	}

	fn poll_read(
		&self,
		group: &mut moq_net::GroupConsumer,
		waiter: &conducer::Waiter,
	) -> Poll<Result<Option<Vec<Frame>>, Self::Error>> {
		match self {
			Self::Legacy => {
				use std::task::ready;

				let Some(data) = ready!(group.poll_read_frame(waiter).map_err(hang::Error::from)?) else {
					return Poll::Ready(Ok(None));
				};

				let mut hang_frame = hang::container::Frame::decode(data)?;
				let payload = hang_frame.payload.copy_to_bytes(hang_frame.payload.remaining());

				Poll::Ready(Ok(Some(vec![Frame {
					timestamp: hang_frame.timestamp,
					payload,
					// Legacy can't determine from data; consumer infers from group position.
					keyframe: false,
				}])))
			}
			Self::Cmaf(cmaf) => cmaf.poll_read(group, waiter).map(|r| r.map_err(Into::into)),
		}
	}
}
