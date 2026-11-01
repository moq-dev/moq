//! How a connection shares its moq-quic state with its stream handles: prototypes for
//! `quest/m2/perf/quic-borrow-free.md`, measured without I/O.
//!
//! A relay (the server) writes `streams` new unidirectional streams to each of `conns`
//! subscribers (the clients) per round, `writes` writes of `size` bytes each, then finishes
//! them; every subscriber leases each stream to its end, releases the credit, and stops it.
//! A round ends once every stream was read and the path settled. Each shape runs the same
//! moq-quic calls; only how a handle reaches the connection differs:
//!
//! - `context`: one owner holds the connection and passes `&mut Connection` into its
//!   children, which hold only a stream ID. The pointer stands in for the borrow.
//! - `refcell`: moq-uring today, an `Rc<RefCell<Connection>>` per handle.
//! - `mutex`: moq-tokio today, an `Arc<Mutex<Connection>>` per handle, and a driver wake
//!   after each change.
//! - `channel`: the driver alone owns the connection. A send handle appends to a per-stream
//!   [`kio::log`] against credit the driver publishes, and a receive handle is handed each
//!   lease through its stream's cell and hands it back for release.
//!
//! Every task wake, the app's and the driver's own, is the same lock-free run queue push.
//!
//! Run with no arguments it does a short pass of every shape. With
//! `<shape> <conns> <streams> <writes> <size> <rounds>` it runs that case once, for
//! `perf stat -e instructions:u` on a pinned core: the difference between two round counts
//! is the steady-state cost of the extra rounds.

use std::{
	cell::{RefCell, UnsafeCell},
	collections::VecDeque,
	net::SocketAddr,
	ptr,
	rc::Rc,
	sync::{
		Arc, Mutex,
		atomic::{AtomicBool, AtomicPtr, AtomicU8, AtomicU64, Ordering},
	},
	task::{Poll, Wake, Waker},
	time::Duration,
};

use bytes::{Buf, Bytes, BytesMut};
use kio::clock::Instant;
use moq_quic::{
	Event, VarInt,
	connection::Connection,
	endpoint::{ConnectionHandle, DatagramEvent, Endpoint, Received as Inbound},
	recv::Lease,
	send,
	stream::{Dir, Id as StreamId},
};
use rustc_hash::FxHashMap;

#[allow(
	dead_code,
	reason = "a harness shared by several benches, each using only part of it"
)]
mod common;

use common::{Pair, Setup, payload};

/// The most datagrams one `poll_transmit` may batch, as the harness allows.
const MAX_DATAGRAMS: usize = 10;
/// How far virtual time may jump to a timer while settling.
const SETTLE: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug)]
struct Workload {
	conns: usize,
	streams: usize,
	writes: usize,
	size: usize,
}

// ---------------------------------------------------------------------------------------------
// A lock-free run queue: what a waker pushes its task onto.

/// The intrusive link a [`Stack`] threads through its nodes.
struct Link<T> {
	queued: AtomicBool,
	next: AtomicPtr<T>,
}

impl<T> Default for Link<T> {
	fn default() -> Self {
		Self {
			queued: AtomicBool::new(false),
			next: AtomicPtr::new(ptr::null_mut()),
		}
	}
}

trait Node: Sized {
	fn link(&self) -> &Link<Self>;
}

/// A Treiber stack each node is on at most once, drained whole by its one consumer.
struct Stack<T: Node> {
	head: AtomicPtr<T>,
}

impl<T: Node> Default for Stack<T> {
	fn default() -> Self {
		Self {
			head: AtomicPtr::new(ptr::null_mut()),
		}
	}
}

impl<T: Node> Stack<T> {
	/// Queue `node` unless it already is.
	fn push(&self, node: &Arc<T>) {
		if node.link().queued.swap(true, Ordering::AcqRel) {
			return;
		}
		let raw = Arc::into_raw(node.clone()).cast_mut();
		let mut head = self.head.load(Ordering::Relaxed);
		loop {
			node.link().next.store(head, Ordering::Relaxed);
			match self
				.head
				.compare_exchange_weak(head, raw, Ordering::Release, Ordering::Relaxed)
			{
				Ok(_) => return,
				Err(actual) => head = actual,
			}
		}
	}

	/// Hand every queued node to `f`, newest first, until none are left.
	fn drain(&self, mut f: impl FnMut(Arc<T>)) {
		loop {
			let mut next = self.head.swap(ptr::null_mut(), Ordering::Acquire);
			if next.is_null() {
				return;
			}
			while !next.is_null() {
				// SAFETY: pushed by `push` from `Arc::into_raw`, and taken off the stack once.
				let node = unsafe { Arc::from_raw(next) };
				next = node.link().next.load(Ordering::Relaxed);
				// A read-modify-write, so a push that found the node still queued is ordered
				// before it, and `f` sees whatever that push published.
				node.link().queued.swap(false, Ordering::AcqRel);
				f(node);
			}
		}
	}
}

impl<T: Node> Drop for Stack<T> {
	fn drop(&mut self) {
		self.drain(drop);
	}
}

// ---------------------------------------------------------------------------------------------
// The network: endpoints exchanging datagrams over a zero-latency path, each connection driven
// through its shape.

