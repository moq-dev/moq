//! Transport doubles shared by the lite tests: a session whose streams record
//! everything written and every reset code, peers that never speak, peers whose
//! incoming streams die before their first byte, and peers that open unidirectional
//! streams carrying a scripted payload.
//!
//! These implement the poll traits directly, since that is what the crate's
//! transport bound requires. A pending fake simply returns `Poll::Pending`
//! without registering a waker: it models a peer that never answers, so nothing
//! should ever wake it.

use std::{
	sync::{
		Arc, Mutex,
		atomic::{AtomicUsize, Ordering},
	},
	task::Poll,
};

use crate::transport::poll;

/// A failed transport call. After a session close, it carries that close's code, as a
/// real transport reports the code the session ended with.
#[derive(Debug, Clone, Default)]
pub struct SinkError {
	code: u32,
	/// The peer reset this one stream with this code, rather than failing the session.
	stream: Option<u32>,
}

impl std::fmt::Display for SinkError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "sink transport error")
	}
}

impl std::error::Error for SinkError {}

impl crate::transport::Error for SinkError {
	fn session_error(&self) -> Option<(u32, String)> {
		match self.stream {
			Some(_) => None,
			None => Some((self.code, "closed".to_string())),
		}
	}

	fn stream_error(&self) -> Option<u32> {
		self.stream
	}
}

/// A datagram's send group id and in-group order.
pub type Rank = (Option<usize>, i64);

/// What the session's send streams recorded. Resets are a list, not a last-value, so a
/// stray second reset (a drop fallback firing behind an abort) is visible.
#[derive(Clone, Default)]
pub struct Log {
	pub datagrams: Arc<Mutex<Vec<bytes::Bytes>>>,
	/// The send group and priority each datagram was sent with, beside [`Self::datagrams`].
	datagram_ranks: Arc<Mutex<Vec<Rank>>>,
	pub writes: Arc<Mutex<Vec<u8>>>,
	pub resets: Arc<Mutex<Vec<u32>>>,
	/// Every write, FIN and reset on every send stream, in call order. See [`Self::trail`].
	trail: Arc<Mutex<Vec<(usize, Sent)>>>,
	/// Hands each send stream its id in the trail.
	streams: Arc<AtomicUsize>,
	stops: Arc<Mutex<Vec<u32>>>,
	closes: Arc<Mutex<Vec<(u32, String)>>>,
	/// The first session close's code, once one happened.
	closed: kio::Shared<Option<u32>>,
	/// How long the trail and the stops were at the first session close.
	closed_at: Arc<Mutex<Option<(usize, usize)>>>,
	bi_opens: Arc<AtomicUsize>,
	pub request_attempts: Arc<AtomicUsize>,
	priorities: Arc<Mutex<Vec<u8>>>,
	/// Hands each send group its id.
	group_ids: Arc<AtomicUsize>,
	groups: Arc<Mutex<Vec<(usize, i64)>>>,
	joins: Arc<Mutex<Vec<(usize, usize)>>>,
	orders: Arc<Mutex<Vec<(usize, i64)>>>,
}

/// One thing a send stream did, as recorded in [`Log::trail`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sent {
	Write(Vec<u8>),
	Finish,
	Reset(u32),
}

/// What the streams sent ahead of the first session close. See [`Log::before_close`].
#[derive(Debug)]
pub struct BeforeClose {
	pub trail: Vec<(usize, Sent)>,
	pub stops: Vec<u32>,
}

impl Log {
	fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<SinkError> {
		self.closed
			.poll(waiter, |closed| match **closed {
				Some(_) => Poll::Ready(()),
				None => Poll::Pending,
			})
			.map(|closed| SinkError {
				code: closed.expect("closed"),
				stream: None,
			})
	}

	/// What every send stream did, in call order, tagged with the stream's id (its
	/// creation order). Unlike [`Self::writes`] and [`Self::resets`], this keeps the
	/// order across streams, so a test can check one stream's message against
	/// another stream's FIN or reset.
	pub fn trail(&self) -> Vec<(usize, Sent)> {
		self.trail.lock().unwrap().clone()
	}

	fn record(&self, stream: usize, sent: Sent) {
		self.trail.lock().unwrap().push((stream, sent));
	}

	pub fn resets(&self) -> Vec<u32> {
		self.resets.lock().unwrap().clone()
	}

	/// The STOP_SENDING codes sent on the session's receive streams, in call order.
	/// Cancelling a request is a reset of what we send plus one of these on what we
	/// receive, so a test for a cancellation has to be able to see them.
	///
	/// Only [`ScriptedRecv`] records them. The other receive streams here discard the code,
	/// so an empty result on those is not evidence that nothing was sent.
	pub fn stops(&self) -> Vec<u32> {
		self.stops.lock().unwrap().clone()
	}

	/// Every value handed to the transport's `set_priority`, in call order. These are
	/// trait-contract send orders (higher = transmitted first), recorded so tests can
	/// verify priority conversions at their protocol boundaries.
	pub fn priorities(&self) -> Vec<u8> {
		self.priorities.lock().unwrap().clone()
	}

	/// Every send group's priority, as (group, priority) in call order: its opening, then
	/// each move.
	pub fn groups(&self) -> Vec<(usize, i64)> {
		self.groups.lock().unwrap().clone()
	}

	/// Every send group a stream joined, as (stream, group) in call order.
	pub fn joins(&self) -> Vec<(usize, usize)> {
		self.joins.lock().unwrap().clone()
	}

