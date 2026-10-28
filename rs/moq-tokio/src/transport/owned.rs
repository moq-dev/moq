use bytes::{Buf, Bytes};
use moq_net::transport::{self as moq, poll};
use std::task::Poll;
use web_transport_trait::{self as wt, poll as backend};

/// A backend failure preserving the session and stream code registries.
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct Error<E>(E);

impl<E: wt::Error> moq::Error for Error<E> {
	fn close_kind(&self) -> moq::CloseKind {
		close_kind(&self.0)
	}
	fn session_error(&self) -> Option<(u32, String)> {
		self.0.session_error()
	}
	fn stream_error(&self) -> Option<u32> {
		self.0.stream_error()
	}
}

struct Metrics<S> {
	inner: S,
	streams_evicted: Option<u64>,
}
impl<S: wt::Stats> moq::Metrics for Metrics<S> {
	fn bytes_lost(&self) -> Option<u64> {
		self.inner.bytes_lost()
	}
	fn packets_sent(&self) -> Option<u64> {
		self.inner.packets_sent()
	}
	fn packets_received(&self) -> Option<u64> {
		self.inner.packets_received()
	}
	fn packets_lost(&self) -> Option<u64> {
		self.inner.packets_lost()
	}
	fn rtt(&self) -> Option<std::time::Duration> {
		self.inner.rtt()
	}
	fn estimated_send_rate(&self) -> Option<u64> {
		self.inner.estimated_send_rate()
	}
	fn streams_evicted(&self) -> Option<u64> {
		self.streams_evicted
	}
}
/// The send half moq-net writes: [`SendStream`](super::SendStream), bridged so its close can be
/// watched through a shared borrow.
///
/// `pub` only because its session's `type SendStream` names it; the module is private.
pub struct BridgedSend<S: super::GroupedSend + 'static>(moq_net::kio::Bridge<super::SendStream<S>>);

impl<S: super::GroupedSend + 'static> BridgedSend<S> {
	pub(super) fn new(stream: S) -> Self {
		Self(moq_net::kio::Bridge::new(super::SendStream::new(stream)))
	}

	/// A newly opened stream, scheduled by its options.
	fn scheduled(stream: S, options: poll::SendOptions<'_, S::Group>) -> Self {
		let mut stream = Self::new(stream);
		stream.0.get_mut().schedule(options);
		stream
	}
}

impl<S: super::Grouped<SendStream: 'static, RecvStream: 'static>> poll::Session for super::Session<S> {
	type SendStream = BridgedSend<S::SendStream>;
	type RecvStream = S::Recv;
	type Error = Error<S::Error>;
	fn poll_accept_uni(&self, waiter: &moq_net::kio::Waiter) -> Poll<Result<Self::RecvStream, Self::Error>> {
		let session = &self.session;
		super::poll_op(&self.accept_uni, waiter, || {
			let session = session.clone();
			Box::pin(async move { session.accept_uni().await })
		})
		.map_ok(|recv| S::recv(super::RecvStream::new(recv)))
		.map_err(Error)
	}
	fn poll_accept_bi(&self, waiter: &moq_net::kio::Waiter) -> Poll<Result<poll::BiStreams<Self>, Self::Error>> {
		let session = &self.session;
		super::poll_op(&self.accept_bi, waiter, || {
			let session = session.clone();
			Box::pin(async move { session.accept_bi().await })
		})
		.map_ok(|(send, recv)| (BridgedSend::new(send), S::recv(super::RecvStream::new(recv))))
		.map_err(Error)
	}
	fn poll_open_uni(
		&self,
		waiter: &moq_net::kio::Waiter,
		options: poll::SendOptions<'_, <S::SendStream as super::GroupedSend>::Group>,
	) -> Poll<Result<Self::SendStream, Self::Error>> {
		let session = &self.session;
		super::poll_op(&self.open_uni, waiter, || {
			let session = session.clone();
			Box::pin(async move { session.open_uni().await })
		})
		.map_ok(|send| BridgedSend::scheduled(send, options))
		.map_err(Error)
	}
	fn poll_open_bi(
		&self,
		waiter: &moq_net::kio::Waiter,
		options: poll::SendOptions<'_, <S::SendStream as super::GroupedSend>::Group>,
	) -> Poll<Result<poll::BiStreams<Self>, Self::Error>> {
		let session = &self.session;
		super::poll_op(&self.open_bi, waiter, || {
			let session = session.clone();
			Box::pin(async move { session.open_bi().await })
		})
		.map_ok(|(send, recv)| {
			(
				BridgedSend::scheduled(send, options),
				S::recv(super::RecvStream::new(recv)),
			)
		})
		.map_err(Error)
	}