/// A connection as its driver holds it.
trait Driver {
	fn conn<R>(&mut self, f: impl FnOnce(&mut Connection) -> R) -> R;
	/// Apply what the handles queued since the last turn, before transmitting.
	fn flush(&mut self) {}
	/// Hand what arrived to the handles, after receiving.
	fn dispatch(&mut self) {}
}

struct Peer<D> {
	endpoint: Endpoint,
	addr: SocketAddr,
	conns: Vec<D>,
	index: FxHashMap<ConnectionHandle, usize>,
	handles: Vec<ConnectionHandle>,
	inbound: VecDeque<(SocketAddr, BytesMut)>,
	outbound: Vec<(SocketAddr, BytesMut)>,
	buf: Vec<u8>,
}

impl<D: Driver> Peer<D> {
	fn new(peer: common::Peer, wrap: &mut impl FnMut(Connection) -> D) -> Self {
		let common::Peer {
			endpoint, addr, conns, ..
		} = peer;
		let mut conns: Vec<_> = conns.into_iter().collect();
		// Ordered, so every run drives the connections the same way.
		conns.sort_by_key(|(ch, _)| ch.0);
		let mut index = FxHashMap::default();
		let mut handles = Vec::new();
		let conns = conns
			.into_iter()
			.enumerate()
			.map(|(i, (ch, conn))| {
				index.insert(ch, i);
				handles.push(ch);
				wrap(conn)
			})
			.collect();
		Self {
			endpoint,
			addr,
			conns,
			index,
			handles,
			inbound: VecDeque::new(),
			outbound: Vec::new(),
			buf: Vec::new(),
		}
	}

	/// Handle every queued datagram and due timer, then hand the result to the handles.
	fn receive(&mut self, now: Instant) {
		for (remote, data) in std::mem::take(&mut self.inbound) {
			self.buf.clear();
			let inbound = Inbound {
				remote,
				local_ip: None,
				ecn: None,
				data,
			};
			match self.endpoint.handle(now, inbound, &mut self.buf) {
				Some(DatagramEvent::ConnectionEvent(ch, event)) => {
					self.conns[self.index[&ch]].conn(|c| c.handle_event(event));
				}
				Some(DatagramEvent::Response(transmit)) => split(&mut self.outbound, &transmit, &self.buf),
				Some(DatagramEvent::NewConnection(_)) => panic!("unexpected connection"),
				None => {}
			}
		}
		for (conn, &ch) in self.conns.iter_mut().zip(&self.handles) {
			let endpoint = &mut self.endpoint;
			conn.conn(|c| {
				if c.poll_timeout().is_some_and(|at| at <= now) {
					c.handle_timeout(now);
				}
				while let Some(event) = c.poll_endpoint_events() {
					if let Some(event) = endpoint.handle_event(ch, event) {
						c.handle_event(event);
					}
				}
				// Waits are woken by moq-quic itself; the events only need draining.
				while let Some(event) = c.poll() {
					if let Event::ConnectionLost { reason } = event {
						panic!("connection lost: {reason}");
					}
				}
			});
			conn.dispatch();
		}
	}

	/// Apply the handles' changes and transmit.
	fn transmit(&mut self, now: Instant) {
		for conn in &mut self.conns {
			conn.flush();
			let buf = &mut self.buf;
			let outbound = &mut self.outbound;
			conn.conn(|c| {
				buf.clear();
				while let Some(transmit) = c.poll_transmit(now, MAX_DATAGRAMS, buf) {
					split(outbound, &transmit, buf);
					buf.clear();
				}
			});
		}
	}

	fn next_timeout(&mut self) -> Option<Instant> {
		self.conns.iter_mut().filter_map(|c| c.conn(|c| c.poll_timeout())).min()
	}
}

fn split(out: &mut Vec<(SocketAddr, BytesMut)>, transmit: &moq_quic::Transmit, buf: &[u8]) {
	let data = &buf[..transmit.size];
	let segment = transmit.segment_size.unwrap_or(data.len());
	for datagram in data.chunks(segment) {
		out.push((transmit.remote, BytesMut::from(datagram)));
	}
}

/// What a shape's subscribers read so far.
#[derive(Default, Debug, Clone, Copy)]
struct Received {
	bytes: u64,
	finished: u64,
}

/// Writes rounds of streams to the relay's connections.
trait Sender {
	/// Write `streams` streams of one round to the connection at `index` of the relay.
	fn send(&self, index: usize, streams: usize, workload: &Workload, payload: &Bytes);
}

/// The application side of a shape.
trait App {
	type Driver: Driver;
	/// Wrap a connection, returning its driver.
	fn wrap(&mut self, conn: Connection) -> Self::Driver;
	type Sender: Sender;
	/// The relay's writers, once every connection is wrapped.
	fn sender(&self) -> Self::Sender;
	/// Run every woken subscriber task.
	fn run(&mut self, received: &mut Received);
}

struct Net<A: App> {
	now: Instant,
	relay: Peer<A::Driver>,
	subscribers: Vec<Peer<A::Driver>>,
	by_addr: FxHashMap<SocketAddr, usize>,
	app: A,
	sender: A::Sender,
	received: Received,
}

