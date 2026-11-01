//! The one transport type the session code is compiled for.
//!
//! [`Client`](crate::Client) and [`Server`](crate::Server) box the caller's transport
//! here before running any protocol code, so the lite and ietf drivers are
//! monomorphized once per binary instead of once per transport and per crate that
//! awaits them. Each transport only instantiates the thin forwarding impls below.

use std::any::Any;
use std::task::Poll;

use bytes::{Buf, Bytes};

use super::poll::{self, SendOptions};
use super::{Address, CloseKind, Probe, Wire};

/// A boxed [`poll::Boxable`] session, shared by every task that uses it.
#[derive(Clone)]
pub(crate) struct Session {
	inner: std::sync::Arc<dyn ErasedSession>,
	/// Shared by every clone, so whichever task closes the transport first says why.
	closing: std::sync::Arc<crate::session::Closing>,
}

impl Session {
	pub(crate) fn new<S: poll::Boxable>(session: S) -> Self {
		Self {
			inner: std::sync::Arc::new(session),
			closing: Default::default(),
		}
	}

	/// How this side closed the transport.
	pub(crate) fn closing(&self) -> &std::sync::Arc<crate::session::Closing> {
		&self.closing
	}

	/// Close with `code` over `cause`, this side's reason for ending the session.
	pub(crate) fn abort(&self, cause: &crate::Error, code: u32, reason: &str) {
		self.closing.sent(code, Some(cause));
		self.inner.close(code, reason);
	}
}

/// A boxed transport error, keeping both code registries and the message.
#[derive(Debug)]
pub(crate) struct Error(Box<dyn super::Error>);

impl Error {
	fn new(err: impl super::Error) -> Self {
		Self(Box::new(err))
	}
}

impl std::fmt::Display for Error {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		self.0.fmt(f)
	}
}

impl std::error::Error for Error {
	fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
		self.0.source()
	}
}

impl super::Error for Error {
	fn close_kind(&self) -> CloseKind {
		self.0.close_kind()
	}

	fn session_error(&self) -> Option<(u32, String)> {
		self.0.session_error()
	}

	fn stream_error(&self) -> Option<u32> {
		self.0.stream_error()
	}
}

/// A boxed send group. It only ever goes back to the session that opened it.
pub(crate) struct Group(Box<dyn ErasedGroup>);

impl Clone for Group {
	fn clone(&self) -> Self {
		Self(self.0.clone_box())
	}
}

impl poll::SendGroup for Group {
	fn set_priority(&mut self, priority: i64) {
		self.0.set_priority(priority)
	}
}

/// A boxed outgoing stream.
pub(crate) struct SendStream(Box<dyn ErasedSend>);

impl SendStream {
	/// Box a stream, keeping its own send-group type behind the boundary.
	#[cfg(test)]
	pub(crate) fn new(stream: impl poll::SendStream<Group: Send + Sync> + Send + Sync) -> Self {
		Self(Box::new(stream))
	}
}

/// A boxed incoming stream.
pub(crate) struct RecvStream(Box<dyn ErasedRecv>);

impl RecvStream {
	#[cfg(test)]
	pub(crate) fn new(stream: impl poll::RecvStream + Send + Sync) -> Self {
		Self(Box::new(stream))
	}
}

/// A boxed [`poll::Lease`], borrowed from a [`RecvStream`].
pub(crate) struct Lease<'a>(Box<dyn ErasedLease + 'a>);

/// The transport's connection metrics, read once.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Metrics {
	bytes_lost: Option<u64>,
	packets_sent: Option<u64>,
	packets_received: Option<u64>,
	packets_lost: Option<u64>,
	rtt: Option<std::time::Duration>,
	estimated_send_rate: Option<u64>,
	streams_evicted: Option<u64>,
}

