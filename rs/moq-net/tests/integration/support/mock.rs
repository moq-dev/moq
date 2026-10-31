//! Fault knobs over the in-memory transport, for deterministic tests.
//!
//! [`MockSession`] wraps one end of a [`memory`] pair, so stream payloads cross
//! zero-copy and flow-controlled exactly as between two in-process peers. The knobs
//! bend delivery the ways a real network may: link latency, streams held back or
//! reordered, a first write split from the rest, lost datagrams, and FINs that are
//! never sent or never acknowledged.
//!
//! The transport hands streams to the acceptor in the order they were opened, so each
//! open queues a [`Line`] that the matching accept picks up: the knobs on a stream's
//! two ends meet there.

use std::{
	collections::VecDeque,
	sync::{
		Arc, Mutex,
		atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering},
	},
	task::{Poll, ready},
	time::Duration,
};

use moq_net::{
	bytes::{Buf, Bytes},
	kio,
	time::Instant,
	transport::{memory, poll},
};

use super::harness::now;

/// One direction of a stream, shared by the knobs on its writer and its reader.
#[derive(Default)]
struct Line {
	/// When each write, then the FIN, reaches the reader: its send time plus the latency.
	arrivals: Mutex<VecDeque<Instant>>,
	/// When the reset reaches the reader.
	reset: Mutex<Option<Instant>>,
	/// The send order last set, for [`MockSession::release_unis_by_priority`].
	priority: AtomicI64,
	/// Opened while its opener held streams back.
	held: bool,
	/// Once released, delivers what follows the first write; see [`MockSession::split_unis`].
	split: Option<Arc<kio::Shared<bool>>>,
}

/// State shared by both sides of a pair.
#[derive(Default)]
struct Conn {
	finishes: AtomicUsize,
	blocked_writes: kio::Shared<usize>,
	hold_fins: AtomicBool,
	hold_writes: AtomicBool,
	/// Acknowledge every FIN as soon as it is sent, before the peer reads it.
	ack_fins: AtomicBool,
	/// One-way delay for stream data and STOP_SENDING in each direction. Zero by default.
	latency: Mutex<Duration>,
}

impl Conn {
	fn latency(&self) -> Duration {
		*self.latency.lock().unwrap()
	}

	fn arrival(&self) -> Instant {
		now() + self.latency()
	}
}

type Uni = (memory::RecvStream, Arc<Line>);
type Bi = ((memory::SendStream, Arc<Line>), (memory::RecvStream, Arc<Line>));

/// Streams flowing one way: the opener's knobs and the acceptor's queue.
struct Link {
	/// The lines of opened streams the acceptor has yet to take, in open order.
	uni: Mutex<VecDeque<Arc<Line>>>,
	/// The same for bidi streams: toward the acceptor, then back.
	bi: Mutex<VecDeque<[Arc<Line>; 2]>>,
	/// Accepted streams held back from the acceptor's application, while holding.
	held_uni: Mutex<Option<Vec<Uni>>>,
	held_bi: Mutex<Option<Vec<Bi>>>,
	/// Accepted streams the acceptor's application may take.
	ready_uni: kio::Queue<Uni>,
	ready_bi: kio::Queue<Bi>,
	/// While set, the uni streams opened hold back everything after their first write.
	split: Mutex<Option<Arc<kio::Shared<bool>>>>,
	/// A responder can gate the return direction of newly opened bidi streams too.
	split_responses: Mutex<Option<Arc<kio::Shared<bool>>>>,
	/// The acceptor's transport, so a release can sort streams it has not accepted yet.
	acceptor: Mutex<memory::Session>,
}

impl Link {
	fn new(acceptor: memory::Session) -> Self {
		Self {
			uni: Default::default(),
			bi: Default::default(),
			held_uni: Default::default(),
			held_bi: Default::default(),
			ready_uni: kio::Queue::new(),
			ready_bi: kio::Queue::new(),
			split: Default::default(),
			split_responses: Default::default(),
			acceptor: Mutex::new(acceptor),
		}
	}

