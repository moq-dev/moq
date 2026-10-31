//! The one transport type the session code is compiled for.
//!
//! [`Client`](crate::Client) and [`Server`](crate::Server) box the caller's transport
//! here before running any protocol code, so the lite and ietf drivers are
//! monomorphized once per binary instead of once per transport and per crate that
//! awaits them. Each transport only instantiates the thin forwarding impls below.

use std::any::Any;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::task::Poll;

use bytes::{Buf, Bytes};

use super::poll::{self, SendOptions};
use super::{Address, CloseKind, MaybeSend, MaybeSync, Probe, ReceiveWindow, Wire};

/// A boxed [`poll::Boxable`] session, shared by every task that uses it.
#[derive(Clone)]
pub(crate) struct Session {
	inner: Arc<Shared<dyn ErasedSession>>,
	/// Shared by every clone, so whichever task closes the transport first says why.
	closing: std::sync::Arc<crate::session::Closing>,
}

impl Session {
	pub(crate) fn new<S: poll::Boxable>(session: S) -> Self {
		Self {
			inner: Arc::new(Shared {
				paused: AtomicU64::new(0),
				session,
			}),
			closing: Default::default(),
		}
	}

	/// The handle each stream this session yields reserves its paused credit through.
	fn budget(&self) -> Budget {
		Arc::downgrade(&self.inner)
	}

	/// How this side closed the transport.
	pub(crate) fn closing(&self) -> &std::sync::Arc<crate::session::Closing> {
		&self.closing
	}

	/// Close with `code` over `cause`, this side's reason for ending the session.
	pub(crate) fn abort(&self, cause: &crate::Error, code: u32, reason: &str) {
		self.closing.sent(code, Some(cause));
		self.inner.session.close(code, reason);
	}
}

/// A boxed transport and the receive credit its paused streams hold.
struct Shared<S: ?Sized> {
	/// The stream credit reserved by streams this session stopped reading to pace a group's
	/// writer, at most half the connection window; see [`ReceiveWindow`].
	paused: AtomicU64,
	session: S,
}

impl Shared<dyn ErasedSession> {
	/// Reserve the credit of one more paused stream, unless it would push the paused total
	/// over half the connection window, returning how much it reserved.
	fn reserve(&self) -> Option<u64> {
		let window = self.session.receive_window()?;
		let charge = window.stream.min(window.connection);
		let cap = window.connection / 2;
		self.paused
			.fetch_update(Ordering::AcqRel, Ordering::Acquire, |paused| {
				paused.checked_add(charge).filter(|total| *total <= cap)
			})
			.ok()?;
		Some(charge)
	}
}

/// The session a stream reserves paused credit from, which a stream outliving it never keeps
/// open: once it is gone, nothing is paused.
type Budget = Weak<Shared<dyn ErasedSession>>;

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
	pub(crate) fn new(stream: impl poll::SendStream<Group: MaybeSend + MaybeSync> + MaybeSend + MaybeSync) -> Self {
		Self(Box::new(stream))
	}
}

/// A boxed incoming stream.
pub(crate) struct RecvStream(Box<dyn ErasedRecv>);

impl RecvStream {
	/// Box a stream outside any session, which therefore never pauses.
	#[cfg(test)]
	pub(crate) fn new(stream: impl poll::RecvStream + MaybeSend + MaybeSync) -> Self {
		Self::budgeted(stream, Weak::<Shared<super::memory::Session>>::new())
	}

	fn budgeted(stream: impl poll::RecvStream + MaybeSend + MaybeSync, session: Budget) -> Self {
		Self(Box::new(Budgeted {
			stream,
			session,
			held: 0,
		}))
	}

	/// Reserve the receive credit this stream may hold while ingest stops reading it, returning
	/// whether it may stop: not once the session's paused streams hold half its connection
	/// window, nor on a transport that reports no window.
	pub(crate) fn pause(&mut self) -> bool {
		self.0.pause()
	}

