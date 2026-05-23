use std::task::Poll;

use crate::container::{Container, Frame, fmp4::Cmaf, legacy::Legacy, loc::Loc};

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
/// - [`Hang::Loc`]: Low Overhead Container (draft-ietf-moq-loc). One media frame per
///   moq-lite frame, with a small property block in front of the codec bitstream.
///
/// Build from a catalog entry with `Hang::try_from(&container)`.
pub enum Hang {
	/// VarInt timestamp prefix + raw codec bitstream. One media frame per moq-lite frame.
	Legacy(Legacy),
	/// CMAF moof+mdat fragments. Wraps a parsed [`Cmaf`] (the track's `trak` box from the
	/// init segment) so per-frame writes/reads have the timescale and track id available.
	Cmaf(Cmaf),
	/// Low Overhead Container. Frame timestamps use microseconds when the
	/// per-frame 0x08 timescale property is absent.
	Loc(Loc),
}

impl TryFrom<&hang::catalog::Container> for Hang {
	type Error = crate::Error;

	fn try_from(container: &hang::catalog::Container) -> Result<Self, Self::Error> {
		match container {
			hang::catalog::Container::Legacy => Ok(Self::Legacy(Legacy::new())),
			hang::catalog::Container::Cmaf { init, .. } => Ok(Self::Cmaf(Cmaf::from_init(init)?)),
			hang::catalog::Container::Loc => Ok(Self::Loc(Loc::new())),
		}
	}
}

impl Container for Hang {
	type Error = crate::Error;

	fn write(&self, group: &mut moq_net::GroupProducer, frames: &[Frame]) -> Result<(), Self::Error> {
		match self {
			Self::Legacy(l) => l.write(group, frames),
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
			Self::Legacy(l) => l.poll_read(group, waiter),
			Self::Cmaf(cmaf) => cmaf.poll_read(group, waiter).map(|r| r.map_err(Into::into)),
			Self::Loc(loc) => loc.poll_read(group, waiter),
		}
	}
}
