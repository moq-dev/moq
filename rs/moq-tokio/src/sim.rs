//! Test support, enabled by the `sim` and `net` features: a network inside the process, for
//! tests on `kio::clock::sim`.
//!
//! A [`Client`](crate::Client) given a [`Network`] opens no sockets: each dial becomes a
//! [`Dial`] on its [`Dials`], and whoever routes those answers it with an in-memory link.
//! Everything else a dial does, the handshake, redials, and GOAWAY handling, runs as it
//! would over a socket.
//!
//! It tracks the crate's internals, so it can change in any release.

use futures::{
	StreamExt,
	channel::{mpsc, oneshot},
};
use moq_net::transport::memory;
use url::Url;

/// Where a [`Client`](crate::Client) dials instead of the network.
///
/// Set it with [`Client::with_network`](crate::Client::with_network); clones send into
/// the same [`Dials`].
#[derive(Clone, Debug)]
pub struct Network {
	dials: mpsc::UnboundedSender<Dial>,
}

impl Network {
	/// A network, and the dials made through it.
	pub fn new() -> (Self, Dials) {
		let (dials, received) = mpsc::unbounded();
		(Self { dials }, Dials { received })
	}

	/// Open a link to `url` offering `versions`, once the routing side accepts it.
	pub(crate) async fn dial(&self, url: Url, versions: moq_net::Versions) -> crate::Result<memory::Session> {
		let refused = || {
			crate::Error::Io(std::sync::Arc::new(std::io::Error::from(
				std::io::ErrorKind::ConnectionRefused,
			)))
		};
		let (reply, link) = oneshot::channel();
		self.dials
			.unbounded_send(Dial { url, versions, reply })
			.map_err(|_| refused())?;
		link.await.map_err(|_| refused())
	}
}

/// Every dial made through a [`Network`], in the order they were made.
#[derive(Debug)]
pub struct Dials {
	received: mpsc::UnboundedReceiver<Dial>,
}

impl Dials {
	/// The next dial, or `None` once every [`Network`] clone is gone.
	pub async fn next(&mut self) -> Option<Dial> {
		self.received.next().await
	}
}

/// A dial waiting for an answer: [`accept`](Self::accept) connects it, and dropping it
/// refuses it, as a closed port would.
#[derive(Debug)]
pub struct Dial {
	url: Url,
	versions: moq_net::Versions,
	reply: oneshot::Sender<memory::Session>,
}

impl Dial {
	/// The URL dialed, credentials included.
	pub fn url(&self) -> &Url {
		&self.url
	}

	/// The versions the dialer offers, most preferred first.
	pub fn versions(&self) -> &moq_net::Versions {
		&self.versions
	}

	/// Connect the dialer over a fresh in-memory link built from `config`, returning the
	/// accepting half. Its `protocol` stands in for the ALPN the two sides agreed on.
	pub fn accept(self, config: memory::Config) -> memory::Session {
		let (dialer, accepter) = memory::pair(config);
		// A dialer that gave up drops its half, which closes the link.
		let _ = self.reply.send(dialer);
		accepter
	}
}
