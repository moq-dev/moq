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

	/// MoQ Hidden: whether the peer understands the HIDDEN parameter on
	/// SUBSCRIBE_NAMESPACE, so we may send it.
	pub hidden: bool,

	/// Whether the peer understands subscriber max age (MoQ Timestamp).
	pub max_age: bool,

	/// MoQ Auth: whether both sides negotiated the Auth request streams.
	pub auth: bool,

	/// MoQ Active Count: whether the REQUEST_OK answering SUBSCRIBE_NAMESPACE counts the
	/// NAMESPACE messages before the subscription is synced, in both directions.
	pub active_count: bool,

	/// MoQ Epoch: whether both sides offered it, so advertisements and requests carry the
	/// publisher's epoch in both directions.
	pub epoch: bool,

	/// Whether this peer offered DEFLATE-compressed tracks.
	pub flate: bool,
}

/// Shared slot for [`Peer`], filled when the peer's SETUP is read.
///
/// The publisher blocks on this before its first advertisement: the SETUP decides both
/// what an advertisement carries (MoQ Cluster) and whether one may be sent unasked (MoQ
/// Solicit), so nothing can go out until it arrives. Cheap to clone; every handle
/// shares the same slot.
#[derive(Clone, Default)]
pub(crate) struct PeerSetup {
	slot: kio::Shared<Option<Peer>>,
	/// What we offered, which an extension negotiated by both sides needs as well.
	declined_epoch: bool,
	/// Where the peer's advertised implementation is published for the session handle.
	implementation: crate::setup::PeerImplementation,
	/// The session's auth link to tell once the peer's SETUP accepts the connection.
	accept: Option<crate::auth::Link>,
}

impl PeerSetup {
	/// A fixture whose peer already sent its SETUP.
	#[cfg(test)]
	pub(crate) fn declared(peer: Peer) -> Self {
		let setup = Self::default();
		setup.set(peer);
		setup
	}

	/// A slot for a session that offered `offered`.
	pub fn new(offered: crate::setup::Extensions) -> Self {
		Self {
			slot: Default::default(),
			declined_epoch: !offered.epoch,
			implementation: Default::default(),
			accept: None,
		}
	}

	/// Tell `link` once the peer's SETUP arrives that it accepted the connection credential.
	pub fn accepting(mut self, link: crate::auth::Link) -> Self {
		link.await_setup();
		self.accept = Some(link);
		self
	}

	/// Publish the peer's advertised implementation to `slot` once its SETUP is read.
	pub fn with_implementation(mut self, slot: crate::setup::PeerImplementation) -> Self {
		self.implementation = slot;
		self
	}

	/// Record the implementation the peer's SETUP advertised.
	pub fn set_implementation(&self, value: Option<String>) {
		self.implementation.set(value);
	}

	/// Record what the peer declared. A SETUP carrying no options records the default
	/// (no extension negotiated, no requirements), which is what unblocks a waiter.
	///
	/// First write wins. The announce loops read this once and hold it for their
	/// lifetime while subscription serving re-reads it, so letting a later SETUP
	/// overwrite the identity would advertise under one exclusion and serve under
	/// another, which is how a routing loop gets back in.
	pub fn set(&self, mut peer: Peer) {
		peer.epoch &= !self.declined_epoch;
		let mut slot = self.slot.lock();
		if slot.is_some() {
			return;
		}
		*slot = Some(peer);
		drop(slot);
		if let Some(link) = &self.accept {
			link.accepted();
		}
	}

	/// Poll for the peer's SETUP.
	///
	/// The peer MUST send exactly one, so this is ready once that stream is read. Pending
	/// forever if it never does; the caller is a session task, cancelled when the driver
	/// drops.
	#[cfg(feature = "async")]
	pub fn poll_get(&self, waiter: &kio::Waiter) -> std::task::Poll<Peer> {
		let slot = std::task::ready!(self.slot.poll(waiter, |peer| match peer.is_some() {
			true => std::task::Poll::Ready(()),
			false => std::task::Poll::Pending,
		}));
		std::task::Poll::Ready((*slot).expect("polled for Some"))
	}

	/// Await the peer's SETUP; see [`Self::poll_get`].
	#[cfg(feature = "async")]
	pub async fn get(&self) -> Peer {
		kio::wait(|waiter| self.poll_get(waiter)).await
	}
}

#[cfg(all(test, feature = "async"))]
mod tests {
	use super::*;

	/// The announce loops read the peer's declaration once and hold it, while
	/// subscription serving re-reads it. A second SETUP overwriting the identity would
	/// split those two apart, so the first write is the one that counts.
	#[kio::test]
	async fn first_write_wins() {
		let first = Peer {
			cluster: cluster::Peer {
				hop: Some(crate::Hop::new(42).unwrap()),
				cost: Some(3),
			},
			solicit: None,
			hidden: false,
			max_age: false,
			auth: false,
			active_count: false,
			epoch: false,
			flate: false,
		};

		let slot = PeerSetup::default();
		slot.set(first);
		slot.set(Peer {
			cluster: cluster::Peer {
				hop: Some(crate::Hop::new(99).unwrap()),
				cost: Some(0),
			},
			solicit: Some(true),
			hidden: true,
			max_age: false,
			auth: true,
			active_count: true,
			epoch: true,
			flate: true,
		});

		assert_eq!(slot.get().await, first);
	}

	/// MoQ Epoch needs both sides: a peer declaring it to a side that declined leaves it off.
	#[kio::test]
	async fn epoch_needs_our_offer_too() {
		let declared = Peer {
			epoch: true,
			..Default::default()
		};

		let offered = PeerSetup::new(crate::setup::Extensions::default());
		offered.set(declared);
		assert!(offered.get().await.epoch);

		let declined = PeerSetup::new(crate::setup::Extensions {
			epoch: false,
			..Default::default()
		});
		declined.set(declared);
		assert!(!declined.get().await.epoch);
	}
}