impl<A: App> Net<A> {
	fn new(conns: usize, mut app: A) -> Self {
		let pair = Pair::new(Setup {
			clients: conns,
			..Setup::default()
		});
		let Pair {
			now, server, clients, ..
		} = pair;
		let mut wrap = |conn| app.wrap(conn);
		let relay = Peer::new(server, &mut wrap);
		let subscribers: Vec<_> = clients.into_iter().map(|c| Peer::new(c, &mut wrap)).collect();
		let by_addr = subscribers.iter().enumerate().map(|(i, s)| (s.addr, i)).collect();
		let sender = app.sender();
		Self {
			now,
			relay,
			subscribers,
			by_addr,
			app,
			sender,
			received: Received::default(),
		}
	}

	/// Runs every peer once and delivers what they sent, returning whether anything moved.
	fn step(&mut self) -> bool {
		let mut moved = false;
		for sub in &mut self.subscribers {
			sub.receive(self.now);
			self.app.run(&mut self.received);
			sub.transmit(self.now);
			for (destination, datagram) in sub.outbound.drain(..) {
				assert_eq!(destination, self.relay.addr);
				moved = true;
				self.relay.inbound.push_back((sub.addr, datagram));
			}
		}
		self.relay.receive(self.now);
		self.relay.transmit(self.now);
		for (destination, datagram) in self.relay.outbound.drain(..) {
			let sub = &mut self.subscribers[self.by_addr[&destination]];
			sub.inbound.push_back((self.relay.addr, datagram));
			moved = true;
		}
		moved
	}

	fn next_timeout(&mut self) -> Option<Instant> {
		let relay = self.relay.next_timeout();
		self.subscribers
			.iter_mut()
			.filter_map(Peer::next_timeout)
			.chain(relay)
			.min()
	}

	/// One round: every connection gets its streams, read to the end, then the path settles.
	fn round(&mut self, workload: &Workload, payload: &Bytes) {
		let target = self.received.finished + (workload.conns * workload.streams) as u64;
		for index in 0..workload.conns {
			self.sender.send(index, workload.streams, workload, payload);
		}
		self.finish_round(target, || true);
	}

	/// Exchange until `target` streams were read, then let the path settle. Time only moves
	/// once `written` says the writers are done.
	fn finish_round(&mut self, target: u64, written: impl Fn() -> bool) {
		while self.received.finished < target {
			// Asked before the step, so a step that moved nothing saw every write.
			let written = written();
			if !self.step() {
				if !written {
					std::hint::spin_loop();
					continue;
				}
				let next = self.next_timeout().expect("stalled with no timer");
				self.now = self.now.max(next);
			}
		}
		loop {
			if self.step() {
				continue;
			}
			match self.next_timeout() {
				Some(next) if next <= self.now + SETTLE => self.now = self.now.max(next),
				_ => break,
			}
		}
	}
}

// ---------------------------------------------------------------------------------------------
// The lock shapes: every handle reaches the whole connection through one shared reference.

/// How a handle reaches the connection.
trait Access: Clone + 'static {
	fn with<R>(&self, f: impl FnOnce(&mut Connection) -> R) -> R;
	/// Tell the driver to run: after a change, unless it runs after its children anyway.
	fn wake(&self);
}

/// Context passing: the owner's `&mut Connection`, as a pointer since the owner outlives every
/// child and polls them one at a time. No reference count and no flag.
#[derive(Clone, Copy)]
struct Direct(*mut Connection);

impl Access for Direct {
	#[inline]
	fn with<R>(&self, f: impl FnOnce(&mut Connection) -> R) -> R {
		// SAFETY: the owner holds the connection for the run and polls one child at a time.
		f(unsafe { &mut *self.0 })
	}

	#[inline]
	fn wake(&self) {}
}

/// moq-uring's shape: a per-core connection behind a `RefCell`, and a driver flag.
struct Local {
	conn: RefCell<Connection>,
	notified: AtomicBool,
}

impl Access for Rc<Local> {
	#[inline]
	fn with<R>(&self, f: impl FnOnce(&mut Connection) -> R) -> R {
		f(&mut self.conn.borrow_mut())
	}

	#[inline]
	fn wake(&self) {
		self.notified.store(true, Ordering::Release);
	}
}

/// moq-tokio's shape: the connection behind a `Mutex`, and a driver wake.
struct Locked {
	conn: Mutex<Connection>,
	notified: AtomicBool,
}

impl Access for Arc<Locked> {
	#[inline]
	fn with<R>(&self, f: impl FnOnce(&mut Connection) -> R) -> R {
		f(&mut self.conn.lock().unwrap())
	}

	#[inline]
	fn wake(&self) {
		self.notified.swap(true, Ordering::AcqRel);
	}
}

/// The driver of a lock shape, through the same reference its handles hold.
struct LockDriver<A>(A);

impl<A: Access> Driver for LockDriver<A> {
	fn conn<R>(&mut self, f: impl FnOnce(&mut Connection) -> R) -> R {
		self.0.with(f)
	}
}

/// A subscriber task of a lock shape: the connection's accept loop, or one stream's reader.
struct Task<A> {
	link: Link<Task<A>>,
	run: Arc<Stack<Task<A>>>,
	conn: A,
	stream: Option<StreamId>,
	/// The waiter its waits park on. Only the task's poll touches it.
	waiter: UnsafeCell<Option<kio::Waiter>>,
}

// SAFETY: a run queue polls a task from one thread at a time, and an `Rc` access only ever
// lives on the thread that runs the whole benchmark.
unsafe impl<A> Send for Task<A> {}
// SAFETY: as above; only `link` and `run` are touched from a waker, and they are atomic.
unsafe impl<A> Sync for Task<A> {}