	/// Move every stream the transport has accepted to the held or ready queue,
	/// returning the transport's error once it has none left.
	fn route(&self, session: &memory::Session, waiter: &kio::Waiter) -> Option<memory::Error> {
		let mut unis = self.uni.lock().unwrap();
		let uni_err = loop {
			match poll::Session::poll_accept_uni(session, waiter) {
				Poll::Ready(Ok(recv)) => {
					let line = unis.pop_front().expect("opened");
					let held = line.held;
					let stream = (recv, line);
					match self.held_uni.lock().unwrap().as_mut() {
						Some(list) if held => list.push(stream),
						_ => drop(self.ready_uni.try_push(stream)),
					}
				}
				Poll::Ready(Err(err)) => break Some(err),
				Poll::Pending => break None,
			}
		};
		drop(unis);

		let mut bis = self.bi.lock().unwrap();
		let bi_err = loop {
			match poll::Session::poll_accept_bi(session, waiter) {
				Poll::Ready(Ok((send, recv))) => {
					let [forward, back] = bis.pop_front().expect("opened");
					let held = forward.held;
					let stream = ((send, back), (recv, forward));
					match self.held_bi.lock().unwrap().as_mut() {
						Some(list) if held => list.push(stream),
						_ => drop(self.ready_bi.try_push(stream)),
					}
				}
				Poll::Ready(Err(err)) => break Some(err),
				Poll::Pending => break None,
			}
		};
		uni_err.or(bi_err)
	}

	/// Sort every stream opened so far, so a release sees them all.
	fn settle(&self) {
		let acceptor = self.acceptor.lock().unwrap();
		self.route(&acceptor, &kio::Waiter::noop());
	}

	/// Deliver held uni streams in the order `pick` leaves them, and stop holding.
	fn release_unis(&self, pick: impl FnOnce(&mut Vec<Uni>)) {
		self.settle();
		let mut held = self.held_uni.lock().unwrap().take().unwrap_or_default();
		pick(&mut held);
		for stream in held {
			let _ = self.ready_uni.try_push(stream);
		}
	}
}

// ── SendStream ──────────────────────────────────────────────────────

/// A send stream with the knobs of the side that holds it.
pub struct MockSendStream {
	inner: memory::SendStream,
	line: Arc<Line>,
	conn: Arc<Conn>,
	/// Waits for the session close while FIN acknowledgements are held.
	session: memory::Session,
	/// Every code the owning side reset a stream with.
	resets: Resets,
	/// Drop this stream's FIN, as a peer that never completes it (see
	/// [`MockSession::withhold_bidi_fins`]).
	withhold_fin: Option<Arc<AtomicBool>>,
	/// Whether the FIN was withheld, so the drop delivers it instead of a reset.
	withheld: bool,
	/// Where to log the first byte written, the stream type (see
	/// [`MockSession::bidi_types`]), until it is written.
	first_byte: Option<StreamTypes>,
	/// Finished or reset.
	done: bool,
	/// Finished, so the FIN is acknowledged when [`Self::ack`] says so.
	finished: bool,
	/// Acknowledge the FIN as soon as it is sent, for a stream the peer's transport holds
	/// back from its application (see [`MockSession::hold_unis`]).
	ack: bool,
}

impl MockSendStream {
	/// Log the first byte, then stamp each accepted write with its arrival.
	fn wrote(
		&mut self,
		first: Option<u8>,
		written: Poll<Result<usize, memory::Error>>,
	) -> Poll<Result<usize, memory::Error>> {
		if written.is_pending() {
			*self.conn.blocked_writes.lock() += 1;
		}
		if let Poll::Ready(Ok(size)) = written
			&& size > 0
		{
			if let Some(types) = self.first_byte.take() {
				types.lock().unwrap().push(first.expect("non-empty write"));
			}
			self.line.arrivals.lock().unwrap().push_back(self.conn.arrival());
		}
		written
	}
}