	/// Every priority set on a stream in a send group, as (stream, priority) in call
	/// order. [`Self::priorities`] keeps those of the streams outside every group.
	pub fn orders(&self) -> Vec<(usize, i64)> {
		self.orders.lock().unwrap().clone()
	}

	/// The send group and priority of every datagram, as (group, priority) in call order.
	pub fn datagram_ranks(&self) -> Vec<Rank> {
		self.datagram_ranks.lock().unwrap().clone()
	}

	fn record_datagram(&self, payload: bytes::Bytes, options: poll::SendOptions<'_, SinkGroup>) {
		self.datagrams.lock().unwrap().push(payload);
		self.datagram_ranks
			.lock()
			.unwrap()
			.push((options.group.map(|group| group.id), options.priority));
	}

	/// The session-level closes, as (code, reason). A list rather than a last-value so
	/// a second close racing the first is visible.
	pub fn closes(&self) -> Vec<(u32, String)> {
		self.closes.lock().unwrap().clone()
	}

	/// The [`Self::trail`] and [`Self::stops`] up to the first session close, or `None`
	/// before one. Nothing a stream sends after the close reaches the peer, so this is
	/// what a peer sees ahead of the close.
	pub fn before_close(&self) -> Option<BeforeClose> {
		let (trail, stops) = (*self.closed_at.lock().unwrap())?;
		Some(BeforeClose {
			trail: self.trail()[..trail].to_vec(),
			stops: self.stops()[..stops].to_vec(),
		})
	}

	fn close(&self, code: u32, reason: &str) {
		self.closed_at
			.lock()
			.unwrap()
			.get_or_insert_with(|| (self.trail.lock().unwrap().len(), self.stops.lock().unwrap().len()));
		self.closes.lock().unwrap().push((code, reason.to_owned()));
		self.closed.lock().get_or_insert(code);
	}

	/// How many bidi streams the session has opened, so a test can pin down how many
	/// requests were sent and not just what they said.
	pub fn bi_opens(&self) -> usize {
		self.bi_opens.load(Ordering::Relaxed)
	}
}

pub struct SinkSend {
	prefix: usize,
	pub log: Log,
	/// This stream's id in [`Log::trail`].
	id: usize,
	/// Writes park until this flips to true; `None` writes immediately. See
	/// [`SinkSession::gated_bi`].
	gate: Option<kio::Consumer<bool>>,
	budget: Option<kio::Consumer<usize>>,
	writes: usize,
	/// Set by [`finish`](poll::SendStream::finish). A finished stream is what
	/// [`poll_closed`](poll::SendStream::poll_closed) waits on, mirroring a peer that
	/// acknowledges the FIN; an unfinished one parks like a peer that never answers.
	finished: bool,
	/// A finished stream still parks, like a peer that has not acknowledged the FIN.
	unacked_fin: bool,
	/// The send group the stream joined.
	group: Option<usize>,
}

/// A send group of a [`SinkSession::with_groups`], recording its moves in the [`Log`].
#[derive(Clone)]
pub struct SinkGroup {
	id: usize,
	log: Log,
}

impl SinkGroup {
	fn open(log: Log, priority: i64) -> Self {
		let id = log.group_ids.fetch_add(1, Ordering::Relaxed);
		log.groups.lock().unwrap().push((id, priority));
		Self { id, log }
	}
}

impl poll::SendGroup for SinkGroup {
	fn set_priority(&mut self, priority: i64) {
		self.log.groups.lock().unwrap().push((self.id, priority));
	}
}

impl SinkSend {
	/// Schedule a stream as it opens, logging only what differs from a fresh stream's
	/// ungrouped priority 0, as the setters would.
	fn opened(mut self, options: poll::SendOptions<'_, SinkGroup>) -> Self {
		use poll::SendStream as _;
		if let Some(group) = options.group {
			self.set_group(group);
		}
		if options.group.is_some() || options.priority != 0 {
			self.set_priority(options.priority);
		}
		self
	}

	pub fn new(log: Log) -> Self {
		Self {
			prefix: 0,
			id: log.streams.fetch_add(1, Ordering::Relaxed),
			log,
			gate: None,
			budget: None,
			writes: 0,
			finished: false,
			unacked_fin: false,
			group: None,
		}
	}

	/// A send stream that accepts only the permitted number of writes.
	pub fn stepped(log: Log, budget: kio::Consumer<usize>) -> Self {
		Self {
			budget: Some(budget),
			..Self::new(log)
		}
	}

	/// A send stream whose writes park until `gate` flips to true.
	pub fn gated(log: Log, gate: kio::Consumer<bool>) -> Self {
		Self {
			prefix: 0,
			id: log.streams.fetch_add(1, Ordering::Relaxed),
			log,
			gate: Some(gate),
			budget: None,
			writes: 0,
			finished: false,
			unacked_fin: false,
			group: None,
		}
	}
}

impl poll::SendStream for SinkSend {
	type Error = SinkError;
	type Group = SinkGroup;

	fn poll_write(&mut self, waiter: &kio::Waiter, buf: &[u8]) -> Poll<Result<usize, Self::Error>> {
		if self.prefix == 0
			&& let Some(gate) = &self.gate
		{
			// A closed gate parks forever, which surfaces as a hung test rather than a
			// write that silently skipped the gate.
			match gate.poll(waiter, |open| if **open { Poll::Ready(()) } else { Poll::Pending }) {
				Poll::Ready(Ok(())) => {}
				Poll::Ready(Err(_)) | Poll::Pending => return Poll::Pending,
			}
		}
		if let Some(budget) = &self.budget {
			match budget.poll(waiter, |limit| {
				if self.writes < **limit {
					Poll::Ready(())
				} else {
					Poll::Pending
				}
			}) {
				Poll::Ready(Ok(_)) => {}
				_ => return Poll::Pending,
			}
		}
		self.writes += 1;
		let buf = if self.prefix > 0 {
			let take = buf.len().min(self.prefix);
			self.prefix -= take;
			&buf[..take]
		} else {
			buf
		};
		self.log.writes.lock().unwrap().extend_from_slice(buf);
		self.log.record(self.id, Sent::Write(buf.to_vec()));
		Poll::Ready(Ok(buf.len()))
	}

