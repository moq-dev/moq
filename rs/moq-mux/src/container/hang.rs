use std::task::Poll;

use crate::container::{Container, Frame, fmp4, legacy::Legacy, loc};

/// Catalog-driven [`Container`] for the hang protocol.
///
/// Runtime-dispatched union over the wire formats — Legacy, CMAF, LOC —
/// so callers can carry a single concrete type through their pipeline
/// (e.g. [`Consumer<Hang>`](crate::container::Consumer)) instead of
/// threading a generic parameter through user code.
///
/// Build from a catalog entry with `Hang::try_from(&container)`.
pub enum Hang {
	/// VarInt timestamp + raw codec bitstream. The original hang wire format.
	Legacy,
	/// ISO-BMFF moof+mdat fragments. Holds a parsed [`fmp4::Fragment`] so
	/// per-frame writes/reads know the timescale and track id.
	Cmaf(fmp4::Fragment),
	/// Low Overhead Container. Each moq frame holds one LOC frame.
	Loc(loc::Frame),
}

impl TryFrom<&hang::catalog::Container> for Hang {
	type Error = crate::Error;

	fn try_from(container: &hang::catalog::Container) -> Result<Self, Self::Error> {
		match container {
			hang::catalog::Container::Legacy => Ok(Self::Legacy),
			hang::catalog::Container::Cmaf { init, .. } => Ok(Self::Cmaf(fmp4::Fragment::from_init(init)?)),
			hang::catalog::Container::Loc => Ok(Self::Loc(loc::Frame::new())),
		}
	}
}

impl Container for Hang {
	type Error = crate::Error;

	fn write(&self, group: &mut moq_net::GroupProducer, frames: &[Frame]) -> Result<(), Self::Error> {
		match self {
			Self::Legacy => Legacy.write(group, frames),
			Self::Cmaf(cmaf) => cmaf.write(group, frames).map_err(Into::into),
			Self::Loc(loc) => loc.write(group, frames),
		}
	}

	fn poll_read(
		&self,
		group: &mut moq_net::GroupConsumer,
		waiter: &conducer::Waiter,
	) -> Poll<Result<Option<Vec<Frame>>, Self::Error>> {
		match self {
			Self::Legacy => Legacy.poll_read(group, waiter),
			Self::Cmaf(cmaf) => cmaf.poll_read(group, waiter).map(|r| r.map_err(Into::into)),
			Self::Loc(loc) => loc.poll_read(group, waiter),
		}
	}
}