impl<A> Node for Task<A> {
	fn link(&self) -> &Link<Self> {
		&self.link
	}
}

impl<A> Task<A> {
	/// Let go of the waiter, and with it the cycle through its waker.
	///
	/// # Safety
	/// Only from the task's own poll.
	unsafe fn done(&self) {
		// SAFETY: only the poll touches it, per the contract.
		unsafe { &mut *self.waiter.get() }.take();
	}
}

impl<A: Access> Wake for Task<A> {
	fn wake(self: Arc<Self>) {
		self.run.push(&self);
	}

	fn wake_by_ref(self: &Arc<Self>) {
		self.run.push(self);
	}
}

impl<A: Access> Task<A> {
	fn spawn(run: &Arc<Stack<Self>>, conn: A, stream: Option<StreamId>) -> Arc<Self> {
		let task = Arc::new(Self {
			link: Link::default(),
			run: run.clone(),
			conn,
			stream,
			waiter: UnsafeCell::new(None),
		});
		run.push(&task);
		task
	}

	/// The task's waiter, made on first use.
	///
	/// # Safety
	/// Only from the task's own poll.
	unsafe fn waiter(self: &Arc<Self>) -> &kio::Waiter {
		// SAFETY: only the poll touches it, per the contract.
		unsafe { &mut *self.waiter.get() }.get_or_insert_with(|| kio::Waiter::new(Waker::from(self.clone())))
	}

	fn poll(self: &Arc<Self>, received: &mut Received) {
		// SAFETY: this is the task's poll.
		let waiter = unsafe { self.waiter() };
		let Some(id) = self.stream else {
			loop {
				match self.conn.with(|c| c.streams().poll_accept(waiter, Dir::Uni)) {
					Poll::Ready(Ok(id)) => {
						Self::spawn(&self.run, self.conn.clone(), Some(id));
					}
					Poll::Ready(Err(err)) => panic!("accept failed: {err}"),
					Poll::Pending => return,
				}
			}
		};
		loop {
			let (poll, transmit) = self.conn.with(|c| c.recv_stream(id).poll_lease(waiter, 1));
			if transmit.should_transmit() {
				self.conn.wake();
			}
			let mut lease = match poll {
				Poll::Ready(Ok(lease)) => lease,
				Poll::Ready(Err(err)) => panic!("lease failed: {err}"),
				Poll::Pending => return,
			};
			let fin = lease.is_fin();
			received.bytes += consume(&mut lease);
			self.conn.with(|c| {
				let mut stream = c.recv_stream(id);
				let _ = stream.release(lease);
				if fin {
					stream.stop(VarInt::from_u32(0)).expect("stopped once");
				}
			});
			self.conn.wake();
			if fin {
				received.finished += 1;
				// SAFETY: this is the task's poll.
				unsafe { self.done() };
				return;
			}
		}
	}
}

/// Take a lease's bytes a chunk at a time, as moq-net hands a frame's payload on.
fn consume(lease: &mut Lease) -> u64 {
	let mut bytes = 0;
	while lease.has_remaining() {
		bytes += lease.copy_to_bytes(lease.chunk().len()).len() as u64;
	}
	bytes
}

/// A lock shape's application: the relay's writers and the subscribers' readers.
struct LockApp<A> {
	make: fn(Connection) -> A,
	/// The relay's connections, then each subscriber's: the order [`Net::new`] wraps them in.
	relay: Vec<A>,
	conns: usize,
	run: Arc<Stack<Task<A>>>,
	/// Every accept task, which lives as long as the run.
	accepts: Vec<Arc<Task<A>>>,
}

/// A lock shape's relay writers: a reference to each relay connection.
#[derive(Clone)]
struct LockSender<A>(Vec<A>);

impl<A: Access> LockApp<A> {
	fn new(conns: usize, make: fn(Connection) -> A) -> Self {
		Self {
			make,
			relay: Vec::new(),
			conns,
			run: Arc::default(),
			accepts: Vec::new(),
		}
	}
}

impl<A> Drop for LockApp<A> {
	fn drop(&mut self) {
		for task in &self.accepts {
			// SAFETY: no poll runs any more.
			unsafe { task.done() };
		}
	}
}

impl<A: Access> App for LockApp<A> {
	type Driver = LockDriver<A>;

	fn wrap(&mut self, conn: Connection) -> LockDriver<A> {
		let access = (self.make)(conn);
		if self.relay.len() < self.conns {
			self.relay.push(access.clone());
		} else {
			self.accepts.push(Task::spawn(&self.run, access.clone(), None));
		}
		LockDriver(access)
	}

	type Sender = LockSender<A>;

	fn sender(&self) -> LockSender<A> {
		LockSender(self.relay.clone())
	}

	fn run(&mut self, received: &mut Received) {
		self.run.drain(|task| task.poll(received));
	}
}