	fn set_priority(&mut self, order: i64) {
		if self.group.is_some() {
			self.log.orders.lock().unwrap().push((self.id, order));
			return;
		}
		let order = u8::try_from(order).expect("moq-net sends u8 send orders outside a group");
		self.log.priorities.lock().unwrap().push(order);
	}

	fn set_group(&mut self, group: &Self::Group) {
		self.group = Some(group.id);
		self.log.joins.lock().unwrap().push((self.id, group.id));
	}

	fn finish(&mut self) -> Result<(), Self::Error> {
		self.finished = true;
		self.log.record(self.id, Sent::Finish);
		Ok(())
	}

	/// Always recorded, even after a finish: quinn resets a stream that has sent its FIN but
	/// still has unacknowledged data, which is exactly the case that loses a final message.
	fn reset(&mut self, code: u32) {
		self.log.resets.lock().unwrap().push(code);
		self.log.record(self.id, Sent::Reset(code));
	}

	fn poll_closed(&self, _waiter: &kio::Waiter) -> Poll<Result<(), Self::Error>> {
		match self.finished && !self.unacked_fin {
			true => Poll::Ready(Ok(())),
			// Nothing to acknowledge yet, so park like a peer that never answers.
			false => Poll::Pending,
		}
	}
}

/// A peer that never speaks and never closes, so a test only ever acts on
/// model-side events.
pub struct PendingRecv;

impl poll::RecvStream for PendingRecv {
	type Error = SinkError;
	// Never handed out.
	type Lease<'a> = poll::BufferedLease<'a>;

	fn poll_lease(&mut self, _waiter: &kio::Waiter, _want: usize) -> Poll<Result<Self::Lease<'_>, Self::Error>> {
		Poll::Pending
	}

	fn set_priority(&mut self, _priority: i64) {}

	fn stop(&mut self, _code: u32) {}
}

/// A RESET_STREAM as the transport reports it. `Some(code)` decodes as
/// `Error::Stream`; `None` is a code the transport could not place in the stream
/// registry, which decodes as `Error::Transport`.
#[derive(Debug, Clone, Copy)]
pub struct ResetError(pub Option<u32>);

impl std::fmt::Display for ResetError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self.0 {
			Some(code) => write!(f, "stream reset by peer (code {code})"),
			None => write!(f, "stream reset by peer (unmapped code)"),
		}
	}
}

impl std::error::Error for ResetError {}

impl crate::transport::Error for ResetError {
	fn session_error(&self) -> Option<(u32, String)> {
		None
	}

	fn stream_error(&self) -> Option<u32> {
		self.0
	}
}

/// A stream that died before delivering a single byte: every read reports a
/// RESET_STREAM ([`ResetError`]), the wire shape of a reset arriving ahead of any
/// payload. QUIC does not order a reset behind the data, so this reaches an accept
/// loop in normal operation, not just from a misbehaving peer.
pub struct DeadRecv(ResetError);

impl poll::RecvStream for DeadRecv {
	type Error = ResetError;
	// Never handed out.
	type Lease<'a> = poll::BufferedLease<'a>;

	fn poll_lease(&mut self, _waiter: &kio::Waiter, _want: usize) -> Poll<Result<Self::Lease<'_>, Self::Error>> {
		Poll::Ready(Err(self.0))
	}

	fn set_priority(&mut self, _priority: i64) {}

	fn stop(&mut self, _code: u32) {}
}

/// Incoming streams that die before their first byte: `accept_uni` / `accept_bi`
/// each yield the configured number of [`DeadRecv`] streams and then park like a
/// peer with nothing more to say. Sends record to [`Log`] like [`SinkSession`].
#[derive(Clone)]
pub struct DeadStreamSession {
	/// What the session's send streams recorded, for test assertions.
	pub log: Log,
	unis: Arc<Mutex<usize>>,
	bis: Arc<Mutex<usize>>,
	reset: ResetError,
}

impl DeadStreamSession {
	/// `count` uni streams that die before their first byte, then silence.
	pub fn unis(count: usize) -> Self {
		Self {
			log: Log::default(),
			unis: Arc::new(Mutex::new(count)),
			bis: Arc::new(Mutex::new(0)),
			reset: ResetError(Some(0)),
		}
	}

	/// `count` bidi streams that die before their first byte, then silence.
	pub fn bis(count: usize) -> Self {
		Self {
			log: Log::default(),
			unis: Arc::new(Mutex::new(0)),
			bis: Arc::new(Mutex::new(count)),
			reset: ResetError(Some(0)),
		}
	}

	/// Reset with a code the transport cannot map, the way a raw QUIC moq-transport
	/// peer's CANCELLED reads through the WebTransport code space.
	pub fn unmapped(mut self) -> Self {
		self.reset = ResetError(None);
		self
	}

	fn take(counter: &Mutex<usize>) -> bool {
		let mut remaining = counter.lock().unwrap();
		match *remaining {
			0 => false,
			_ => {
				*remaining -= 1;
				true
			}
		}
	}
}