	fn poll_open_request(
		&self,
		waiter: &moq_net::kio::Waiter,
		options: poll::SendOptions<'_, <S::SendStream as super::GroupedSend>::Group>,
	) -> Poll<Result<Option<poll::BiStreams<Self>>, Self::Error>> {
		let session = &self.session;
		super::poll_op(&self.request_bi, waiter, || {
			let session = session.clone();
			Box::pin(async move { session.open_request().await })
		})
		.map_ok(|streams| {
			streams.map(|(send, recv)| {
				(
					BridgedSend::scheduled(send, options),
					S::recv(super::RecvStream::new(recv)),
				)
			})
		})
		.map_err(Error)
	}
	fn send_group(&self, priority: i64) -> Option<<S::SendStream as super::GroupedSend>::Group> {
		super::Grouped::send_group(&self.session, priority)
	}
	fn send_datagram(
		&self,
		payload: Bytes,
		options: poll::SendOptions<'_, <S::SendStream as super::GroupedSend>::Group>,
	) -> Result<(), Self::Error> {
		super::Grouped::send_datagram(&self.session, payload, options).map_err(Error)
	}
	fn poll_recv_datagram(&self, waiter: &moq_net::kio::Waiter) -> Poll<Result<Bytes, Self::Error>> {
		let session = &self.session;
		super::poll_op(&self.recv_datagram, waiter, || {
			let session = session.clone();
			Box::pin(async move { session.recv_datagram().await })
		})
		.map_err(Error)
	}
	fn max_datagram_size(&self) -> usize {
		wt::Session::max_datagram_size(&self.session)
	}
	fn address(&self) -> moq::Address {
		self.address
	}
	fn protocol(&self) -> Option<&str> {
		wt::Session::protocol(&self.session)
	}
	fn close(&self, code: u32, reason: &str) {
		wt::Session::close(&self.session, code, reason);
	}
	fn poll_closed(&self, waiter: &moq_net::kio::Waiter) -> Poll<Self::Error> {
		let session = &self.session;
		super::poll_op(&self.closed, waiter, || {
			let session = session.clone();
			Box::pin(async move { session.closed().await })
		})
		.map(Error)
	}
	fn metrics(&self) -> impl moq::Metrics {
		let inner = wt::Session::stats(&self.session);
		// Reading the metrics refreshes the polled wire, which moq-net reads right after.
		if super::Grouped::wire(&self.session).is_none() {
			if let Some(sent) = wt::Stats::bytes_sent(&inner) {
				self.polled_wire.set_sent_bytes(sent);
			}
			if let Some(received) = wt::Stats::bytes_received(&inner) {
				self.polled_wire.set_received_bytes(received);
			}
		}
		Metrics {
			inner,
			streams_evicted: super::Grouped::streams_evicted(&self.session),
		}
	}
	fn wire(&self) -> Option<moq::Wire> {
		super::Grouped::wire(&self.session).or_else(|| Some(self.polled_wire.clone()))
	}
	fn probe(&self) -> Option<moq::Probe> {
		super::Grouped::probe(&self.session)
	}
}
impl<S: super::GroupedSend + 'static> poll::SendStream for BridgedSend<S> {
	type Error = Error<S::Error>;
	type Group = S::Group;
	fn poll_write(&mut self, waiter: &moq_net::kio::Waiter, buf: &[u8]) -> Poll<Result<usize, Self::Error>> {
		backend::SendStream::poll_write(self.0.get_mut(), &mut waiter.context(), buf).map_err(Error)
	}
	fn poll_write_buf<B: Buf>(
		&mut self,
		waiter: &moq_net::kio::Waiter,
		buf: &mut B,
	) -> Poll<Result<usize, Self::Error>> {
		backend::SendStream::poll_write_buf(self.0.get_mut(), &mut waiter.context(), buf).map_err(Error)
	}
	fn set_priority(&mut self, priority: i64) {
		self.0.get_mut().prioritize(priority)
	}
	fn set_group(&mut self, group: &Self::Group) {
		self.0.get_mut().join(group)
	}
	fn finish(&mut self) -> Result<(), Self::Error> {
		backend::SendStream::finish(self.0.get_mut()).map_err(Error)
	}
	fn reset(&mut self, code: u32) {
		backend::SendStream::reset(self.0.get_mut(), code)
	}
	fn poll_closed(&self, waiter: &moq_net::kio::Waiter) -> Poll<Result<(), Self::Error>> {
		self.0.poll(waiter, backend::SendStream::poll_closed).map_err(Error)
	}
}
impl<S: wt::RecvStream + 'static> poll::Chunks for super::RecvStream<S> {
	type Error = Error<S::Error>;
	fn poll_read_chunk(
		&mut self,
		waiter: &moq_net::kio::Waiter,
		max: usize,
	) -> Poll<Result<Option<Bytes>, Self::Error>> {
		backend::RecvStream::poll_read_chunk(self, &mut waiter.context(), max).map_err(Error)
	}
	fn stop(&mut self, code: u32) {
		backend::RecvStream::stop(self, code)
	}
}