impl<A: Access> Sender for LockSender<A> {
	fn send(&self, index: usize, streams: usize, workload: &Workload, payload: &Bytes) {
		let shared = &self.0[index];
		let waiter = kio::Waiter::noop();
		for _ in 0..streams {
			// The handle holds its own reference, as a stream handle does.
			let conn = shared.clone();
			let id = conn
				.with(|c| c.streams().open(Dir::Uni, send::Options::default()))
				.expect("stream credit");
			for _ in 0..workload.writes {
				let mut data = payload.clone();
				while data.has_remaining() {
					match conn.with(|c| c.send_stream(id).poll_write(&waiter, &mut data)) {
						Poll::Ready(Ok(_)) => conn.wake(),
						Poll::Ready(Err(err)) => panic!("write failed: {err}"),
						Poll::Pending => panic!("the benchmark's windows never fill"),
					}
				}
			}
			conn.with(|c| c.send_stream(id).finish()).expect("finish");
			conn.wake();
			// The finished handle's drop, which gives the ID back.
			conn.with(|c| {
				let stream = c.send_stream(id);
				let _ = stream.end();
				stream.release();
			});
		}
	}
}

// ---------------------------------------------------------------------------------------------
// The channel shape: the driver alone owns the connection, and each stream is a cell between
// its handle and the driver.

/// What every cell of a connection shares with its driver.
struct ChannelShared {
	/// Send cells with something new: an open, an append, or a finish.
	dirty: Stack<SendCell>,
	/// Receive cells moq-quic woke, or whose lease came back.
	ready: Stack<RecvCell>,
	/// Connection credit: every byte the driver ever let the handles append, and every byte
	/// they appended.
	granted: AtomicU64,
	reserved: AtomicU64,
	/// The stream credit a new stream starts with, before its driver opened it.
	initial: AtomicU64,
	notified: AtomicBool,
}

impl ChannelShared {
	fn wake(&self) {
		self.notified.swap(true, Ordering::AcqRel);
	}
}

/// A send stream between its handle and the driver.
struct SendCell {
	link: Link<SendCell>,
	conn: Arc<ChannelShared>,
	/// Appended by the handle only.
	log: UnsafeCell<kio::log::Writer<Bytes>>,
	/// Touched by the driver only.
	driver: UnsafeCell<SendDriver>,
	/// The log entries the driver moved into moq-quic, for the handle to evict.
	consumed: AtomicU64,
	/// Stream credit: bytes the driver let the handle append in total, and those it did.
	granted: AtomicU64,
	reserved: AtomicU64,
	fin: AtomicBool,
}

struct SendDriver {
	reader: kio::log::Reader<Bytes>,
	next: u64,
	id: Option<StreamId>,
	written: u64,
	/// Finished and released: a push that raced the finish finds nothing left to do.
	finished: bool,
}

// SAFETY: `log` is the handle's alone and `driver` the driver's alone, and the rest is atomic.
unsafe impl Send for SendCell {}
// SAFETY: as above.
unsafe impl Sync for SendCell {}

impl Node for SendCell {
	fn link(&self) -> &Link<Self> {
		&self.link
	}
}

/// A send stream's handle: non-`Clone`, the one appender of its cell.
struct SendHandle(Arc<SendCell>);

impl SendHandle {
	fn open(conn: &Arc<ChannelShared>) -> Self {
		let log = kio::log::Writer::new(0);
		let reader = log.reader();
		let cell = Arc::new(SendCell {
			link: Link::default(),
			conn: conn.clone(),
			log: UnsafeCell::new(log),
			driver: UnsafeCell::new(SendDriver {
				reader,
				next: 0,
				id: None,
				written: 0,
				finished: false,
			}),
			consumed: AtomicU64::new(0),
			granted: AtomicU64::new(conn.initial.load(Ordering::Acquire)),
			reserved: AtomicU64::new(0),
			fin: AtomicBool::new(false),
		});
		conn.dirty.push(&cell);
		conn.wake();
		Self(cell)
	}

	/// Append as much of `data` as the credit allows, returning how many bytes.
	fn write(&mut self, data: &mut Bytes) -> Poll<usize> {
		let cell = &*self.0;
		let reserved = cell.reserved.load(Ordering::Relaxed);
		let stream = cell.granted.load(Ordering::Acquire) - reserved;
		let want = (data.len() as u64).min(stream);
		let Some(n) = reserve(&cell.conn.granted, &cell.conn.reserved, want) else {
			return Poll::Pending;
		};
		cell.reserved.store(reserved + n, Ordering::Relaxed);
		// SAFETY: the handle is the log's only appender.
		let log = unsafe { &mut *cell.log.get() };
		let consumed = cell.consumed.load(Ordering::Acquire);
		log.evict(consumed);
		log.push(data.split_to(n as usize));
		cell.conn.dirty.push(&self.0);
		cell.conn.wake();
		Poll::Ready(n as usize)
	}

	fn finish(self) {
		self.0.fin.store(true, Ordering::Release);
		self.0.conn.dirty.push(&self.0);
		self.0.conn.wake();
	}
}

/// Take up to `want` bytes of a shared credit, or `None` when none is left.
fn reserve(granted: &AtomicU64, reserved: &AtomicU64, want: u64) -> Option<u64> {
	let mut current = reserved.load(Ordering::Relaxed);
	loop {
		let n = want.min(granted.load(Ordering::Acquire).saturating_sub(current));
		if n == 0 {
			return None;
		}
		match reserved.compare_exchange_weak(current, current + n, Ordering::AcqRel, Ordering::Relaxed) {
			Ok(_) => return Some(n),
			Err(actual) => current = actual,
		}
	}
}