impl poll::Session for DeadStreamSession {
	type SendStream = SinkSend;
	type RecvStream = DeadRecv;
	type Error = SinkError;

	fn poll_accept_uni(&self, waiter: &kio::Waiter) -> Poll<Result<Self::RecvStream, Self::Error>> {
		match Self::take(&self.unis) {
			true => Poll::Ready(Ok(DeadRecv(self.reset))),
			false => self.log.poll_closed(waiter).map(Err),
		}
	}

	fn poll_accept_bi(&self, waiter: &kio::Waiter) -> Poll<Result<poll::BiStreams<Self>, Self::Error>> {
		match Self::take(&self.bis) {
			true => Poll::Ready(Ok((SinkSend::new(self.log.clone()), DeadRecv(self.reset)))),
			false => self.log.poll_closed(waiter).map(Err),
		}
	}

	fn poll_open_bi(
		&self,
		_waiter: &kio::Waiter,
		options: poll::SendOptions<'_, SinkGroup>,
	) -> Poll<Result<poll::BiStreams<Self>, Self::Error>> {
		self.log.bi_opens.fetch_add(1, Ordering::Relaxed);
		Poll::Ready(Ok((
			SinkSend::new(self.log.clone()).opened(options),
			DeadRecv(self.reset),
		)))
	}

	fn poll_open_request(
		&self,
		waiter: &kio::Waiter,
		options: crate::transport::poll::SendOptions<
			'_,
			<Self::SendStream as crate::transport::poll::SendStream>::Group,
		>,
	) -> std::task::Poll<Result<Option<crate::transport::poll::BiStreams<Self>>, Self::Error>> {
		self.poll_open_bi(waiter, options).map_ok(Some)
	}

	fn poll_open_uni(
		&self,
		_waiter: &kio::Waiter,
		options: poll::SendOptions<'_, SinkGroup>,
	) -> Poll<Result<Self::SendStream, Self::Error>> {
		Poll::Ready(Ok(SinkSend::new(self.log.clone()).opened(options)))
	}

	fn send_group(&self, _: i64) -> Option<SinkGroup> {
		None
	}

	fn send_datagram(
		&self,
		payload: bytes::Bytes,
		options: poll::SendOptions<'_, SinkGroup>,
	) -> Result<(), Self::Error> {
		self.log.record_datagram(payload, options);
		Ok(())
	}

	fn poll_recv_datagram(&self, _waiter: &kio::Waiter) -> Poll<Result<bytes::Bytes, Self::Error>> {
		Poll::Pending
	}

	fn max_datagram_size(&self) -> usize {
		0
	}

	fn address(&self) -> crate::transport::Address {
		crate::transport::Address::Setup
	}
	fn protocol(&self) -> Option<&str> {
		None
	}

	fn close(&self, code: u32, reason: &str) {
		self.log.close(code, reason);
	}

	fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Self::Error> {
		self.log.poll_closed(waiter)
	}

	fn metrics(&self) -> impl crate::transport::Metrics {
		SinkStats::default()
	}
}

/// Send streams record their bytes while peer reads park forever.
#[derive(Clone, Default)]
pub struct SinkSession {
	bi_prefix: usize,
	pub log: Log,
	/// Set by [`Self::gated_bi`]. `None` parks `open_bi` itself forever, which is all
	/// a test driving only uni streams needs.
	bi_gate: Option<kio::Consumer<bool>>,
	/// Set by [`Self::accepted_bi`]. `None` parks `accept_bi` forever, which is what
	/// every session that only ever opens its own streams expects.
	accept_gate: Option<kio::Consumer<bool>>,
	/// Set by [`Self::with_open_bi_gate`] to withhold bidi stream credit.
	bi_open_gate: Option<kio::Consumer<bool>>,
	/// Set by [`Self::gated_uni`] to hold unidirectional stream writes.
	uni_gate: Option<kio::Consumer<bool>>,
	/// Set by [`Self::gated_open_uni`] to withhold unidirectional stream credit.
	uni_open_gate: Option<kio::Consumer<bool>>,
	/// Set by [`Self::with_unacked_fin`].
	unacked_fin: bool,
	/// Set by [`Self::with_groups`].
	groups: bool,
	/// Set by [`Self::with_datagrams`]; zero disables datagrams.
	datagram_size: usize,
	/// The ALPN to report, for a test that needs a specific negotiated version rather
	/// than the SETUP-negotiated fallback an absent one selects.
	protocol: Option<&'static str>,
	/// What the transport claims to measure. Defaults to nothing, like a transport
	/// with no congestion controller exposed. Shared and mutable so a test can
	/// change it mid-session, the way a real transport's figures move.
	stats: Arc<Mutex<SinkStats>>,
	/// Set by [`Self::with_probe`].
	probe: Option<crate::transport::Probe>,
}

impl SinkSession {
	pub fn new(log: Log) -> Self {
		Self {
			log,
			..Default::default()
		}
	}

	/// Never acknowledge a unidirectional stream's FIN, like a congested peer, so a
	/// finished group stays waiting on it.
	pub fn with_unacked_fin(mut self) -> Self {
		self.unacked_fin = true;
		self
	}

	/// Open send groups, as native QUIC does, recording them in the log.
	pub fn with_groups(mut self) -> Self {
		self.groups = true;
		self
	}

	/// Accept datagrams up to `max_size` bytes, recording them in the log.
	pub fn with_datagrams(mut self, max_size: usize) -> Self {
		self.datagram_size = max_size;
		self
	}