impl poll::SendStream for MockSendStream {
	type Error = memory::Error;
	type Group = poll::Ungrouped;

	fn poll_write(&mut self, waiter: &kio::Waiter, buf: &[u8]) -> Poll<Result<usize, Self::Error>> {
		if self.conn.hold_writes.load(Ordering::Relaxed) {
			let written = poll::Session::poll_closed(&self.session, waiter).map(Err);
			return self.wrote(buf.first().copied(), written);
		}
		let written = self.inner.poll_write(waiter, buf);
		self.wrote(buf.first().copied(), written)
	}

	fn poll_write_buf<B: Buf>(&mut self, waiter: &kio::Waiter, buf: &mut B) -> Poll<Result<usize, Self::Error>> {
		let first = buf.chunk().first().copied();
		if self.conn.hold_writes.load(Ordering::Relaxed) {
			let written = poll::Session::poll_closed(&self.session, waiter).map(Err);
			return self.wrote(first, written);
		}
		let written = self.inner.poll_write_buf(waiter, buf);
		self.wrote(first, written)
	}

	fn set_priority(&mut self, order: i64) {
		self.line.priority.store(order, Ordering::Relaxed);
		self.inner.set_priority(order);
	}

	fn set_group(&mut self, group: &Self::Group) {
		match *group {}
	}

	fn finish(&mut self) -> Result<(), Self::Error> {
		if self
			.withhold_fin
			.as_ref()
			.is_some_and(|flag| flag.load(Ordering::Relaxed))
		{
			self.withheld = true;
			return Ok(());
		}
		self.inner.finish()?;
		self.done = true;
		self.finished = true;
		self.line.arrivals.lock().unwrap().push_back(self.conn.arrival());
		self.conn.finishes.fetch_add(1, Ordering::Relaxed);
		Ok(())
	}

	fn reset(&mut self, code: u32) {
		if !self.done {
			self.done = true;
			self.resets.lock().unwrap().push(code);
			*self.line.reset.lock().unwrap() = Some(self.conn.arrival());
			self.inner.reset(code);
		}
	}

	fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Result<(), Self::Error>> {
		if self.conn.hold_fins.load(Ordering::Relaxed) {
			return Poll::Ready(Err(ready!(poll::Session::poll_closed(&self.session, waiter))));
		}
		if self.finished && (self.ack || self.conn.ack_fins.load(Ordering::Relaxed)) {
			return Poll::Ready(Ok(()));
		}
		self.inner.poll_closed(waiter)
	}
}

impl Drop for MockSendStream {
	fn drop(&mut self) {
		// A withheld FIN goes out when the stream does, as a peer that only finishes on teardown.
		if self.withheld && poll::SendStream::finish(&mut self.inner).is_ok() {
			self.line.arrivals.lock().unwrap().push_back(self.conn.arrival());
		}
	}
}

// ── RecvStream ──────────────────────────────────────────────────────

/// What the transport handed over: a chunk, the FIN, or an error.
type Event = Result<Option<Bytes>, memory::Error>;

/// A receive stream that delivers each event once its arrival has passed.
pub type MockRecvStream = poll::Buffered<MockChunks>;

/// The plain reads of a [`MockRecvStream`].
pub struct MockChunks {
	/// Taken once this side stops reading.
	inner: Option<memory::RecvStream>,
	line: Arc<Line>,
	conn: Arc<Conn>,
	/// The next event, read from the transport but still crossing the link.
	flight: Option<(Instant, Event)>,
	/// The arrival a wake is already scheduled for, so each flight schedules one.
	landing: Option<Instant>,
	/// The rest of a chunk a smaller read split.
	buf: Bytes,
	/// Data chunks handed to the reader so far.
	chunks: usize,
}

