//! What the peer declared in its SETUP, and the slot the session tasks read it from.

use super::cluster;

/// The Setup Options the peer sent us.
///
/// One value per session: both extensions ride the same SETUP, so they arrive together
/// and are read together.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Peer {
	/// The MoQ Cluster options: the peer's Hop ID and what it charges to cross.
	pub cluster: cluster::Peer,

	/// MoQ Solicit: whether the peer requires advertisements to be solicited, so an
	/// unsolicited PUBLISH_NAMESPACE is unwanted. `None` when it declared nothing, which
	/// is the one case where sending us one anyway is not a protocol violation.
	pub solicit: Option<bool>,
}

/// Shared slot for [`Peer`], filled when the peer's SETUP is read.
///
/// The publisher blocks on this before its first advertisement: the SETUP decides both
/// what an advertisement carries (MoQ Cluster) and whether one may be sent unasked (MoQ
/// Solicit), so nothing can go out until it arrives. Cheap to clone; every handle
/// shares the same slot.
#[derive(Clone, Default)]
pub(crate) struct PeerSetup(kio::Shared<Option<Peer>>);

impl PeerSetup {
	/// Record what the peer declared. A SETUP carrying no options records the default
	/// (no extension negotiated, no requirements), which is what unblocks a waiter.
	///
	/// First write wins. The announce loops read this once and hold it for their
	/// lifetime while subscription serving re-reads it, so letting a later SETUP
	/// overwrite the identity would advertise under one exclusion and serve under
	/// another, which is how a routing loop gets back in.
	pub fn set(&self, peer: Peer) {
		let mut slot = self.0.lock();
		if slot.is_none() {
			*slot = Some(peer);
		}
	}

	/// Await the peer's SETUP.
	///
	/// The peer MUST send exactly one, so this resolves once that stream is read. Waits
	/// forever if it never does; the caller is a session task, cancelled when the driver
	/// drops.
	pub async fn get(&self) -> Peer {
		let slot = self
			.0
			.wait(|peer| match peer.is_some() {
				true => std::task::Poll::Ready(()),
				false => std::task::Poll::Pending,
			})
			.await;
		(*slot).expect("waited for Some")
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// The announce loops read the peer's declaration once and hold it, while
	/// subscription serving re-reads it. A second SETUP overwriting the identity would
	/// split those two apart, so the first write is the one that counts.
	#[tokio::test]
	async fn first_write_wins() {
		let first = Peer {
			cluster: cluster::Peer {
				origin: Some(crate::Origin::new(42).unwrap()),
				cost: Some(3),
			},
			solicit: None,
		};

		let slot = PeerSetup::default();
		slot.set(first);
		slot.set(Peer {
			cluster: cluster::Peer {
				origin: Some(crate::Origin::new(99).unwrap()),
				cost: Some(0),
			},
			solicit: Some(true),
		});

		assert_eq!(slot.get().await, first);
	}
}