#[cfg(feature = "quic")]
impl poll::RecvStream for crate::quic::web::RecvStream {
	type Error = Error<crate::quic::web::ReadError>;
	type Lease<'a> = crate::quic::driver::Lease<'a>;

	fn poll_lease(&mut self, waiter: &moq_net::kio::Waiter, want: usize) -> Poll<Result<Self::Lease<'_>, Self::Error>> {
		crate::quic::web::RecvStream::poll_lease(self, &mut waiter.context(), want).map_err(Error)
	}
	fn set_priority(&mut self, priority: i64) {
		crate::quic::web::RecvStream::set_priority(self, priority)
	}
	fn stop(&mut self, code: u32) {
		// A stream already closed has nobody left to tell.
		crate::quic::web::RecvStream::stop(self, code).ok();
	}
}

#[cfg(feature = "quic")]
impl poll::Lease for crate::quic::driver::Lease<'_> {
	fn is_fin(&self) -> bool {
		crate::quic::driver::Lease::is_fin(self)
	}
	fn chunks(&self) -> impl Iterator<Item = &[u8]> {
		crate::quic::driver::Lease::chunks(self)
	}
	fn release(self) {
		crate::quic::driver::Lease::release(self)
	}
}

fn close_kind(error: &(dyn std::error::Error + 'static)) -> moq::CloseKind {
	#[cfg(feature = "quic")]
	if let Some(crate::quic::web::ReadError::Evicted) = error.downcast_ref::<crate::quic::web::ReadError>() {
		return moq::CloseKind::Evicted;
	}
	#[cfg(feature = "quic")]
	if let Some(error) = error.downcast_ref::<crate::quic::web::SessionError>() {
		use crate::quic::{driver::ConnectionError, web::SessionError};
		return match error {
			SessionError::ConnectionError(ConnectionError::TimedOut) => moq::CloseKind::IdleTimeout,
			SessionError::ConnectionError(ConnectionError::LocallyClosed) => moq::CloseKind::Closed,
			_ => moq::CloseKind::Transport,
		};
	}
	#[cfg(feature = "iroh")]
	if let Some(error) = error.downcast_ref::<web_transport_iroh::iroh::endpoint::ConnectionError>() {
		use web_transport_iroh::iroh::endpoint::ConnectionError;
		return match error {
			ConnectionError::TimedOut => moq::CloseKind::IdleTimeout,
			ConnectionError::LocallyClosed => moq::CloseKind::Closed,
			_ => moq::CloseKind::Transport,
		};
	}
	error.source().map(close_kind).unwrap_or_default()
}

#[cfg(all(test, feature = "quic"))]
mod outcome_tests {
	use super::*;
	use crate::quic::{driver::ConnectionError, web::SessionError};
	use moq_net::transport::Error as _;

	#[test]
	fn typed_idle_and_local_closes_survive_session_and_stream_wrappers() {
		let idle = Error(SessionError::ConnectionError(ConnectionError::TimedOut));
		assert!(matches!(
			moq_net::Error::from_transport(idle),
			moq_net::Error::IdleTimeout
		));
		let idle = Error(crate::quic::web::ReadError::SessionError(
			SessionError::ConnectionError(ConnectionError::TimedOut),
		));
		assert_eq!(idle.close_kind(), moq::CloseKind::IdleTimeout);
		let local = Error(SessionError::ConnectionError(ConnectionError::LocallyClosed));
		assert!(matches!(
			moq_net::Error::from_transport(local),
			moq_net::Error::Session(moq_net::SessionError::Cancel)
		));
	}
}

#[cfg(all(test, feature = "iroh"))]
mod iroh_outcome_tests {
	use super::*;
	use web_transport_iroh::{SessionError, iroh::endpoint::ConnectionError};

	#[test]
	fn typed_iroh_close_kinds_survive_the_error_source_chain() {
		let idle = Error(SessionError::ConnectionError(ConnectionError::TimedOut));
		assert!(matches!(
			moq_net::Error::from_transport(idle),
			moq_net::Error::IdleTimeout
		));
		let local = Error(SessionError::ConnectionError(ConnectionError::LocallyClosed));
		assert!(matches!(
			moq_net::Error::from_transport(local),
			moq_net::Error::Session(moq_net::SessionError::Cancel)
		));
	}
}