/// A receive stream between the driver and its handle.
struct RecvCell {
	link: Link<RecvCell>,
	conn: Arc<ChannelShared>,
	id: StreamId,
	/// The lease out to the handle, and the one it handed back.
	out: AtomicPtr<Lease>,
	back: AtomicPtr<Lease>,
	/// Driver-only progress: whether a lease is out, and whether the stream ended.
	state: AtomicU8,
	/// The driver's waiter on moq-quic, waking this cell onto the driver's ready stack.
	waiter: UnsafeCell<Option<kio::Waiter>>,
	/// The handle's task, woken when a lease is out.
	task: UnsafeCell<Option<Arc<RecvTask>>>,
}

const LEASED: u8 = 1;
const FIN: u8 = 2;

// SAFETY: `waiter` and `task` are the driver's alone, and the rest is atomic.
unsafe impl Send for RecvCell {}
// SAFETY: as above.
unsafe impl Sync for RecvCell {}

impl Node for RecvCell {
	fn link(&self) -> &Link<Self> {
		&self.link
	}
}

impl Wake for RecvCell {
	fn wake(self: Arc<Self>) {
		self.conn.ready.push(&self);
		self.conn.wake();
	}

	fn wake_by_ref(self: &Arc<Self>) {
		self.conn.ready.push(self);
		self.conn.wake();
	}
}

fn take_lease(slot: &AtomicPtr<Lease>) -> Option<Box<Lease>> {
	let raw = slot.swap(ptr::null_mut(), Ordering::AcqRel);
	// SAFETY: only `put_lease` stores into a slot, from `Box::into_raw`.
	(!raw.is_null()).then(|| unsafe { Box::from_raw(raw) })
}

fn put_lease(slot: &AtomicPtr<Lease>, lease: Box<Lease>) {
	let old = slot.swap(Box::into_raw(lease), Ordering::AcqRel);
	assert!(old.is_null(), "one lease at a time");
}

/// A subscriber's task reading one receive cell.
struct RecvTask {
	link: Link<RecvTask>,
	run: Arc<Stack<RecvTask>>,
	cell: Arc<RecvCell>,
}

impl Node for RecvTask {
	fn link(&self) -> &Link<Self> {
		&self.link
	}
}

impl RecvTask {
	/// Read every lease the driver handed over, handing each back.
	fn poll(&self, received: &mut Received) {
		let cell = &self.cell;
		let Some(mut lease) = take_lease(&cell.out) else {
			return;
		};
		let fin = lease.is_fin();
		received.bytes += consume(&mut lease);
		put_lease(&cell.back, lease);
		cell.conn.ready.push(cell);
		cell.conn.wake();
		if fin {
			received.finished += 1;
		}
	}
}

/// The channel shape's driver: the only owner of its connection.
struct ChannelDriver {
	conn: Connection,
	shared: Arc<ChannelShared>,
	/// Opens that found no stream credit, in order.
	opening: VecDeque<Arc<SendCell>>,
	/// Every byte written into moq-quic, for the connection credit.
	written: u64,
	/// The subscriber run queue its accepted streams' tasks go on, if a subscriber.
	run: Option<Arc<Stack<RecvTask>>>,
}

impl ChannelDriver {
	fn publish(&mut self) {
		let granted = self.written + self.conn.streams().write_limit();
		self.shared.granted.fetch_max(granted, Ordering::AcqRel);
	}

	/// Move what a send cell's handle appended into moq-quic.
	fn send(&mut self, cell: Arc<SendCell>) {
		// SAFETY: the driver is the only one to touch `driver`.
		let driver = unsafe { &mut *cell.driver.get() };
		if driver.finished {
			return;
		}
		let id = match driver.id {
			Some(id) => id,
			None => match self.conn.streams().open(Dir::Uni, send::Options::default()) {
				Some(id) => *driver.id.insert(id),
				None => {
					self.opening.push_back(cell);
					return;
				}
			},
		};
		// Read before the entries, so a finish covers every entry appended before it.
		let fin = cell.fin.load(Ordering::Acquire);
		let mut stream = self.conn.send_stream(id);
		{
			let mut read = driver.reader.read();
			while let Some(entry) = read.get(driver.next).expect("evicted only once consumed") {
				let mut data = entry.clone();
				let len = data.len();
				let written = stream.write(&mut data).expect("within the published credit");
				assert_eq!(written, len, "the published credit covers every append");
				driver.written += len as u64;
				self.written += len as u64;
				driver.next += 1;
			}
		}
		cell.consumed.store(driver.next, Ordering::Release);
		if fin {
			driver.finished = true;
			stream.finish().expect("finish");
			let _ = stream.end();
			stream.release();
			return;
		}
		let granted = driver.written + stream.writable();
		cell.granted.fetch_max(granted, Ordering::AcqRel);
	}