impl MockChunks {
	fn new(inner: memory::RecvStream, line: Arc<Line>, conn: Arc<Conn>) -> Self {
		Self {
			inner: Some(inner),
			line,
			conn,
			flight: None,
			landing: None,
			buf: Bytes::new(),
			chunks: 0,
		}
	}

	/// The next whole chunk, FIN, or error, once it has arrived.
	fn poll_event(&mut self, waiter: &kio::Waiter) -> Poll<Event> {
		if let Some(split) = &self.line.split
			&& self.chunks > 0
			&& split
				.poll(waiter, |released| match **released {
					true => Poll::Ready(()),
					false => Poll::Pending,
				})
				.is_pending()
		{
			return Poll::Pending;
		}

		if self.flight.is_none() {
			let Some(inner) = self.inner.as_mut() else {
				return Poll::Ready(Err(memory::Error::Closed));
			};
			// Below the buffering, so each event is one whole message the writer sent.
			let event = ready!(poll::Chunks::poll_read_chunk(inner.get_mut(), waiter, usize::MAX));
			let arrival = match &event {
				Err(memory::Error::Reset(_)) => self.line.reset.lock().unwrap().take(),
				Ok(_) => self.line.arrivals.lock().unwrap().pop_front(),
				Err(_) => None,
			};
			self.flight = Some((arrival.unwrap_or_else(now), event));
		}

		let arrival = self.flight.as_ref().expect("flight set above").0;
		if arrival > now() {
			// Only a test sets a latency, so the flight lands on the simulated clock.
			if self.landing != Some(arrival) {
				self.landing = Some(arrival);
				let waker = waiter.waker().clone();
				drop(moq_clock::sim::spawn(async move {
					moq_clock::sleep_until(arrival).await;
					waker.wake();
				}));
			}
			return Poll::Pending;
		}

		let (_, event) = self.flight.take().expect("flight set above");
		if let Ok(Some(_)) = event {
			self.chunks += 1;
		}
		Poll::Ready(event)
	}

	/// Stop reading, telling the writer once the latency has passed.
	fn stop_after_latency(&mut self, code: u32) {
		let Some(mut inner) = self.inner.take() else {
			return;
		};
		let latency = self.conn.latency();
		if latency.is_zero() {
			poll::RecvStream::stop(&mut inner, code);
			return;
		}
		drop(moq_clock::sim::spawn(async move {
			moq_clock::sleep(latency).await;
			poll::RecvStream::stop(&mut inner, code);
		}));
	}
}

impl poll::Chunks for MockChunks {
	type Error = memory::Error;

	fn poll_read_chunk(&mut self, waiter: &kio::Waiter, max: usize) -> Poll<Result<Option<Bytes>, Self::Error>> {
		if max == 0 {
			return Poll::Ready(Ok(Some(Bytes::new())));
		}
		if self.buf.is_empty() {
			match ready!(self.poll_event(waiter))? {
				Some(chunk) => self.buf = chunk,
				None => return Poll::Ready(Ok(None)),
			}
		}
		let size = max.min(self.buf.len());
		Poll::Ready(Ok(Some(self.buf.split_to(size))))
	}

	fn stop(&mut self, code: u32) {
		self.stop_after_latency(code);
	}
}

impl Drop for MockChunks {
	fn drop(&mut self) {
		self.stop_after_latency(0);
	}
}

// ── MockSession ─────────────────────────────────────────────────────

/// The stream reset codes one side has sent.
type Resets = Arc<Mutex<Vec<u32>>>;

/// The first byte of each bidi stream one side has opened.
type StreamTypes = Arc<Mutex<Vec<u8>>>;

