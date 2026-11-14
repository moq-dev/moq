#[cfg(test)]
use std::net::{IpAddr, SocketAddr};

use moq_quic::connection::Error as ConnectionError;

use crate::quic::driver::{connection::Connecting, endpoint::EndpointRef};

/// An incoming connection for which the server has not yet begun its part of the handshake
#[derive(Debug)]
pub struct Incoming(Option<State>);

impl Incoming {
	pub(crate) fn new(inner: moq_quic::endpoint::Incoming, endpoint: EndpointRef) -> Self {
		Self(Some(State { inner, endpoint }))
	}

	/// Attempt to accept this incoming connection (an error may still occur)
	pub fn accept(mut self) -> Result<Connecting, ConnectionError> {
		let state = self.0.take().unwrap();
		state.endpoint.accept(state.inner)
	}

	/// Reject this incoming connection attempt
	#[cfg(any(test, feature = "quic"))]
	pub(crate) fn refuse(mut self) {
		let state = self.0.take().unwrap();
		state.endpoint.refuse(state.inner);
	}

	/// Send a Retry unless the peer has already proven it receives at its address
	///
	/// Returns the incoming to accept when the address is validated, by a token from an earlier
	/// Retry or connection; otherwise the peer is asked to dial again with a Retry token and this
	/// attempt ends. A Retry costs the client a round trip, and spares the server the
	/// handshake state and the three-times amplification budget a spoofed address would get.
	pub fn validate_address(mut self) -> Option<Self> {
		let state = self.0.as_ref().unwrap();
		if state.inner.remote_address_validated() {
			return Some(self);
		}
		let state = self.0.take().unwrap();
		state.endpoint.retry(state.inner);
		None
	}

	/// The local IP address which was used when the peer established the connection
	#[cfg(test)]
	pub(super) fn local_ip(&self) -> Option<IpAddr> {
		self.0.as_ref().unwrap().inner.local_ip()
	}

	/// The peer's UDP address
	#[cfg(test)]
	pub(super) fn remote_address(&self) -> SocketAddr {
		self.0.as_ref().unwrap().inner.remote_address()
	}

}

impl Drop for Incoming {
	fn drop(&mut self) {
		// Implicit reject, similar to Connection's implicit close
		if let Some(state) = self.0.take() {
			state.endpoint.refuse(state.inner);
		}
	}
}

#[derive(Debug)]
struct State {
	inner: moq_quic::endpoint::Incoming,
	endpoint: EndpointRef,
}