impl Metrics {
	fn read(metrics: impl super::Metrics) -> Self {
		Self {
			bytes_lost: metrics.bytes_lost(),
			packets_sent: metrics.packets_sent(),
			packets_received: metrics.packets_received(),
			packets_lost: metrics.packets_lost(),
			rtt: metrics.rtt(),
			estimated_send_rate: metrics.estimated_send_rate(),
			streams_evicted: metrics.streams_evicted(),
		}
	}
}

impl super::Metrics for Metrics {
	fn bytes_lost(&self) -> Option<u64> {
		self.bytes_lost
	}
	fn packets_sent(&self) -> Option<u64> {
		self.packets_sent
	}
	fn packets_received(&self) -> Option<u64> {
		self.packets_received
	}
	fn packets_lost(&self) -> Option<u64> {
		self.packets_lost
	}
	fn rtt(&self) -> Option<std::time::Duration> {
		self.rtt
	}
	fn estimated_send_rate(&self) -> Option<u64> {
		self.estimated_send_rate
	}
	fn streams_evicted(&self) -> Option<u64> {
		self.streams_evicted
	}
}

trait ErasedSession: Send + Sync {
	fn poll_accept_uni(&self, waiter: &kio::Waiter) -> Poll<Result<RecvStream, Error>>;
	fn poll_accept_bi(&self, waiter: &kio::Waiter) -> Poll<Result<(SendStream, RecvStream), Error>>;
	fn poll_open_uni(&self, waiter: &kio::Waiter, options: SendOptions<'_, Group>) -> Poll<Result<SendStream, Error>>;
	fn poll_open_bi(
		&self,
		waiter: &kio::Waiter,
		options: SendOptions<'_, Group>,
	) -> Poll<Result<(SendStream, RecvStream), Error>>;
	fn poll_open_request(
		&self,
		waiter: &kio::Waiter,
		options: SendOptions<'_, Group>,
	) -> Poll<Result<Option<(SendStream, RecvStream)>, Error>>;
	fn send_group(&self, priority: i64) -> Option<Group>;
	fn send_datagram(&self, payload: Bytes, options: SendOptions<'_, Group>) -> Result<(), Error>;
	fn poll_recv_datagram(&self, waiter: &kio::Waiter) -> Poll<Result<Bytes, Error>>;
	fn max_datagram_size(&self) -> usize;
	fn protocol(&self) -> Option<&str>;
	fn address(&self) -> Address;
	fn close(&self, code: u32, reason: &str);
	fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Error>;
	fn metrics(&self) -> Metrics;
	fn wire(&self) -> Option<Wire>;
	fn probe(&self) -> Option<Probe>;
}

trait ErasedGroup: Send + Sync {
	fn clone_box(&self) -> Box<dyn ErasedGroup>;
	fn set_priority(&mut self, priority: i64);
	fn as_any(&self) -> &dyn Any;
}

trait ErasedSend: Send + Sync {
	fn poll_write(&mut self, waiter: &kio::Waiter, buf: &[u8]) -> Poll<Result<usize, Error>>;
	fn poll_write_buf(&mut self, waiter: &kio::Waiter, buf: &mut dyn Buf) -> Poll<Result<usize, Error>>;
	fn set_priority(&mut self, priority: i64);
	fn set_group(&mut self, group: &Group);
	fn finish(&mut self) -> Result<(), Error>;
	fn reset(&mut self, code: u32);
	fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Result<(), Error>>;
}

trait ErasedRecv: Send + Sync {
	fn poll_lease(&mut self, waiter: &kio::Waiter, want: usize) -> Poll<Result<Lease<'_>, Error>>;
	fn poll_read_chunk(&mut self, waiter: &kio::Waiter, max: usize) -> Poll<Result<Option<Bytes>, Error>>;
	fn set_priority(&mut self, priority: i64);
	fn stop(&mut self, code: u32);
}

// Not `Send`: a lease may hold its transport's lock, and never outlives a poll.
trait ErasedLease: Buf {
	fn chunks(&self) -> Box<dyn Iterator<Item = &[u8]> + '_>;
	fn is_fin(&self) -> bool;
	fn release(self: Box<Self>);
}

type GroupOf<S> = <<S as poll::Session>::SendStream as poll::SendStream>::Group;

/// Recover the transport's own group from a boxed one.
fn unbox<'a, G: 'static>(options: SendOptions<'a, Group>) -> SendOptions<'a, G> {
	SendOptions {
		group: options.group.map(|group| {
			group
				.0
				.as_any()
				.downcast_ref::<G>()
				.expect("send group belongs to another transport")
		}),
		priority: options.priority,
	}
}

fn bi<S: poll::Boxable>((send, recv): poll::BiStreams<S>) -> (SendStream, RecvStream) {
	(SendStream(Box::new(send)), RecvStream(Box::new(recv)))
}

impl<S: poll::Boxable> ErasedSession for S {
	fn poll_accept_uni(&self, waiter: &kio::Waiter) -> Poll<Result<RecvStream, Error>> {
		poll::Session::poll_accept_uni(self, waiter)
			.map(|res| res.map(|recv| RecvStream(Box::new(recv))).map_err(Error::new))
	}

	fn poll_accept_bi(&self, waiter: &kio::Waiter) -> Poll<Result<(SendStream, RecvStream), Error>> {
		poll::Session::poll_accept_bi(self, waiter).map(|res| res.map(bi::<S>).map_err(Error::new))
	}

	fn poll_open_uni(&self, waiter: &kio::Waiter, options: SendOptions<'_, Group>) -> Poll<Result<SendStream, Error>> {
		poll::Session::poll_open_uni(self, waiter, unbox(options))
			.map(|res| res.map(|send| SendStream(Box::new(send))).map_err(Error::new))
	}

	fn poll_open_bi(
		&self,
		waiter: &kio::Waiter,
		options: SendOptions<'_, Group>,
	) -> Poll<Result<(SendStream, RecvStream), Error>> {
		poll::Session::poll_open_bi(self, waiter, unbox(options)).map(|res| res.map(bi::<S>).map_err(Error::new))
	}

	fn poll_open_request(
		&self,
		waiter: &kio::Waiter,
		options: SendOptions<'_, Group>,
	) -> Poll<Result<Option<(SendStream, RecvStream)>, Error>> {
		poll::Session::poll_open_request(self, waiter, unbox(options))
			.map(|res| res.map(|streams| streams.map(bi::<S>)).map_err(Error::new))
	}

	fn send_group(&self, priority: i64) -> Option<Group> {
		poll::Session::send_group(self, priority).map(|group| Group(Box::new(group)))
	}

	fn send_datagram(&self, payload: Bytes, options: SendOptions<'_, Group>) -> Result<(), Error> {
		poll::Session::send_datagram(self, payload, unbox::<GroupOf<S>>(options)).map_err(Error::new)
	}

	fn poll_recv_datagram(&self, waiter: &kio::Waiter) -> Poll<Result<Bytes, Error>> {
		poll::Session::poll_recv_datagram(self, waiter).map_err(Error::new)
	}

	fn max_datagram_size(&self) -> usize {
		poll::Session::max_datagram_size(self)
	}

	fn protocol(&self) -> Option<&str> {
		poll::Session::protocol(self)
	}

	fn address(&self) -> Address {
		poll::Session::address(self)
	}

	fn close(&self, code: u32, reason: &str) {
		poll::Session::close(self, code, reason)
	}

	fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Error> {
		poll::Session::poll_closed(self, waiter).map(Error::new)
	}

	fn metrics(&self) -> Metrics {
		Metrics::read(poll::Session::metrics(self))
	}

	fn wire(&self) -> Option<Wire> {
		poll::Session::wire(self)
	}

	fn probe(&self) -> Option<Probe> {
		poll::Session::probe(self)
	}
}

impl<G: poll::SendGroup + Send + Sync> ErasedGroup for G {
	fn clone_box(&self) -> Box<dyn ErasedGroup> {
		Box::new(self.clone())
	}

	fn set_priority(&mut self, priority: i64) {
		poll::SendGroup::set_priority(self, priority)
	}

	fn as_any(&self) -> &dyn Any {
		self
	}
}

impl<T: poll::SendStream<Group: Send + Sync> + Send + Sync> ErasedSend for T {
	fn poll_write(&mut self, waiter: &kio::Waiter, buf: &[u8]) -> Poll<Result<usize, Error>> {
		poll::SendStream::poll_write(self, waiter, buf).map_err(Error::new)
	}

	fn poll_write_buf(&mut self, waiter: &kio::Waiter, mut buf: &mut dyn Buf) -> Poll<Result<usize, Error>> {
		// `&mut dyn Buf` forwards `copy_to_bytes`, so a `Bytes` source stays zero-copy.
		poll::SendStream::poll_write_buf(self, waiter, &mut buf).map_err(Error::new)
	}

	fn set_priority(&mut self, priority: i64) {
		poll::SendStream::set_priority(self, priority)
	}

	fn set_group(&mut self, group: &Group) {
		let group = group
			.0
			.as_any()
			.downcast_ref::<T::Group>()
			.expect("send group belongs to another transport");
		poll::SendStream::set_group(self, group)
	}

	fn finish(&mut self) -> Result<(), Error> {
		poll::SendStream::finish(self).map_err(Error::new)
	}

	fn reset(&mut self, code: u32) {
		poll::SendStream::reset(self, code)
	}

	fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		poll::SendStream::poll_closed(self, waiter).map_err(Error::new)
	}
}

impl<T: poll::RecvStream + Send + Sync> ErasedRecv for T {
	fn poll_lease(&mut self, waiter: &kio::Waiter, want: usize) -> Poll<Result<Lease<'_>, Error>> {
		poll::RecvStream::poll_lease(self, waiter, want)
			.map(|res| res.map(|lease| Lease(Box::new(lease))).map_err(Error::new))
	}

	// Forwarded rather than left to the provided method, so reading a chunk never boxes a lease.
	fn poll_read_chunk(&mut self, waiter: &kio::Waiter, max: usize) -> Poll<Result<Option<Bytes>, Error>> {
		poll::RecvStream::poll_read_chunk(self, waiter, max).map_err(Error::new)
	}

	fn set_priority(&mut self, priority: i64) {
		poll::RecvStream::set_priority(self, priority)
	}

	fn stop(&mut self, code: u32) {
		poll::RecvStream::stop(self, code)
	}
}

impl<L: poll::Lease> ErasedLease for L {
	fn chunks(&self) -> Box<dyn Iterator<Item = &[u8]> + '_> {
		Box::new(poll::Lease::chunks(self))
	}