/// One side's knobs.
struct Side {
	conn: Arc<Conn>,
	/// Streams this side opens.
	out: Arc<Link>,
	/// Streams the peer opens.
	inc: Arc<Link>,
	/// Whether the peer has withheld uni stream credit, parking every open.
	withheld: AtomicBool,
	/// Whether the datagrams this side sends are lost.
	lossy: AtomicBool,
	/// Whether this side drops the FIN of the bidi streams it opens.
	withhold_bidi_fins: Arc<AtomicBool>,
	/// The first byte of each bidi stream this side opened.
	bidi_types: StreamTypes,
	/// Stream resets sent by this side.
	resets: Resets,
	/// What [`poll::Session::metrics`] reports.
	stats: Mutex<MockStats>,
	/// How many times the stats were read.
	stats_reads: AtomicUsize,
	/// Whether reading the stats panics; see [`MockSession::panic_on_stats`].
	panic_on_stats: AtomicBool,
	/// The wire counters [`poll::Session::wire`] exposes, which a test updates as a transport would.
	wire: moq_net::transport::Wire,
}

/// The connection statistics a [`MockSession`] reports. Unavailable by default.
#[derive(Clone, Copy, Default)]
pub struct MockStats {
	/// The congestion controller's send-rate estimate, in bits per second.
	pub estimated_send_rate: Option<u64>,
	/// The smoothed round-trip time.
	pub rtt: Option<Duration>,
}

impl moq_net::transport::Metrics for MockStats {
	fn estimated_send_rate(&self) -> Option<u64> {
		self.estimated_send_rate
	}

	fn rtt(&self) -> Option<Duration> {
		self.rtt
	}
}

/// One end of an in-memory session pair, with fault knobs. Created by
/// [`create_mock_session_pair`].
#[derive(Clone)]
pub struct MockSession {
	inner: memory::Session,
	/// Parks withheld opens on the close, apart from the session's own close poll.
	closer: memory::Session,
	side: Arc<Side>,
}

impl MockSession {
	fn send(&self, inner: memory::SendStream, line: Arc<Line>) -> MockSendStream {
		MockSendStream {
			inner,
			line,
			conn: self.side.conn.clone(),
			session: self.inner.clone(),
			resets: self.side.resets.clone(),
			withhold_fin: None,
			withheld: false,
			first_byte: None,
			done: false,
			finished: false,
			ack: false,
		}
	}

	fn recv(&self, inner: memory::RecvStream, line: Arc<Line>) -> MockRecvStream {
		poll::Buffered::new(MockChunks::new(inner, line, self.side.conn.clone()))
	}
}

impl poll::Session for MockSession {
	type SendStream = MockSendStream;
	type RecvStream = MockRecvStream;
	type Error = memory::Error;

	fn poll_accept_uni(&self, waiter: &kio::Waiter) -> Poll<Result<Self::RecvStream, Self::Error>> {
		let err = self.side.inc.route(&self.inner, waiter);
		if let Poll::Ready(Ok((recv, line))) = self.side.inc.ready_uni.poll_pop(waiter) {
			return Poll::Ready(Ok(self.recv(recv, line)));
		}
		err.map_or(Poll::Pending, |err| Poll::Ready(Err(err)))
	}

	fn poll_accept_bi(&self, waiter: &kio::Waiter) -> Poll<Result<(Self::SendStream, Self::RecvStream), Self::Error>> {
		let err = self.side.inc.route(&self.inner, waiter);
		if let Poll::Ready(Ok(((send, back), (recv, forward)))) = self.side.inc.ready_bi.poll_pop(waiter) {
			return Poll::Ready(Ok((self.send(send, back), self.recv(recv, forward))));
		}
		err.map_or(Poll::Pending, |err| Poll::Ready(Err(err)))
	}

	fn poll_open_bi(
		&self,
		waiter: &kio::Waiter,
		options: poll::SendOptions<'_, poll::Ungrouped>,
	) -> Poll<Result<(Self::SendStream, Self::RecvStream), Self::Error>> {
		let link = &self.side.out;
		let mut lines = link.bi.lock().unwrap();
		let (send, recv) = ready!(self.inner.poll_open_bi(waiter, Default::default()))?;
		let held = link.held_bi.lock().unwrap().is_some();
		let forward = Arc::new(Line {
			held,
			..Default::default()
		});
		let back = Arc::new(Line {
			split: self.side.inc.split_responses.lock().unwrap().clone(),
			..Default::default()
		});
		lines.push_back([forward.clone(), back.clone()]);
		drop(lines);

		let mut send = self.send(send, forward);
		poll::SendStream::set_priority(&mut send, options.priority);
		send.withhold_fin = Some(self.side.withhold_bidi_fins.clone());
		send.first_byte = Some(self.side.bidi_types.clone());
		Poll::Ready(Ok((send, self.recv(recv, back))))
	}

