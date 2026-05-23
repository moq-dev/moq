use std::task::Poll;

use crate::container::{Container as ContainerTrait, Frame, fmp4, legacy, loc};

/// Runtime-dispatched wire format for a track described by a hang catalog.
///
/// Built from a [`hang::catalog::Container`] entry. Lets callers carry one
/// concrete type through their pipeline — [`Consumer<Container>`](crate::container::Consumer),
/// [`Producer<Container>`](crate::container::Producer) — instead of
/// threading a generic parameter everywhere.
pub enum Container {
	/// VarInt timestamp + raw codec bitstream. The original hang wire format.
	Legacy,
	/// ISO-BMFF moof+mdat fragments. The wrapped [`fmp4::Wire`] holds
	/// the track's `trak` box so per-frame writes and reads have the
	/// timescale and track id available.
	Cmaf(fmp4::Wire),
	/// Low Overhead Container. One LOC frame per moq frame.
	Loc,
}

impl TryFrom<&hang::catalog::Container> for Container {
	type Error = crate::Error;

	fn try_from(container: &hang::catalog::Container) -> Result<Self, Self::Error> {
		match container {
			hang::catalog::Container::Legacy => Ok(Self::Legacy),
			hang::catalog::Container::Cmaf { init, .. } => Ok(Self::Cmaf(fmp4::Wire::from_init(init)?)),
			hang::catalog::Container::Loc => Ok(Self::Loc),
		}
	}
}

impl ContainerTrait for Container {
	type Error = crate::Error;

	fn write(&self, group: &mut moq_net::GroupProducer, frames: &[Frame]) -> Result<(), Self::Error> {
		match self {
			Self::Legacy => legacy::Wire.write(group, frames),
			Self::Cmaf(cmaf) => cmaf.write(group, frames).map_err(Into::into),
			Self::Loc => loc::Wire.write(group, frames),
		}
	}

	fn poll_read(
		&self,
		group: &mut moq_net::GroupConsumer,
		waiter: &conducer::Waiter,
	) -> Poll<Result<Option<Vec<Frame>>, Self::Error>> {
		match self {
			Self::Legacy => legacy::Wire.poll_read(group, waiter),
			Self::Cmaf(cmaf) => cmaf.poll_read(group, waiter).map(|r| r.map_err(Into::into)),
			Self::Loc => loc::Wire.poll_read(group, waiter),
		}
	}
}