	/// Return the credit [`Self::pause`] reserved, as ingest reads the stream again.
	pub(crate) fn resume(&mut self) {
		self.0.resume()
	}
}

/// A transport's receive stream and the paused credit it reserved from its session.
struct Budgeted<T> {
	stream: T,
	session: Budget,
	/// The credit reserved while paused, zero while read.
	held: u64,
}

impl<T> Budgeted<T> {
	/// Return the credit reserved while paused, if any.
	fn release(&mut self) {
		let held = std::mem::take(&mut self.held);
		if held > 0
			&& let Some(shared) = self.session.upgrade()
		{
			shared.paused.fetch_sub(held, Ordering::AcqRel);
		}
	}
}

impl<T> Drop for Budgeted<T> {
	fn drop(&mut self) {
		self.release();
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

trait ErasedSession: MaybeSend + MaybeSync {
	fn poll_accept_uni(&self, waiter: &kio::Waiter, budget: Budget) -> Poll<Result<RecvStream, Error>>;
	fn poll_accept_bi(&self, waiter: &kio::Waiter, budget: Budget) -> Poll<Result<(SendStream, RecvStream), Error>>;
	fn poll_open_uni(&self, waiter: &kio::Waiter, options: SendOptions<'_, Group>) -> Poll<Result<SendStream, Error>>;
	fn poll_open_bi(
		&self,
		waiter: &kio::Waiter,
		options: SendOptions<'_, Group>,
		budget: Budget,
	) -> Poll<Result<(SendStream, RecvStream), Error>>;
	fn poll_open_request(
		&self,
		waiter: &kio::Waiter,
		options: SendOptions<'_, Group>,
		budget: Budget,
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
	fn receive_window(&self) -> Option<ReceiveWindow>;
}

trait ErasedGroup: MaybeSend + MaybeSync {
	fn clone_box(&self) -> Box<dyn ErasedGroup>;
	fn set_priority(&mut self, priority: i64);
	fn as_any(&self) -> &dyn Any;
}

trait ErasedSend: MaybeSend + MaybeSync {
	fn poll_write(&mut self, waiter: &kio::Waiter, buf: &[u8]) -> Poll<Result<usize, Error>>;
	fn poll_write_buf(&mut self, waiter: &kio::Waiter, buf: &mut dyn Buf) -> Poll<Result<usize, Error>>;
	fn set_priority(&mut self, priority: i64);
	fn set_group(&mut self, group: &Group);
	fn finish(&mut self) -> Result<(), Error>;
	fn reset(&mut self, code: u32);
	fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Result<(), Error>>;
}

trait ErasedRecv: MaybeSend + MaybeSync {
	fn poll_lease(&mut self, waiter: &kio::Waiter, want: usize) -> Poll<Result<Lease<'_>, Error>>;
	fn poll_read_chunk(&mut self, waiter: &kio::Waiter, max: usize) -> Poll<Result<Option<Bytes>, Error>>;
	fn set_priority(&mut self, priority: i64);
	fn stop(&mut self, code: u32);
	fn poll_reset(&mut self, waiter: &kio::Waiter) -> Poll<Error>;
	fn pause(&mut self) -> bool;
	fn resume(&mut self);
}

// Not `MaybeSend`: a lease may hold its transport's lock, and never outlives a poll.
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

fn bi<S: poll::Boxable>((send, recv): poll::BiStreams<S>, budget: Budget) -> (SendStream, RecvStream) {
	(SendStream(Box::new(send)), RecvStream::budgeted(recv, budget))
}

impl<S: poll::Boxable> ErasedSession for S {
	fn poll_accept_uni(&self, waiter: &kio::Waiter, budget: Budget) -> Poll<Result<RecvStream, Error>> {
		poll::Session::poll_accept_uni(self, waiter)
			.map(|res| res.map(|recv| RecvStream::budgeted(recv, budget)).map_err(Error::new))
	}

	fn poll_accept_bi(&self, waiter: &kio::Waiter, budget: Budget) -> Poll<Result<(SendStream, RecvStream), Error>> {
		poll::Session::poll_accept_bi(self, waiter).map(|res| res.map(|bis| bi::<S>(bis, budget)).map_err(Error::new))
	}

	fn poll_open_uni(&self, waiter: &kio::Waiter, options: SendOptions<'_, Group>) -> Poll<Result<SendStream, Error>> {
		poll::Session::poll_open_uni(self, waiter, unbox(options))
			.map(|res| res.map(|send| SendStream(Box::new(send))).map_err(Error::new))
	}

	fn poll_open_bi(
		&self,
		waiter: &kio::Waiter,
		options: SendOptions<'_, Group>,
		budget: Budget,
	) -> Poll<Result<(SendStream, RecvStream), Error>> {
		poll::Session::poll_open_bi(self, waiter, unbox(options))
			.map(|res| res.map(|bis| bi::<S>(bis, budget)).map_err(Error::new))
	}

	fn poll_open_request(
		&self,
		waiter: &kio::Waiter,
		options: SendOptions<'_, Group>,
		budget: Budget,
	) -> Poll<Result<Option<(SendStream, RecvStream)>, Error>> {
		poll::Session::poll_open_request(self, waiter, unbox(options)).map(|res| {
			res.map(|streams| streams.map(|bis| bi::<S>(bis, budget)))
				.map_err(Error::new)
		})
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

	fn receive_window(&self) -> Option<ReceiveWindow> {
		poll::Session::receive_window(self)
	}
}

impl<G: poll::SendGroup + MaybeSend + MaybeSync> ErasedGroup for G {
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

impl<T: poll::SendStream<Group: MaybeSend + MaybeSync> + MaybeSend + MaybeSync> ErasedSend for T {
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

impl<T: poll::RecvStream + MaybeSend + MaybeSync> ErasedRecv for Budgeted<T> {
	fn poll_lease(&mut self, waiter: &kio::Waiter, want: usize) -> Poll<Result<Lease<'_>, Error>> {
		poll::RecvStream::poll_lease(&mut self.stream, waiter, want)
			.map(|res| res.map(|lease| Lease(Box::new(lease))).map_err(Error::new))
	}

	// Forwarded rather than left to the provided method, so reading a chunk never boxes a lease.
	fn poll_read_chunk(&mut self, waiter: &kio::Waiter, max: usize) -> Poll<Result<Option<Bytes>, Error>> {
		poll::RecvStream::poll_read_chunk(&mut self.stream, waiter, max).map_err(Error::new)
	}

	fn set_priority(&mut self, priority: i64) {
		poll::RecvStream::set_priority(&mut self.stream, priority)
	}

	fn stop(&mut self, code: u32) {
		poll::RecvStream::stop(&mut self.stream, code)
	}

	fn poll_reset(&mut self, waiter: &kio::Waiter) -> Poll<Error> {
		poll::RecvStream::poll_reset(&mut self.stream, waiter).map(Error::new)
	}

	fn pause(&mut self) -> bool {
		if self.held > 0 {
			return true;
		}
		let Some(held) = self.session.upgrade().and_then(|shared| shared.reserve()) else {
			return false;
		};
		self.held = held;
		true
	}

	fn resume(&mut self) {
		self.release();
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
		self.inner.session.poll_accept_uni(waiter, self.budget())
	}

	fn poll_accept_bi(&self, waiter: &kio::Waiter) -> Poll<Result<(SendStream, RecvStream), Error>> {
		self.inner.session.poll_accept_bi(waiter, self.budget())
	}

	fn poll_open_uni(&self, waiter: &kio::Waiter, options: SendOptions<'_, Group>) -> Poll<Result<SendStream, Error>> {
		self.inner.session.poll_open_uni(waiter, options)
	}

	fn poll_open_bi(
		&self,
		waiter: &kio::Waiter,
		options: SendOptions<'_, Group>,
	) -> Poll<Result<(SendStream, RecvStream), Error>> {
		self.inner.session.poll_open_bi(waiter, options, self.budget())
	}

	fn poll_open_request(
		&self,
		waiter: &kio::Waiter,
		options: SendOptions<'_, Group>,
	) -> Poll<Result<Option<(SendStream, RecvStream)>, Error>> {
		self.inner.session.poll_open_request(waiter, options, self.budget())
	}

	fn send_group(&self, priority: i64) -> Option<Group> {
		self.inner.session.send_group(priority)
	}

	fn send_datagram(&self, payload: Bytes, options: SendOptions<'_, Group>) -> Result<(), Error> {
		self.inner.session.send_datagram(payload, options)
	}

	fn poll_recv_datagram(&self, waiter: &kio::Waiter) -> Poll<Result<Bytes, Error>> {
		self.inner.session.poll_recv_datagram(waiter)
	}

	fn max_datagram_size(&self) -> usize {
		self.inner.session.max_datagram_size()
	}

	fn protocol(&self) -> Option<&str> {
		self.inner.session.protocol()
	}

	fn address(&self) -> Address {
		self.inner.session.address()
	}

	fn close(&self, code: u32, reason: &str) {
		self.closing.sent(code, None);
		self.inner.session.close(code, reason)
	}

	fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Error> {
		self.inner.session.poll_closed(waiter)
	}

	fn metrics(&self) -> impl super::Metrics {
		self.inner.session.metrics()
	}

	fn wire(&self) -> Option<Wire> {
		self.inner.session.wire()
	}

	fn probe(&self) -> Option<Probe> {
		self.inner.session.probe()
	}

	fn receive_window(&self) -> Option<ReceiveWindow> {
		self.inner.session.receive_window()
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

	fn poll_reset(&mut self, waiter: &kio::Waiter) -> Poll<Error> {
		self.0.poll_reset(waiter)
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

#[cfg(test)]
mod test {
	use super::*;
	use crate::lite::test_transport::SinkSession;

	fn accept(session: &Session) -> RecvStream {
		let Poll::Ready(Ok((_, recv))) = poll::Session::poll_accept_bi(session, &kio::Waiter::noop()) else {
			panic!("the sink accepts at once");
		};
		recv
	}

	/// Each paused stream reserves its stream window, and the session pauses no more than
	/// fit in half the connection window; a stream resuming or dropped returns its share.
	#[test]
	fn paused_streams_hold_at_most_half_the_connection_window() {
		let gate = kio::Producer::new(true);
		let session =
			Session::new(SinkSession::accepted_bi(gate.consume()).with_receive_window(ReceiveWindow::new(100, 20)));
		let mut streams: Vec<_> = (0..3).map(|_| accept(&session)).collect();
		assert!(streams[0].pause());
		assert!(streams[0].pause(), "a paused stream reserves once");
		assert!(streams[1].pause());
		assert!(!streams[2].pause(), "a third would hold 60 of 100");

		streams[0].resume();
		assert!(streams[2].pause(), "a resumed stream returned its share");
		drop(streams.remove(1));
		assert!(accept(&session).pause(), "so did a dropped one");
	}

	/// A transport that reports no window never pauses a stream, since nothing bounds what
	/// it would hold; nor does a stream that outlived its session.
	#[test]
	fn an_unbounded_stream_never_pauses() {
		let gate = kio::Producer::new(true);
		let session = Session::new(SinkSession::accepted_bi(gate.consume()));
		assert!(!accept(&session).pause());

		let session =
			Session::new(SinkSession::accepted_bi(gate.consume()).with_receive_window(ReceiveWindow::new(100, 20)));
		let mut stream = accept(&session);
		drop(session);
		assert!(!stream.pause());
	}
}