	fn is_fin(&self) -> bool {
		poll::Lease::is_fin(self)
	}

	fn release(self: Box<Self>) {
		poll::Lease::release(*self)
	}
}

impl poll::Session for Session {
	type SendStream = SendStream;
	type RecvStream = RecvStream;
	type Error = Error;

	fn poll_accept_uni(&self, waiter: &kio::Waiter) -> Poll<Result<RecvStream, Error>> {
		self.inner.poll_accept_uni(waiter)
	}

	fn poll_accept_bi(&self, waiter: &kio::Waiter) -> Poll<Result<(SendStream, RecvStream), Error>> {
		self.inner.poll_accept_bi(waiter)
	}

	fn poll_open_uni(&self, waiter: &kio::Waiter, options: SendOptions<'_, Group>) -> Poll<Result<SendStream, Error>> {
		self.inner.poll_open_uni(waiter, options)
	}

	fn poll_open_bi(
		&self,
		waiter: &kio::Waiter,
		options: SendOptions<'_, Group>,
	) -> Poll<Result<(SendStream, RecvStream), Error>> {
		self.inner.poll_open_bi(waiter, options)
	}

	fn poll_open_request(
		&self,
		waiter: &kio::Waiter,
		options: SendOptions<'_, Group>,
	) -> Poll<Result<Option<(SendStream, RecvStream)>, Error>> {
		self.inner.poll_open_request(waiter, options)
	}