	/// Give the transport a sender for the session to hand its probe target.
	pub fn with_probe(mut self) -> Self {
		self.probe = Some(crate::transport::Probe::new());
		self
	}

	/// Report these connection statistics, as a real transport would.
	pub fn with_stats(self, stats: SinkStats) -> Self {
		self.set_stats(stats);
		self
	}

	/// Change what the transport reports, mid-session.
	pub fn set_stats(&self, stats: SinkStats) {
		*self.stats.lock().unwrap() = stats;
	}

	/// Report `protocol` as the negotiated ALPN.
	pub fn with_protocol(mut self, protocol: &'static str) -> Self {
		self.protocol = Some(protocol);
		self
	}

	/// Serve bidi streams, holding every write until `gate` flips to true.
	///
	/// The pause is the point: it lets a test assert what must already be true at the
	/// instant the first byte would reach the wire, without the transport calling back
	/// into the test.
	pub fn gated_bi(gate: kio::Consumer<bool>) -> Self {
		Self {
			bi_gate: Some(gate),
			..Default::default()
		}
	}

	/// Accept bidi streams from the peer, holding every write until `gate` flips to true.
	///
	/// The accepted half of a control stream carries the answers (track info,
	/// subscribe responses), so a test of what must be true before the first byte of
	/// a reply reaches the wire needs a session that hands out accepted streams.
	pub fn accepted_bi(gate: kio::Consumer<bool>) -> Self {
		Self {
			accept_gate: Some(gate),
			..Default::default()
		}
	}

	/// Open unidirectional streams immediately, holding their writes until `gate` opens.
	pub fn gated_uni(gate: kio::Consumer<bool>) -> Self {
		Self {
			uni_gate: Some(gate),
			..Default::default()
		}
	}

	/// Hold unidirectional stream opens until `gate` grants stream credit.
	pub fn gated_open_uni(gate: kio::Consumer<bool>) -> Self {
		Self::default().with_open_uni_gate(gate)
	}

	/// Also hold unidirectional stream opens until `gate` grants stream credit.
	pub fn with_open_uni_gate(mut self, gate: kio::Consumer<bool>) -> Self {
		self.uni_open_gate = Some(gate);
		self
	}

	/// Also hold bidi stream opens until `gate` grants stream credit.
	pub fn with_open_bi_gate(mut self, gate: kio::Consumer<bool>) -> Self {
		self.bi_open_gate = Some(gate);
		self
	}
}

impl poll::Session for SinkSession {
	type SendStream = SinkSend;
	type RecvStream = PendingRecv;
	type Error = SinkError;

	fn poll_accept_uni(&self, waiter: &kio::Waiter) -> Poll<Result<Self::RecvStream, Self::Error>> {
		self.log.poll_closed(waiter).map(Err)
	}

	fn poll_accept_bi(&self, _waiter: &kio::Waiter) -> Poll<Result<poll::BiStreams<Self>, Self::Error>> {
		let Some(gate) = self.accept_gate.clone() else {
			return Poll::Pending;
		};

		let send = SinkSend::gated(self.log.clone(), gate);
		Poll::Ready(Ok((send, PendingRecv)))
	}

	fn poll_open_bi(
		&self,
		waiter: &kio::Waiter,
		options: poll::SendOptions<'_, SinkGroup>,
	) -> Poll<Result<poll::BiStreams<Self>, Self::Error>> {
		let Some(gate) = self.bi_gate.clone() else {
			return Poll::Pending;
		};
		if let Some(credit) = &self.bi_open_gate {
			match credit.poll(waiter, |open| (**open).then_some(()).map_or(Poll::Pending, Poll::Ready)) {
				Poll::Ready(Ok(())) => {}
				Poll::Ready(Err(_)) | Poll::Pending => return Poll::Pending,
			}
		}
		self.log.bi_opens.fetch_add(1, Ordering::Relaxed);

		let mut send = SinkSend::gated(self.log.clone(), gate).opened(options);
		send.prefix = self.bi_prefix;
		Poll::Ready(Ok((send, PendingRecv)))
	}

	fn poll_open_request(
		&self,
		waiter: &kio::Waiter,
		options: crate::transport::poll::SendOptions<
			'_,
			<Self::SendStream as crate::transport::poll::SendStream>::Group,
		>,
	) -> std::task::Poll<Result<Option<crate::transport::poll::BiStreams<Self>>, Self::Error>> {
		self.poll_open_bi(waiter, options).map_ok(Some)
	}

	fn poll_open_uni(
		&self,
		waiter: &kio::Waiter,
		options: poll::SendOptions<'_, SinkGroup>,
	) -> Poll<Result<Self::SendStream, Self::Error>> {
		if let Some(gate) = &self.uni_open_gate {
			match gate.poll(waiter, |open| (**open).then_some(()).map_or(Poll::Pending, Poll::Ready)) {
				Poll::Ready(Ok(())) => {}
				Poll::Ready(Err(_)) | Poll::Pending => return Poll::Pending,
			}
		}
		let mut send = match &self.uni_gate {
			Some(gate) => SinkSend::gated(self.log.clone(), gate.clone()),
			None => SinkSend::new(self.log.clone()),
		};
		send.unacked_fin = self.unacked_fin;
		Poll::Ready(Ok(send.opened(options)))
	}

	fn send_group(&self, priority: i64) -> Option<SinkGroup> {
		self.groups.then(|| SinkGroup::open(self.log.clone(), priority))
	}

	fn send_datagram(
		&self,
		payload: bytes::Bytes,
		options: poll::SendOptions<'_, SinkGroup>,
	) -> Result<(), Self::Error> {
		self.log.record_datagram(payload, options);
		Ok(())
	}