	fn poll_open_request(
		&self,
		waiter: &kio::Waiter,
		options: moq_net::transport::poll::SendOptions<
			'_,
			<Self::SendStream as moq_net::transport::poll::SendStream>::Group,
		>,
	) -> std::task::Poll<Result<Option<moq_net::transport::poll::BiStreams<Self>>, Self::Error>> {
		self.poll_open_bi(waiter, options).map_ok(Some)
	}

	fn poll_open_uni(
		&self,
		waiter: &kio::Waiter,
		options: poll::SendOptions<'_, poll::Ungrouped>,
	) -> Poll<Result<Self::SendStream, Self::Error>> {
		if self.side.withheld.load(Ordering::Relaxed) {
			return Poll::Ready(Err(ready!(self.closer.poll_closed(waiter))));
		}

		let link = &self.side.out;
		let mut lines = link.uni.lock().unwrap();
		let send = ready!(self.inner.poll_open_uni(waiter, Default::default()))?;
		let line = Arc::new(Line {
			held: link.held_uni.lock().unwrap().is_some(),
			split: link.split.lock().unwrap().clone(),
			..Default::default()
		});
		lines.push_back(line.clone());
		drop(lines);

		let mut send = self.send(send, line);
		poll::SendStream::set_priority(&mut send, options.priority);
		send.ack = send.line.held;
		Poll::Ready(Ok(send))
	}

	fn send_group(&self, _: i64) -> Option<poll::Ungrouped> {
		None
	}

	fn send_datagram(
		&self,
		payload: Bytes,
		options: poll::SendOptions<'_, poll::Ungrouped>,
	) -> Result<(), Self::Error> {
		if self.side.lossy.load(Ordering::Relaxed) {
			return Ok(());
		}
		self.inner.send_datagram(payload, options)
	}

	fn poll_recv_datagram(&self, waiter: &kio::Waiter) -> Poll<Result<Bytes, Self::Error>> {
		self.inner.poll_recv_datagram(waiter)
	}

	fn max_datagram_size(&self) -> usize {
		self.inner.max_datagram_size()
	}

	fn address(&self) -> moq_net::transport::Address {
		self.inner.address()
	}

	fn protocol(&self) -> Option<&str> {
		self.inner.protocol()
	}

	fn close(&self, code: u32, reason: &str) {
		self.inner.close(code, reason);
	}

	fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Self::Error> {
		self.inner.poll_closed(waiter)
	}

	fn metrics(&self) -> impl moq_net::transport::Metrics {
		assert!(
			!self.side.panic_on_stats.load(Ordering::Relaxed),
			"injected transport panic"
		);
		self.side.stats_reads.fetch_add(1, Ordering::Relaxed);
		*self.side.stats.lock().unwrap()
	}

	fn wire(&self) -> Option<moq_net::transport::Wire> {
		Some(self.side.wire.clone())
	}
}

#[allow(dead_code, reason = "only some test binaries steer delivery")]
impl MockSession {
	/// Writes that parked on the in-memory stream's byte window.
	pub fn blocked_writes(&self) -> usize {
		*self.side.conn.blocked_writes.read()
	}

	/// Wait until a write parks after the supplied count, without polling or sleeping.
	pub async fn blocked_after(&self, before: usize) {
		kio::wait(|waiter| {
			self.side.conn.blocked_writes.poll(waiter, |count| {
				if **count > before {
					Poll::Ready(())
				} else {
					Poll::Pending
				}
			})
		})
		.await;
	}