	/// Hand a receive cell's lease to its handle, or release the one it handed back.
	fn recv(&mut self, cell: Arc<RecvCell>) {
		let mut state = cell.state.load(Ordering::Relaxed);
		if let Some(lease) = take_lease(&cell.back) {
			let mut stream = self.conn.recv_stream(cell.id);
			let _ = stream.release(*lease);
			state &= !LEASED;
			if state & FIN != 0 {
				stream.stop(VarInt::from_u32(0)).expect("stopped once");
				// SAFETY: the driver is the only one to touch them.
				unsafe {
					(*cell.waiter.get()).take();
					(*cell.task.get()).take();
				}
				cell.state.store(state, Ordering::Relaxed);
				return;
			}
		}
		if state & LEASED == 0 {
			// SAFETY: the driver is the only one to touch it.
			let waiter =
				unsafe { &mut *cell.waiter.get() }.get_or_insert_with(|| kio::Waiter::new(Waker::from(cell.clone())));
			let (poll, _transmit) = self.conn.recv_stream(cell.id).poll_lease(waiter, 1);
			match poll {
				Poll::Ready(Ok(lease)) => {
					if lease.is_fin() {
						state |= FIN;
					}
					state |= LEASED;
					put_lease(&cell.out, Box::new(lease));
					// SAFETY: the driver is the only one to touch it.
					let task = unsafe { &*cell.task.get() }.as_ref().expect("a task per cell");
					task.run.push(task);
				}
				Poll::Ready(Err(err)) => panic!("lease failed: {err}"),
				Poll::Pending => {}
			}
		}
		cell.state.store(state, Ordering::Relaxed);
	}
}

impl Driver for ChannelDriver {
	fn conn<R>(&mut self, f: impl FnOnce(&mut Connection) -> R) -> R {
		f(&mut self.conn)
	}

	fn dispatch(&mut self) {
		if let Some(run) = self.run.clone() {
			while let Some(id) = self.conn.streams().accept(Dir::Uni) {
				let cell = Arc::new(RecvCell {
					link: Link::default(),
					conn: self.shared.clone(),
					id,
					out: AtomicPtr::new(ptr::null_mut()),
					back: AtomicPtr::new(ptr::null_mut()),
					state: AtomicU8::new(0),
					waiter: UnsafeCell::new(None),
					task: UnsafeCell::new(None),
				});
				let task = Arc::new(RecvTask {
					link: Link::default(),
					run: run.clone(),
					cell: cell.clone(),
				});
				// SAFETY: the driver is the only one to touch it.
				unsafe { *cell.task.get() = Some(task) };
				self.shared.ready.push(&cell);
			}
		}
		let shared = self.shared.clone();
		shared.ready.drain(|cell| self.recv(cell));
	}

	fn flush(&mut self) {
		self.shared.notified.store(false, Ordering::Release);
		let shared = self.shared.clone();
		shared.ready.drain(|cell| self.recv(cell));
		for _ in 0..self.opening.len() {
			let cell = self.opening.pop_front().expect("counted");
			self.send(cell);
		}
		shared.dirty.drain(|cell| self.send(cell));
		self.publish();
	}
}

struct ChannelApp {
	relay: Vec<Arc<ChannelShared>>,
	conns: usize,
	run: Arc<Stack<RecvTask>>,
}

impl App for ChannelApp {
	type Driver = ChannelDriver;

	fn wrap(&mut self, mut conn: Connection) -> ChannelDriver {
		let shared = Arc::new(ChannelShared {
			dirty: Stack::default(),
			ready: Stack::default(),
			granted: AtomicU64::new(0),
			reserved: AtomicU64::new(0),
			initial: AtomicU64::new(conn.streams().initial_send_credit(Dir::Uni)),
			notified: AtomicBool::new(false),
		});
		let relay = self.relay.len() < self.conns;
		if relay {
			self.relay.push(shared.clone());
		}
		let mut driver = ChannelDriver {
			conn,
			shared,
			opening: VecDeque::new(),
			written: 0,
			run: (!relay).then(|| self.run.clone()),
		};
		driver.publish();
		driver
	}

	type Sender = ChannelSender;

	fn sender(&self) -> ChannelSender {
		ChannelSender(self.relay.clone())
	}

	fn run(&mut self, received: &mut Received) {
		self.run.drain(|task| task.poll(received));
	}
}

/// The channel shape's relay writers: each relay connection's shared cell state.
#[derive(Clone)]
struct ChannelSender(Vec<Arc<ChannelShared>>);

impl Sender for ChannelSender {
	fn send(&self, index: usize, streams: usize, workload: &Workload, payload: &Bytes) {
		let shared = &self.0[index];
		for _ in 0..streams {
			let mut handle = SendHandle::open(shared);
			for _ in 0..workload.writes {
				let mut data = payload.clone();
				while !data.is_empty() {
					if handle.write(&mut data).is_pending() {
						panic!("the benchmark's windows never fill");
					}
				}
			}
			handle.finish();
		}
	}
}

// ---------------------------------------------------------------------------------------------

/// The context shape's connections, freed when the run ends.
struct Owned(Vec<*mut Connection>);

impl Drop for Owned {
	fn drop(&mut self) {
		for conn in self.0.drain(..) {
			// SAFETY: from `Box::into_raw`, and every child is gone.
			drop(unsafe { Box::from_raw(conn) });
		}
	}
}

fn run_rounds<A: App>(app: A, workload: &Workload, rounds: usize) -> Received {
	let payload = payload(workload.size);
	let mut net = Net::new(workload.conns, app);
	for _ in 0..rounds {
		net.round(workload, &payload);
	}
	let expected = (rounds * workload.conns * workload.streams) as u64;
	assert_eq!(net.received.finished, expected, "every stream read to its end");
	assert_eq!(
		net.received.bytes,
		expected * (workload.writes * workload.size) as u64,
		"every byte read"
	);
	net.received
}