	fn poll_recv_datagram(&self, _waiter: &kio::Waiter) -> Poll<Result<bytes::Bytes, Self::Error>> {
		Poll::Pending
	}

	fn max_datagram_size(&self) -> usize {
		self.datagram_size
	}

	fn address(&self) -> crate::transport::Address {
		crate::transport::Address::Setup
	}
	fn protocol(&self) -> Option<&str> {
		self.protocol
	}

	fn close(&self, code: u32, reason: &str) {
		self.log.close(code, reason);
	}

	fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Self::Error> {
		self.log.poll_closed(waiter)
	}

	fn metrics(&self) -> impl crate::transport::Metrics {
		*self.stats.lock().unwrap()
	}

	fn probe(&self) -> Option<crate::transport::Probe> {
		self.probe.clone()
	}
}

/// Connection statistics a test can dictate. Every metric defaults to unknown,
/// matching a transport that exposes no congestion controller.
#[derive(Default, Clone, Copy)]
pub struct SinkStats {
	pub estimated_send_rate: Option<u64>,
	pub rtt: Option<std::time::Duration>,
}

impl SinkStats {
	/// Report a send-rate estimate, in bits per second.
	pub fn with_send_rate(mut self, rate: u64) -> Self {
		self.estimated_send_rate = Some(rate);
		self
	}

	/// Report a round-trip time.
	pub fn with_rtt(mut self, rtt: std::time::Duration) -> Self {
		self.rtt = Some(rtt);
		self
	}
}

impl crate::transport::Metrics for SinkStats {
	fn estimated_send_rate(&self) -> Option<u64> {
		self.estimated_send_rate
	}

	fn rtt(&self) -> Option<std::time::Duration> {
		self.rtt
	}
}

/// A peer that replays a canned byte script and then goes quiet, so a test can drive
/// a response arm end to end instead of duplicating its decoding.
///
/// Parks once the script is exhausted rather than reporting EOF: a read loop that saw
/// EOF would exit, and a test usually wants to assert against the loop still running.
pub type ScriptedRecv = poll::Buffered<Script>;

/// The plain reads of a [`ScriptedRecv`].
pub struct Script {
	script: Arc<Mutex<Vec<u8>>>,
	/// How the peer's send side ends once the script runs out; `None` parks.
	close: Arc<Mutex<Option<Close>>>,
	log: Log,
}

/// How a scripted peer closes its send side. See [`ScriptedSession::close`].
#[derive(Clone, Copy, Debug)]
pub enum Close {
	Fin,
	/// Fail the read as a session close would.
	Reset,
	/// Reset just this stream with a code, as a peer that gave up on one stream would.
	ResetStream(u32),
}

impl poll::Chunks for Script {
	type Error = SinkError;

	fn poll_read_chunk(&mut self, waiter: &kio::Waiter, max: usize) -> Poll<Result<Option<bytes::Bytes>, Self::Error>> {
		let chunk: bytes::Bytes = {
			let mut script = self.script.lock().unwrap();
			// A real transport hands over what each packet carried, so a body that is all
			// ready at once still takes many reads.
			let take = max.min(script.len()).min(8 * 1024);
			script.drain(..take).collect()
		};

		match chunk.is_empty() {
			true => match *self.close.lock().unwrap() {
				Some(Close::Fin) => Poll::Ready(Ok(None)),
				Some(Close::Reset) => Poll::Ready(Err(SinkError::default())),
				Some(Close::ResetStream(code)) => Poll::Ready(Err(SinkError {
					stream: Some(code),
					..Default::default()
				})),
				None => self.log.poll_closed(waiter).map(Err),
			},
			false => Poll::Ready(Ok(Some(chunk))),
		}
	}

	fn stop(&mut self, code: u32) {
		self.log.stops.lock().unwrap().push(code);
	}
}

/// Records what we send while replaying a scripted peer response on every stream.
///
/// The script is shared across streams, so a test that only opens one gets exactly
/// what it wrote; drive one stream at a time. [`Self::per_stream`] hands each
/// opened stream its own script instead, for flows holding several requests open
/// at once.
#[derive(Clone)]
pub struct ScriptedSession {
	pub log: Log,
	stats: SinkStats,
	/// Shared with every stream, like `script`. See [`Self::close`].
	close: Arc<Mutex<Option<Close>>>,
	script: Arc<Mutex<Vec<u8>>>,
	/// Per-stream scripts popped by `open_bi` in order; `None` shares `script`
	/// across every stream.
	queue: Option<Arc<Mutex<std::collections::VecDeque<Vec<u8>>>>>,
	/// Set by [`Self::gated_open`]: parks `poll_open_bi` until the gate opens, standing in
	/// for a transport header blocked on send capacity.
	open_gate: Option<kio::Consumer<bool>>,
	credit: kio::Producer<bool>,
	/// Scripts the peer pushes at us on unidirectional streams, popped by `accept_uni`
	/// in order. See [`Self::with_incoming_unis`].
	incoming_unis: Arc<Mutex<std::collections::VecDeque<Vec<u8>>>>,
	incoming_bidis: Arc<Mutex<std::collections::VecDeque<Vec<u8>>>>,
	/// Set by [`Self::with_unacked_fin`].
	unacked_fin: bool,
	/// Set by [`Self::with_unacked_uni_fins`]; shared, so every clone counts down one budget.
	unacked_unis: Arc<AtomicUsize>,
	datagram_size: usize,
	/// Set by [`Self::with_groups`].
	groups: bool,
	/// Set by [`Self::with_send_window`].
	send_window: Option<usize>,
}