	/// Streams whose FIN was sent before the connection closed.
	pub fn finished_streams(&self) -> usize {
		self.side.conn.finishes.load(Ordering::Relaxed)
	}

	/// Report `stats` from now on.
	pub fn set_stats(&self, stats: MockStats) {
		*self.side.stats.lock().unwrap() = stats;
	}

	/// How many times the stats were read.
	pub fn stats_reads(&self) -> usize {
		self.side.stats_reads.load(Ordering::Relaxed)
	}

	/// The wire counters this side's transport exposes.
	pub fn wire(&self) -> &moq_net::transport::Wire {
		&self.side.wire
	}

	/// Panic on the driver's next stats sample, which a [`moq_net::Session::stats`]
	/// read requests.
	pub fn panic_on_stats(&self) {
		self.side.panic_on_stats.store(true, Ordering::Relaxed);
	}

	/// Drop the FIN of the bidi streams this side opens, keeping them open.
	pub fn withhold_bidi_fins(&self) {
		self.side.withhold_bidi_fins.store(true, Ordering::Relaxed);
	}

	/// Acknowledge every FIN as soon as it is sent, before the peer reads it, as QUIC does.
	pub fn ack_fins(&self) {
		self.side.conn.ack_fins.store(true, Ordering::Relaxed);
	}

	/// Hold every stream write until the session closes, as a peer with a full receive window.
	pub fn hold_writes(&self) {
		self.side.conn.hold_writes.store(true, Ordering::Relaxed);
	}

	/// Withhold FIN acknowledgements while continuing to deliver stream data.
	pub fn hold_fin_acknowledgements(&self) {
		self.side.conn.hold_fins.store(true, Ordering::Relaxed);
	}

	/// Hold back the uni streams this side opens from now on.
	///
	/// The peer's transport has them, so a FIN is acknowledged at once, but its application
	/// does not see them until [`Self::release_unis`]. That is QUIC delivering streams out of
	/// order: a publisher can see a group stream acknowledged and end the subscription
	/// before the subscriber has read the group's header.
	pub fn hold_unis(&self) {
		self.side.out.held_uni.lock().unwrap().get_or_insert_default();
	}

	/// Deliver the held uni streams to the peer in the order they were opened, and stop
	/// holding.
	pub fn release_unis(&self) {
		self.side.out.release_unis(|_| {});
	}

	/// Deliver only the newest held uni stream, and keep holding the rest. Returns how
	/// many are still held.
	pub fn release_newest_uni(&self) -> usize {
		let link = &self.side.out;
		link.settle();
		let mut held = link.held_uni.lock().unwrap();
		let held = held.as_mut().expect("not holding");
		if let Some(stream) = held.pop() {
			let _ = link.ready_uni.try_push(stream);
		}
		held.len()
	}

	/// Deliver the held uni streams newest first, and stop holding.
	pub fn release_unis_reversed(&self) {
		self.side.out.release_unis(|held| held.reverse());
	}

	/// Deliver the held uni streams highest send order first, as a congested sender
	/// would transmit them, and stop holding. Equal send orders keep their open order.
	pub fn release_unis_by_priority(&self) {
		self.side.out.release_unis(|held| {
			held.sort_by_key(|(_, line)| std::cmp::Reverse(line.priority.load(Ordering::Relaxed)))
		});
	}

	/// Lose the held uni streams, as if each were reset before its header arrived, and
	/// stop holding.
	pub fn drop_unis(&self) {
		self.side.out.release_unis(|held| held.clear());
	}

	/// Hold back the bidi streams this side opens from now on, until
	/// [`Self::release_bidis`]: QUIC does not order streams, so the peer may see a later
	/// stream's data before an earlier stream arrives at all.
	pub fn hold_bidis(&self) {
		self.side.out.held_bi.lock().unwrap().get_or_insert_default();
	}