	fn send_group(&self, priority: i64) -> Option<Group> {
		self.inner.send_group(priority)
	}

	fn send_datagram(&self, payload: Bytes, options: SendOptions<'_, Group>) -> Result<(), Error> {
		self.inner.send_datagram(payload, options)
	}

	fn poll_recv_datagram(&self, waiter: &kio::Waiter) -> Poll<Result<Bytes, Error>> {
		self.inner.poll_recv_datagram(waiter)
	}

	fn max_datagram_size(&self) -> usize {
		self.inner.max_datagram_size()
	}

	fn protocol(&self) -> Option<&str> {
		self.inner.protocol()
	}

	fn address(&self) -> Address {
		self.inner.address()
	}

	fn close(&self, code: u32, reason: &str) {
		self.closing.sent(code, None);
		self.inner.close(code, reason)
	}

	fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Error> {
		self.inner.poll_closed(waiter)
	}

	fn metrics(&self) -> impl super::Metrics {
		self.inner.metrics()
	}

	fn wire(&self) -> Option<Wire> {
		self.inner.wire()
	}

	fn probe(&self) -> Option<Probe> {
		self.inner.probe()
	}
}

impl poll::SendStream for SendStream {
	type Error = Error;
	type Group = Group;

	fn poll_write(&mut self, waiter: &kio::Waiter, buf: &[u8]) -> Poll<Result<usize, Error>> {
		self.0.poll_write(waiter, buf)
	}