/// Runs `rounds` with each relay connection's streams spread over `writers` threads, while this thread
/// drives every connection and reads every stream, returning the wall time per stream.
///
/// Not deterministic: it shows what a lock costs when the handles and the driver of one
/// connection run on different cores, as moq-tokio's tasks do.
#[allow(
	clippy::disallowed_methods,
	clippy::disallowed_types,
	reason = "the threaded run measures real elapsed time, which no virtual clock gives"
)]
fn run_threaded<A: App>(app: A, workload: &Workload, rounds: usize, writers: usize) -> Duration
where
	A::Sender: Clone + std::marker::Send + 'static,
{
	use std::sync::{Barrier, atomic::AtomicUsize};

	let payload = payload(workload.size);
	let mut net = Net::new(workload.conns, app);
	let start = Arc::new(Barrier::new(writers + 1));
	let done = Arc::new(AtomicUsize::new(0));
	let per_round = (workload.conns * workload.streams) as u64;
	let elapsed = std::thread::scope(|scope| {
		for writer in 0..writers {
			let sender = net.sender.clone();
			let (start, done, payload) = (start.clone(), done.clone(), payload.clone());
			let workload = *workload;
			scope.spawn(move || {
				for _ in 0..rounds {
					start.wait();
					// Every writer on every connection, as a connection's stream tasks spread
					// over the workers.
					let streams = workload.streams / writers + usize::from(writer < workload.streams % writers);
					for index in 0..workload.conns {
						sender.send(index, streams, &workload, &payload);
					}
					done.fetch_add(1, Ordering::AcqRel);
				}
			});
		}
		let begin = std::time::Instant::now();
		for round in 1..=rounds {
			let target = net.received.finished + per_round;
			start.wait();
			// A writer may count its last round in after the reads finished it.
			net.finish_round(target, || done.load(Ordering::Acquire) >= round * writers);
		}
		begin.elapsed()
	});
	assert_eq!(
		net.received.finished,
		rounds as u64 * per_round,
		"every stream read to its end"
	);
	elapsed / (rounds as u32 * per_round as u32)
}

fn run(shape: &str, workload: &Workload, rounds: usize) -> Received {
	match shape {
		"context" => run_context(workload, rounds),
		"refcell" => run_rounds(
			LockApp::new(workload.conns, |conn| {
				Rc::new(Local {
					conn: RefCell::new(conn),
					notified: AtomicBool::new(false),
				})
			}),
			workload,
			rounds,
		),
		"mutex" => run_rounds(
			LockApp::new(workload.conns, |conn| {
				Arc::new(Locked {
					conn: Mutex::new(conn),
					notified: AtomicBool::new(false),
				})
			}),
			workload,
			rounds,
		),
		"channel" => run_rounds(
			ChannelApp {
				relay: Vec::new(),
				conns: workload.conns,
				run: Arc::default(),
			},
			workload,
			rounds,
		),
		other => panic!("unknown shape {other}: context, refcell, mutex, or channel"),
	}
}

thread_local! {
	/// Where the context shape's `fn` constructor parks each connection it boxes.
	static OWNED: RefCell<Vec<*mut Connection>> = const { RefCell::new(Vec::new()) };
}

fn run_context(workload: &Workload, rounds: usize) -> Received {
	let received = run_rounds(
		LockApp::new(workload.conns, |conn| {
			let raw = Box::into_raw(Box::new(conn));
			OWNED.with(|owned| owned.borrow_mut().push(raw));
			Direct(raw)
		}),
		workload,
		rounds,
	);
	OWNED.with(|owned| drop(Owned(std::mem::take(&mut *owned.borrow_mut()))));
	received
}

fn main() {
	let args: Vec<String> = std::env::args().skip(1).filter(|a| !a.starts_with("--")).collect();
	if args.len() == 7 {
		let num = |i: usize| args[i].parse::<usize>().expect("a count");
		let workload = Workload {
			conns: num(1),
			streams: num(2),
			writes: num(3),
			size: num(4),
		};
		let (rounds, writers) = (num(5), num(6));
		let per_stream = match args[0].as_str() {
			"mutex" => run_threaded(
				LockApp::new(workload.conns, |conn| {
					Arc::new(Locked {
						conn: Mutex::new(conn),
						notified: AtomicBool::new(false),
					})
				}),
				&workload,
				rounds,
				writers,
			),
			"channel" => run_threaded(
				ChannelApp {
					relay: Vec::new(),
					conns: workload.conns,
					run: Arc::default(),
				},
				&workload,
				rounds,
				writers,
			),
			other => panic!("only mutex and channel run threaded, not {other}"),
		};
		println!("{}", per_stream.as_nanos());
		return;
	}
	if args.len() == 6 {
		let num = |i: usize| args[i].parse::<usize>().expect("a count");
		let workload = Workload {
			conns: num(1),
			streams: num(2),
			writes: num(3),
			size: num(4),
		};
		run(&args[0], &workload, num(5));
		return;
	}
	// A short pass of every shape, so `cargo bench` and CI keep them building and correct.
	let workload = Workload {
		conns: 4,
		streams: 8,
		writes: 2,
		size: 1200,
	};
	for shape in ["context", "refcell", "mutex", "channel"] {
		let received = run(shape, &workload, 4);
		println!("{shape}: {received:?}");
	}
}