	/// Deliver the held bidi streams to the peer in the order they were opened, and stop
	/// holding.
	pub fn release_bidis(&self) {
		let link = &self.side.out;
		link.settle();
		for stream in link.held_bi.lock().unwrap().take().unwrap_or_default() {
			let _ = link.ready_bi.try_push(stream);
		}
	}

	/// Park every uni stream this side opens from now on, like a peer that has granted
	/// no more stream credit.
	pub fn withhold_unis(&self) {
		self.side.withheld.store(true, Ordering::Relaxed);
	}

	/// Deliver only the first write of each uni stream this side opens from now on, holding
	/// the rest until [`Self::release_split`]. A group stream's first write is its header,
	/// so the header lands before the group's first frame, as QUIC may split them across
	/// packets.
	pub fn split_unis(&self) {
		*self.side.out.split.lock().unwrap() = Some(Arc::default());
	}

	/// Deliver the first write of new bidi responses, holding the rest until `release_split`.
	pub fn split_responses(&self) {
		*self.side.out.split_responses.lock().unwrap() = Some(Arc::default());
	}

	/// Deliver what `split_unis` or `split_responses` held, and stop splitting.
	pub fn release_split(&self) {
		for gate in [
			self.side.out.split.lock().unwrap().take(),
			self.side.out.split_responses.lock().unwrap().take(),
		]
		.into_iter()
		.flatten()
		{
			*gate.lock() = true;
		}
	}
	/// Lose every datagram this side sends from now on.
	pub fn lose_datagrams(&self) {
		self.side.lossy.store(true, Ordering::Relaxed);
	}

	/// Delay stream data and STOP_SENDING sent from now on by `latency` in each direction,
	/// keeping each stream in order. Measured on the simulated clock, so tests advance
	/// through it without sleeping.
	pub fn set_latency(&self, latency: Duration) {
		*self.side.conn.latency.lock().unwrap() = latency;
	}

	/// The code and reason the connection was closed with, once either side closed it.
	pub fn close_reason(&self) -> Option<(u32, String)> {
		let waiter = &kio::Waiter::noop();
		match poll::Session::poll_closed(&self.inner.clone(), waiter) {
			Poll::Ready(err) => moq_net::transport::Error::session_error(&err),
			Poll::Pending => None,
		}
	}

	/// The first byte, the stream type, of every bidi stream this side has written to,
	/// in the order each was first written.
	pub fn bidi_types(&self) -> Vec<u8> {
		self.side.bidi_types.lock().unwrap().clone()
	}

	/// Every code this side has reset a stream with, in order.
	pub fn resets(&self) -> Vec<u32> {
		self.side.resets.lock().unwrap().clone()
	}
}

// ── Pair constructor ────────────────────────────────────────────────

/// Create a pair of connected mock sessions.
///
/// Streams opened on `client` appear in `server.accept_*()` and vice versa.
/// Both sides report `config.protocol` from
/// [`moq_net::transport::poll::Session::protocol`], matching ALPN negotiation
/// behavior.
pub fn create_mock_session_pair(config: memory::Config) -> (MockSession, MockSession) {
	let (client, server) = memory::pair(config);

	let conn = Arc::new(Conn::default());
	let to_server = Arc::new(Link::new(server.clone()));
	let to_client = Arc::new(Link::new(client.clone()));

	let side = |inner: memory::Session, out: &Arc<Link>, inc: &Arc<Link>| MockSession {
		closer: inner.clone(),
		inner,
		side: Arc::new(Side {
			conn: conn.clone(),
			out: out.clone(),
			inc: inc.clone(),
			withheld: Default::default(),
			lossy: Default::default(),
			withhold_bidi_fins: Default::default(),
			bidi_types: Default::default(),
			resets: Default::default(),
			stats: Default::default(),
			stats_reads: Default::default(),
			panic_on_stats: Default::default(),
			wire: Default::default(),
		}),
	};

	(
		side(client, &to_server, &to_client),
		side(server, &to_client, &to_server),
	)
}