impl ScriptedSession {
	pub fn without_credit(self) -> Self {
		*self.credit.write().ok().expect("credit is live") = false;
		self
	}

	pub fn grant_credit(&self) {
		*self.credit.write().ok().expect("credit is live") = true;
	}

	pub fn with_stats(mut self, stats: SinkStats) -> Self {
		self.stats = stats;
		self
	}

	pub fn with_datagrams(mut self, max_size: usize) -> Self {
		self.datagram_size = max_size;
		self
	}

	pub fn new(script: Vec<u8>) -> Self {
		Self {
			log: Log::default(),
			stats: SinkStats::default(),
			close: Default::default(),
			script: Arc::new(Mutex::new(script)),
			queue: None,
			open_gate: None,
			credit: kio::Producer::new(true),
			incoming_unis: Arc::new(Mutex::new(std::collections::VecDeque::new())),
			incoming_bidis: Arc::new(Mutex::new(std::collections::VecDeque::new())),
			unacked_fin: false,
			unacked_unis: Default::default(),
			datagram_size: 0,
			groups: false,
			send_window: None,
		}
	}

	/// Let each opened bidi stream send only its first `bytes`, then park its writes for good,
	/// like a peer that never grants more send credit.
	pub fn with_send_window(mut self, bytes: usize) -> Self {
		self.send_window = Some(bytes);
		self
	}

	/// Open send groups, as native QUIC does, recording them in the log.
	pub fn with_groups(mut self) -> Self {
		self.groups = true;
		self
	}

	/// Never acknowledge an opened stream's FIN, like a peer that stopped reading.
	pub fn with_unacked_fin(mut self) -> Self {
		self.unacked_fin = true;
		self
	}

	/// Never acknowledge the FIN of the next `count` unidirectional streams opened, like a
	/// peer stalled on those alone.
	pub fn with_unacked_uni_fins(self, count: usize) -> Self {
		self.unacked_unis.store(count, Ordering::Relaxed);
		self
	}

	/// Have the peer open one unidirectional stream per script, in order, each replaying
	/// its bytes and then going quiet (or reporting EOF, on an `eof` session).
	///
	/// This is the only double here whose `accept_uni` yields anything readable, so it is
	/// what a test of an incoming-stream dispatch loop drives: the loop sees a real stream
	/// carrying a real header, and [`Log::stops`] then records the STOP_SENDING the loop
	/// answers with. Once the scripts run out, `accept_uni` parks like a peer with nothing
	/// more to open.
	pub fn with_incoming_unis(mut self, scripts: Vec<Vec<u8>>) -> Self {
		self.incoming_unis = Arc::new(Mutex::new(scripts.into_iter().collect()));
		self
	}

	/// Have the peer open one bidirectional stream per script, then go quiet.
	pub fn with_incoming_bidis(mut self, scripts: Vec<Vec<u8>>) -> Self {
		self.incoming_bidis = Arc::new(Mutex::new(scripts.into_iter().collect()));
		self
	}

	/// Replay `script`, then close the stream instead of parking.
	///
	/// Parking is the right default for asserting that a loop is still running, but a
	/// test for what a loop does on the way *out* needs the read to actually end.
	pub fn eof(script: Vec<u8>) -> Self {
		let session = Self::new(script);
		session.close(Close::Fin);
		session
	}

	/// Each `open_bi` replays the next script in order. An exhausted queue (or an
	/// empty entry) reads as a peer that never speaks.
	pub fn per_stream(scripts: Vec<Vec<u8>>) -> Self {
		Self {
			queue: Some(Arc::new(Mutex::new(scripts.into_iter().collect()))),
			..Self::new(Vec::new())
		}
	}

	/// Like [`Self::per_stream`], but an exhausted script closes the stream instead of
	/// parking, for a test that drives each stream to its end.
	pub fn per_stream_eof(scripts: Vec<Vec<u8>>) -> Self {
		let session = Self::per_stream(scripts);
		session.close(Close::Fin);
		session
	}

	/// Like [`Self::per_stream`], but an exhausted script resets the stream, as a peer
	/// that fails after replying would.
	pub fn per_stream_reset(scripts: Vec<Vec<u8>>) -> Self {
		let session = Self::per_stream(scripts);
		session.close(Close::Reset);
		session
	}

	/// Append to the shared script: the peer sending more on a stream it already opened.
	/// Nothing is woken, so the test re-polls the reader itself.
	pub fn push(&self, bytes: &[u8]) {
		self.script.lock().unwrap().extend_from_slice(bytes);
	}

	/// Close the peer's send side once the script runs out. Nothing is woken, so the
	/// test re-polls the reader itself.
	pub fn close(&self, close: Close) {
		*self.close.lock().unwrap() = Some(close);
	}

	/// Answer each stream from `scripts`, but only once the gate opens: a peer that
	/// replies normally after its transport header gets send capacity.
	pub fn gated_open(scripts: Vec<Vec<u8>>, gate: kio::Consumer<bool>) -> Self {
		Self {
			open_gate: Some(gate),
			..Self::per_stream(scripts)
		}
	}
}

impl poll::Session for ScriptedSession {
	type SendStream = SinkSend;
	type RecvStream = ScriptedRecv;
	type Error = SinkError;

	fn poll_accept_uni(&self, waiter: &kio::Waiter) -> Poll<Result<Self::RecvStream, Self::Error>> {
		let Some(script) = self.incoming_unis.lock().unwrap().pop_front() else {
			return self.log.poll_closed(waiter).map(Err);
		};
		Poll::Ready(Ok(poll::Buffered::new(Script {
			script: Arc::new(Mutex::new(script)),
			close: self.close.clone(),
			log: self.log.clone(),
		})))
	}

