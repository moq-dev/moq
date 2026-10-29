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
	pub(crate) fn refuse(mut self) {
		let state = self.0.take().unwrap();
		state.endpoint.refuse(state.inner);
	}

	/// Retry an incoming connection in driver tests.
	#[cfg(test)]
	pub(super) fn retry(mut self) -> Result<(), moq_quic::endpoint::RetryError> {
		let state = self.0.take().unwrap();
		state.endpoint.retry(state.inner)
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

	/// Whether the socket address that is initiating this connection has been validated
	///
	/// This means that the sender of the initial packet has proved that they can receive traffic
	/// sent to `self.remote_address()`.
	///
	#[cfg(test)]
	pub(super) fn remote_address_validated(&self) -> bool {
		self.0.as_ref().unwrap().inner.remote_address_validated()
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