	fn poll_write_buf<B: Buf>(&mut self, waiter: &kio::Waiter, buf: &mut B) -> Poll<Result<usize, Error>> {
		self.0.poll_write_buf(waiter, buf)
	}

	fn set_priority(&mut self, priority: i64) {
		self.0.set_priority(priority)
	}

	fn set_group(&mut self, group: &Group) {
		self.0.set_group(group)
	}

	fn finish(&mut self) -> Result<(), Error> {
		self.0.finish()
	}

	fn reset(&mut self, code: u32) {
		self.0.reset(code)
	}

	fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		self.0.poll_closed(waiter)
	}
}

impl poll::RecvStream for RecvStream {
	type Error = Error;
	type Lease<'a> = Lease<'a>;

	fn poll_lease(&mut self, waiter: &kio::Waiter, want: usize) -> Poll<Result<Lease<'_>, Error>> {
		self.0.poll_lease(waiter, want)
	}

	fn poll_read_chunk(&mut self, waiter: &kio::Waiter, max: usize) -> Poll<Result<Option<Bytes>, Error>> {
		self.0.poll_read_chunk(waiter, max)
	}

	fn set_priority(&mut self, priority: i64) {
		self.0.set_priority(priority)
	}

	fn stop(&mut self, code: u32) {
		self.0.stop(code)
	}
}

impl Buf for Lease<'_> {
	fn remaining(&self) -> usize {
		self.0.remaining()
	}

	fn chunk(&self) -> &[u8] {
		self.0.chunk()
	}

	fn advance(&mut self, n: usize) {
		self.0.advance(n)
	}

	// Forwarded so a transport's zero-copy override survives the box.
	fn copy_to_bytes(&mut self, n: usize) -> Bytes {
		self.0.copy_to_bytes(n)
	}
}

impl poll::Lease for Lease<'_> {
	fn chunks(&self) -> impl Iterator<Item = &[u8]> {
		self.0.chunks()
	}

	fn is_fin(&self) -> bool {
		self.0.is_fin()
	}

	fn release(self) {
		self.0.release()
	}
}