	fn poll_accept_bi(&self, waiter: &kio::Waiter) -> Poll<Result<poll::BiStreams<Self>, Self::Error>> {
		let Some(script) = self.incoming_bidis.lock().unwrap().pop_front() else {
			return self.log.poll_closed(waiter).map(Err);
		};
		Poll::Ready(Ok((
			SinkSend::new(self.log.clone()),
			poll::Buffered::new(Script {
				script: Arc::new(Mutex::new(script)),
				close: self.close.clone(),
				log: self.log.clone(),
			}),
		)))
	}

	fn poll_open_bi(
		&self,
		waiter: &kio::Waiter,
		options: poll::SendOptions<'_, SinkGroup>,
	) -> Poll<Result<poll::BiStreams<Self>, Self::Error>> {
		let credit = self.credit.consume();
		if credit
			.poll(waiter, |credit| if **credit { Poll::Ready(()) } else { Poll::Pending })
			.is_pending()
		{
			return Poll::Pending;
		}
		if let Some(gate) = self.open_gate.clone() {
			// A closed gate reads as open: the test dropped the producer, so nothing is
			// holding the stream back anymore.
			if gate
				.poll(waiter, |open| if **open { Poll::Ready(()) } else { Poll::Pending })
				.is_pending()
			{
				return Poll::Pending;
			}
		}

		self.log.bi_opens.fetch_add(1, Ordering::Relaxed);
		let script = match &self.queue {
			Some(queue) => Arc::new(Mutex::new(queue.lock().unwrap().pop_front().unwrap_or_default())),
			None => self.script.clone(),
		};
		let mut send = match self.send_window {
			// The gate's producer is gone, so the writes past the prefix park forever.
			Some(window) => SinkSend {
				prefix: window,
				..SinkSend::gated(self.log.clone(), kio::Producer::new(false).consume())
			},
			None => SinkSend::new(self.log.clone()),
		}
		.opened(options);
		send.unacked_fin = self.unacked_fin;
		Poll::Ready(Ok((
			send,
			poll::Buffered::new(Script {
				script,
				close: self.close.clone(),
				log: self.log.clone(),
			}),
		)))
	}

	fn poll_open_request(
		&self,
		waiter: &kio::Waiter,
		options: crate::transport::poll::SendOptions<
			'_,
			<Self::SendStream as crate::transport::poll::SendStream>::Group,
		>,
	) -> std::task::Poll<Result<Option<crate::transport::poll::BiStreams<Self>>, Self::Error>> {
		self.log.request_attempts.fetch_add(1, Ordering::Relaxed);
		if !*self.credit.read() {
			return Poll::Ready(Ok(None));
		}
		self.poll_open_bi(waiter, options).map_ok(Some)
	}

	fn poll_open_uni(
		&self,
		_waiter: &kio::Waiter,
		options: poll::SendOptions<'_, SinkGroup>,
	) -> Poll<Result<Self::SendStream, Self::Error>> {
		let mut send = SinkSend::new(self.log.clone());
		send.unacked_fin = self
			.unacked_unis
			.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| count.checked_sub(1))
			.is_ok();
		Poll::Ready(Ok(send.opened(options)))
	}

	fn send_group(&self, priority: i64) -> Option<SinkGroup> {
		self.groups.then(|| SinkGroup::open(self.log.clone(), priority))
	}

	fn send_datagram(
		&self,
		payload: bytes::Bytes,
		options: poll::SendOptions<'_, SinkGroup>,
	) -> Result<(), Self::Error> {
		self.log.record_datagram(payload, options);
		Ok(())
	}

	fn poll_recv_datagram(&self, _waiter: &kio::Waiter) -> Poll<Result<bytes::Bytes, Self::Error>> {
		Poll::Pending
	}

	fn max_datagram_size(&self) -> usize {
		self.datagram_size
	}

	fn address(&self) -> crate::transport::Address {
		crate::transport::Address::Setup
	}
	fn protocol(&self) -> Option<&str> {
		None
	}

	fn close(&self, code: u32, reason: &str) {
		self.log.close(code, reason);
	}

	fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Self::Error> {
		self.log.poll_closed(waiter)
	}

	fn metrics(&self) -> impl crate::transport::Metrics {
		self.stats
	}
}

#[test]
fn a_local_close_preserves_scripted_uni_bytes_and_ends_accepts() {
	use poll::{RecvStream as _, Session as _};
	for fin in [false, true] {
		let session = ScriptedSession::new(Vec::new()).with_incoming_unis(vec![b"last".to_vec()]);
		if fin {
			session.close(Close::Fin);
		}
		poll::Session::close(&session, 0, "done");
		let waiter = &kio::Waiter::noop();
		assert!(session.poll_closed(waiter).is_ready());
		let Poll::Ready(Ok(mut recv)) = session.poll_accept_uni(waiter) else {
			panic!("buffered uni lost")
		};
		let Poll::Ready(Ok(Some(bytes))) = recv.poll_read_chunk(waiter, 4) else {
			panic!("buffered bytes lost")
		};
		assert_eq!(&bytes[..], b"last");
		let end = recv.poll_read_chunk(waiter, 4);
		assert!(if fin {
			matches!(end, Poll::Ready(Ok(None)))
		} else {
			matches!(end, Poll::Ready(Err(_)))
		});
		assert!(matches!(session.poll_accept_uni(waiter), Poll::Ready(Err(_))));
	}
}
