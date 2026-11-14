//! The `qmux` target: a scripted peer against a server session, over a byte stream or a
//! WebSocket, as a relay accepts one.
//!
//! The peer is a client session of this crate with three ways around it: a record of arbitrary
//! frames handed to its transport, arbitrary bytes written straight to the socket (mid-record,
//! or mid WebSocket frame), and a socket it can stop reading. The server runs a small app: it
//! echoes every bidirectional stream, reads every unidirectional one, and reads datagrams, or,
//! when the input asks, accepts streams and never reads them. It all runs on the simulated
//! clock with a fixed seed, so a failing input replays exactly. Beyond "does not panic":
//!
//! - What the server holds stays within what its limits allow, whatever the peer does: the heap
//!   it allocated for this session peaks below [`budget`].
//! - While the peer sent nothing but well-formed traffic, each stream it opened reached the app
//!   byte for byte, every echo came back the same, and every datagram arrived intact.
//! - Once the peer is gone and the clock runs past every timeout, the server's driver and app
//!   tasks have all finished and it holds nothing it allocated.

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::num::NonZeroU64;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use bytes::Bytes;
use kio::clock::sim::{self, JoinHandle};
use moq_net::transport::poll::{RecvStream as _, SendOptions, SendStream as _, Session as _};
use moq_quic::Side;
use tokio::io::{AsyncRead, AsyncWrite, DuplexStream, ReadBuf};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::protocol::Role;

use super::alloc::{self, Usage};
use crate::transport::{Stream, Transport, WsTransport};
use crate::{Config, Error, RecvStream, SendStream, Session, Version, ws};

/// The executor's seed. Nothing here draws from it, but a fixed one keeps a replay exact.
const SEED: u64 = 0x716d_7578;

/// What one [`Action::Advance`] tick moves the clock.
const TICK: Duration = Duration::from_millis(100);

/// The most a script may advance the clock in all. Idle sessions still wake for their timers.
const ADVANCE: Duration = Duration::from_secs(600);

/// How long both sides run after each action, so what it sent lands before the next one.
const SETTLE: Duration = Duration::from_millis(1);

/// How long the clock runs once the script ends, so the round trip can complete.
const FLUSH: Duration = Duration::from_secs(1);

/// How long the clock runs once the peer leaves: past the idle timeout and the close bound.
const DRAIN: Duration = Duration::from_secs(60);

/// The streams the peer tracks; it opens no more than this.
const STREAMS: usize = 16;

/// The most bytes one [`Action::Write`] or [`Action::Datagram`] sends.
const WRITE: usize = 16 * 1024;

/// Room in each direction of the socket.
const PIPE: usize = 64 * 1024;

/// The server's limits, small so a script reaches them.
const MAX_STREAMS: u64 = 4;
const STREAM_WINDOW: u64 = 16 * 1024;
const SEND_WINDOW: u64 = 64 * 1024;

/// The most heap the server may hold for one session under [`config`].
///
/// Its connection receive window, the 1 MiB datagram buffer QMux keeps, its send window, and one
/// record per stream it may hold, with room for its transport's buffers: two 16 KiB read
/// buffers and 64 KiB queued, or tungstenite's 128 KiB read and write buffers, and the
/// largest message it reads, twice over while a frame is unmasked into a message.
fn budget() -> usize {
	const MIB: usize = 1 << 20;
	const RECORD: usize = 16 * 1024;
	let streams = 2 * MAX_STREAMS as usize;
	let windows = moq_net::transport::MIN_RECEIVE_WINDOW as usize + MIB + SEND_WINDOW as usize;
	let transport = 2 * 128 * 1024 + 2 * RECORD;
	windows + streams * RECORD + transport
}

/// The QMux session both sides run at `version`.
fn config(version: Version) -> Config {
	let mut config = Config::new(version);
	config.limits.max_streams = NonZeroU64::new(MAX_STREAMS);
	config.limits.stream_receive_window = NonZeroU64::new(STREAM_WINDOW);
	config.limits.send_window = NonZeroU64::new(SEND_WINDOW);
	config
}

/// Play `data` against a server session, returning whether the handshake completed.
pub fn qmux(data: &[u8]) -> bool {
	let Some((&selector, script)) = data.split_first() else {
		return false;
	};
	let mode = Mode::new(selector);
	let (established, usage) = alloc::measure(|| sim::run_seeded(SEED, play(mode, script)));
	check(usage);
	established
}

/// The server's heap stayed within its budget, and it freed all of it.
fn check(usage: Usage) {
	assert!(
		usage.peak <= budget(),
		"the server held {} bytes, past its budget of {}",
		usage.peak,
		budget()
	);
	assert_eq!(usage.live, 0, "the server leaked {} bytes", usage.live);
}

/// What the selector byte picks.
#[derive(Clone, Copy, Debug)]
pub(super) struct Mode {
	pub version: Version,
	pub websocket: bool,
	/// The server app reads what it accepts.
	pub reads: bool,
}

impl Mode {
	fn new(selector: u8) -> Self {
		let versions = Version::ALL;
		Self {
			version: versions[(selector >> 2) as usize % versions.len()],
			websocket: selector & 1 != 0,
			reads: selector & 2 == 0,
		}
	}

	pub(super) fn selector(self) -> u8 {
		let version = Version::ALL.iter().position(|&v| v == self.version).unwrap() as u8;
		(version << 2) | u8::from(!self.reads) << 1 | u8::from(self.websocket)
	}
}

async fn play(mode: Mode, script: &[u8]) -> bool {
	let (client_io, server_io) = tokio::io::duplex(PIPE);
	let book = Rc::new(Book::default());

	let server = victim(serve(mode, server_io, book.clone()));
	let Some(mut peer) = Peer::connect(mode, client_io, book.clone()).await else {
		drain(server, &book).await;
		return false;
	};

	let mut script = Script(script);
	while let Some(action) = script.next() {
		peer.play(action).await;
		kio::clock::sleep(SETTLE).await;
	}

	// Let what the peer sent land, and read back what the server echoed.
	peer.tap.stall(false);
	let flushed = kio::clock::now().saturating_add(FLUSH);
	while kio::clock::now() < flushed {
		peer.read_all();
		kio::clock::sleep(TICK / 10).await;
	}
	peer.read_all();
	peer.verify(mode);

	peer.leave();
	drain(server, &book).await;
	true
}

/// Run the clock until the server is done with the peer, then check it is.
async fn drain(server: JoinHandle<()>, book: &Book) {
	kio::clock::sleep(DRAIN).await;
	assert!(server.is_finished(), "the server's session outlived its peer");
	assert_eq!(book.tasks.get(), 0, "a server task outlived its peer");
}

/// Spawn `future` with what it allocates charged to the server.
fn victim<F: Future<Output = ()> + 'static>(future: F) -> JoinHandle<()> {
	alloc::charge(|| sim::spawn(Charged(Box::pin(future))))
}

/// A server task, charged for what it allocates.
struct Charged<F>(Pin<Box<F>>);

impl<F: Future> Future for Charged<F> {
	type Output = F::Output;

	fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<F::Output> {
		alloc::charge(|| self.0.as_mut().poll(cx))
	}
}

/// What the server app saw, for the checks, and how many of its tasks still run.
#[derive(Default)]
struct Book {
	/// The peer sent something other than well-formed traffic, so the content checks are off.
	tainted: Cell<bool>,
	/// What arrived on each unidirectional stream, by the tag it opens with.
	uni: RefCell<[Report; STREAMS]>,
	/// Server tasks still running.
	tasks: Cell<usize>,
}

#[derive(Clone, Copy, Default, Debug)]
struct Report {
	received: u64,
	fin: bool,
}

/// Counts a server task as running until it drops.
struct Task(Rc<Book>);

impl Task {
	fn new(book: &Rc<Book>) -> Self {
		book.tasks.set(book.tasks.get() + 1);
		Self(book.clone())
	}
}

impl Drop for Task {
	fn drop(&mut self) {
		self.0.tasks.set(self.0.tasks.get() - 1);
	}
}

/// The byte at `offset` of the stream tagged `tag`: the tag itself, then a pattern of it.
fn pattern(tag: u8, offset: u64) -> u8 {
	match offset {
		0 => tag,
		_ => (offset.wrapping_mul(31) ^ (offset >> 7) ^ u64::from(tag).wrapping_mul(0x9d)) as u8,
	}
}

/// The `len` bytes of stream `tag` from `offset`.
fn fill(tag: u8, offset: u64, len: usize) -> Vec<u8> {
	(0..len as u64).map(|i| pattern(tag, offset + i)).collect()
}

/// Checks a stream's bytes against the pattern its first byte names.
#[derive(Default)]
struct Reader {
	tag: Option<u8>,
	offset: u64,
}

impl Reader {
	fn feed(&mut self, chunk: &[u8], tainted: bool) {
		for &byte in chunk {
			let tag = *self.tag.get_or_insert(byte);
			if !tainted {
				assert_eq!(
					byte,
					pattern(tag, self.offset),
					"stream {tag} corrupted at {}",
					self.offset
				);
			}
			self.offset += 1;
		}
	}
}

/// The server: accept the session as a relay does, then run the app on it.
async fn serve(mode: Mode, io: DuplexStream, book: Rc<Book>) {
	let config = config(mode.version);
	let accepted = match mode.websocket {
		true => {
			// With the keep-alive the relay's WebSocket listener sets (moq-tokio's `websocket`).
			ws::Upgraded::new(io, mode.version.alpn())
				.with_keep_alive(ws::KeepAlive::default())
				.with_session_config(config)
				.accept()
				.await
				.map(|(session, driver)| (session, victim(driver)))
		}
		false => Session::accept(Stream::new(io), config)
			.await
			.map(|(session, driver)| (session, victim(driver))),
	};
	let Ok((session, driver)) = accepted else {
		return;
	};
	victim(accept_uni(session.clone(), mode, book.clone()));
	victim(accept_bi(session.clone(), mode, book.clone()));
	if mode.reads {
		victim(datagrams(session.clone(), book.clone()));
	}
	let _ = session.closed().await;
	drop(session);
	let _ = driver.await;
}

async fn accept_uni(session: Session, mode: Mode, book: Rc<Book>) {
	let _task = Task::new(&book);
	// Streams the app never reads, kept until the session ends.
	let mut held = Vec::new();
	while let Ok(recv) = session.accept_uni().await {
		match mode.reads {
			true => drop(victim(read_uni(recv, book.clone()))),
			false => held.push(recv),
		}
	}
}

async fn read_uni(mut recv: RecvStream, book: Rc<Book>) {
	let _task = Task::new(&book);
	let mut reader = Reader::default();
	loop {
		let (chunk, fin) = match recv.read_chunk(usize::MAX).await {
			Ok(Some(chunk)) => (chunk, false),
			Ok(None) => (Bytes::new(), true),
			Err(_) => return,
		};
		reader.feed(&chunk, book.tainted.get());
		// Reported as it arrives, since a stream the peer never finishes never ends here.
		if let Some(tag) = reader.tag
			&& let Some(report) = book.uni.borrow_mut().get_mut(usize::from(tag))
		{
			*report = Report {
				received: reader.offset,
				fin,
			};
		}
		if fin {
			return;
		}
	}
}

async fn accept_bi(session: Session, mode: Mode, book: Rc<Book>) {
	let _task = Task::new(&book);
	let mut held = Vec::new();
	while let Ok((send, recv)) = session.accept_bi().await {
		match mode.reads {
			true => drop(victim(echo(send, recv, book.clone()))),
			false => held.push((send, recv)),
		}
	}
}

/// Write back everything the stream carries, then finish.
async fn echo(mut send: SendStream, mut recv: RecvStream, book: Rc<Book>) {
	let _task = Task::new(&book);
	loop {
		match recv.read_chunk(usize::MAX).await {
			Ok(Some(chunk)) => {
				if send.write_chunk(chunk).await.is_err() {
					return;
				}
			}
			Ok(None) => {
				let _ = send.finish();
				return;
			}
			Err(_) => return,
		}
	}
}

async fn datagrams(session: Session, book: Rc<Book>) {
	let _task = Task::new(&book);
	while let Ok(datagram) = session.recv_datagram().await {
		if !book.tainted.get() {
			let mut reader = Reader::default();
			reader.feed(&datagram, false);
		}
	}
}

/// What the peer's script does.
#[derive(Clone, Debug)]
pub(super) enum Action {
	OpenUni,
	OpenBi,
	/// Write the next `len` bytes of a stream's pattern, as much as it takes now.
	Write {
		stream: u8,
		len: u16,
	},
	Finish {
		stream: u8,
	},
	Reset {
		stream: u8,
		code: u8,
	},
	Stop {
		stream: u8,
		code: u8,
	},
	/// Read what the server echoed on a stream.
	Read {
		stream: u8,
	},
	Datagram {
		len: u16,
	},
	/// One record of arbitrary frames, framed by the transport.
	Record(Vec<u8>),
	/// Arbitrary bytes straight onto the socket.
	Raw(Vec<u8>),
	/// Stop or resume reading the socket.
	Stall(bool),
	Advance {
		ticks: u8,
	},
	Close {
		code: u8,
	},
	/// Drop the socket without a close.
	Abort,
}

impl Action {
	const OPS: u8 = 14;

	pub(super) fn encode(&self, out: &mut Vec<u8>) {
		let bytes = |out: &mut Vec<u8>, op: u8, data: &[u8]| {
			out.push(op);
			out.extend_from_slice(&(data.len() as u16).to_be_bytes());
			out.extend_from_slice(data);
		};
		match self {
			Self::OpenUni => out.push(0),
			Self::OpenBi => out.push(1),
			Self::Write { stream, len } => {
				out.extend_from_slice(&[2, *stream]);
				out.extend_from_slice(&len.to_be_bytes());
			}
			Self::Finish { stream } => out.extend_from_slice(&[3, *stream]),
			Self::Reset { stream, code } => out.extend_from_slice(&[4, *stream, *code]),
			Self::Stop { stream, code } => out.extend_from_slice(&[5, *stream, *code]),
			Self::Read { stream } => out.extend_from_slice(&[6, *stream]),
			Self::Datagram { len } => {
				out.push(7);
				out.extend_from_slice(&len.to_be_bytes());
			}
			Self::Record(data) => bytes(out, 8, data),
			Self::Raw(data) => bytes(out, 9, data),
			Self::Stall(stall) => out.extend_from_slice(&[10, u8::from(*stall)]),
			Self::Advance { ticks } => out.extend_from_slice(&[11, *ticks]),
			Self::Close { code } => out.extend_from_slice(&[12, *code]),
			Self::Abort => out.push(13),
		}
	}
}

struct Script<'a>(&'a [u8]);

impl Script<'_> {
	fn byte(&mut self) -> Option<u8> {
		let (&byte, rest) = self.0.split_first()?;
		self.0 = rest;
		Some(byte)
	}

	fn u16(&mut self) -> Option<u16> {
		Some(u16::from_be_bytes([self.byte()?, self.byte()?]))
	}

	fn bytes(&mut self) -> Option<Vec<u8>> {
		let len = usize::from(self.u16()?).min(self.0.len());
		let (data, rest) = self.0.split_at(len);
		self.0 = rest;
		Some(data.to_vec())
	}

	fn next(&mut self) -> Option<Action> {
		Some(match self.byte()? % Action::OPS {
			0 => Action::OpenUni,
			1 => Action::OpenBi,
			2 => Action::Write {
				stream: self.byte()?,
				len: self.u16()?,
			},
			3 => Action::Finish { stream: self.byte()? },
			4 => Action::Reset {
				stream: self.byte()?,
				code: self.byte()?,
			},
			5 => Action::Stop {
				stream: self.byte()?,
				code: self.byte()?,
			},
			6 => Action::Read { stream: self.byte()? },
			7 => Action::Datagram { len: self.u16()? },
			8 => Action::Record(self.bytes()?),
			9 => Action::Raw(self.bytes()?),
			10 => Action::Stall(self.byte()? & 1 != 0),
			11 => Action::Advance { ticks: self.byte()? },
			12 => Action::Close { code: self.byte()? },
			_ => Action::Abort,
		})
	}
}

/// The peer's socket: a pipe it can stop reading, and write past its own session onto.
#[derive(Clone)]
struct Tap(Rc<RefCell<TapState>>);

struct TapState {
	io: DuplexStream,
	stalled: bool,
	/// The reader parked on a stall.
	reader: Option<Waker>,
}

impl Tap {
	fn stall(&self, stall: bool) {
		let mut state = self.0.borrow_mut();
		state.stalled = stall;
		if !stall && let Some(reader) = state.reader.take() {
			reader.wake();
		}
	}

	/// Write as much of `data` as the pipe takes now, wherever the session's own writes left off.
	fn raw(&self, data: &[u8]) {
		let mut cx = Context::from_waker(Waker::noop());
		let _ = Pin::new(&mut self.0.borrow_mut().io).poll_write(&mut cx, data);
	}

	/// Drop the socket under the peer's session.
	fn abort(&self) {
		let mut cx = Context::from_waker(Waker::noop());
		let _ = Pin::new(&mut self.0.borrow_mut().io).poll_shutdown(&mut cx);
	}
}

impl AsyncRead for Tap {
	fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
		let mut state = self.0.borrow_mut();
		if state.stalled {
			state.reader = Some(cx.waker().clone());
			return Poll::Pending;
		}
		Pin::new(&mut state.io).poll_read(cx, buf)
	}
}

impl AsyncWrite for Tap {
	fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
		Pin::new(&mut self.0.borrow_mut().io).poll_write(cx, buf)
	}

	fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
		Pin::new(&mut self.0.borrow_mut().io).poll_flush(cx)
	}

	fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
		Pin::new(&mut self.0.borrow_mut().io).poll_shutdown(cx)
	}
}

/// The peer's transport, with records of arbitrary frames slipped in between its own.
struct Inject<T> {
	inner: T,
	queue: Rc<RefCell<Injected>>,
}

#[derive(Default)]
struct Injected {
	records: std::collections::VecDeque<Bytes>,
	waiters: kio::WaiterList,
}

impl<T: Transport> Transport for Inject<T> {
	fn bind(&mut self, config: &Config) {
		self.inner.bind(config);
	}

	fn poll_recv(&mut self, waiter: &kio::Waiter) -> Poll<Result<Bytes, Error>> {
		// The driver polls the receive side when its waiter wakes, and flushes after, so
		// queue the injected records here.
		let mut queue = self.queue.borrow_mut();
		while !queue.records.is_empty() {
			match self.inner.poll_ready(waiter) {
				Poll::Ready(Ok(())) => {
					let record = queue.records.pop_front().expect("not empty");
					self.inner.send(record)?;
				}
				Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
				Poll::Pending => break,
			}
		}
		queue.waiters.register(waiter);
		drop(queue);
		self.inner.poll_recv(waiter)
	}

	fn poll_ready(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		self.inner.poll_ready(waiter)
	}

	fn send(&mut self, record: Bytes) -> Result<(), Error> {
		self.inner.send(record)
	}

	fn poll_flush(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		self.inner.poll_flush(waiter)
	}

	fn poll_close(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		self.inner.poll_close(waiter)
	}
}

/// One stream the peer opened.
struct PeerStream {
	tag: u8,
	bi: bool,
	send: Option<SendStream>,
	recv: Option<RecvStream>,
	written: u64,
	finished: bool,
	/// The peer reset or stopped it, or the server did, so it is out of the round-trip check.
	cut: bool,
	echo: Reader,
	echo_fin: bool,
}

struct Peer {
	session: Option<Session>,
	driver: Option<JoinHandle<()>>,
	tap: Tap,
	injected: Rc<RefCell<Injected>>,
	streams: Vec<PeerStream>,
	opened: u8,
	datagrams: u8,
	advance: Duration,
	book: Rc<Book>,
}

impl Peer {
	async fn connect(mode: Mode, io: DuplexStream, book: Rc<Book>) -> Option<Self> {
		let tap = Tap(Rc::new(RefCell::new(TapState {
			io,
			stalled: false,
			reader: None,
		})));
		let injected = Rc::new(RefCell::new(Injected::default()));
		let config = config(mode.version);
		let (session, driver) = match mode.websocket {
			true => {
				let socket =
					WebSocketStream::from_raw_socket(tap.clone(), Role::Client, Some(ws::capped(&config))).await;
				let transport = Inject {
					inner: WsTransport::new(socket, moq_net::transport::Address::Uri),
					queue: injected.clone(),
				};
				let (session, driver) = Session::start(transport, Side::Client, config).await.ok()?;
				(session, sim::spawn(driver))
			}
			false => {
				let transport = Inject {
					inner: Stream::new(tap.clone()),
					queue: injected.clone(),
				};
				let (session, driver) = Session::start(transport, Side::Client, config).await.ok()?;
				(session, sim::spawn(driver))
			}
		};
		Some(Self {
			session: Some(session),
			driver: Some(driver),
			tap,
			injected,
			streams: Vec::new(),
			opened: 0,
			datagrams: 0,
			advance: ADVANCE,
			book,
		})
	}

	/// Do `action` without waiting: whatever the session cannot take at once is skipped.
	async fn play(&mut self, action: Action) {
		let waiter = kio::Waiter::noop();
		match action {
			Action::OpenUni | Action::OpenBi => {
				let Some(session) = &self.session else { return };
				if self.streams.len() == STREAMS {
					return;
				}
				let opened = match action {
					Action::OpenUni => match session.poll_open_uni(&waiter, SendOptions::default()) {
						Poll::Ready(Ok(send)) => Some((send, None)),
						_ => None,
					},
					_ => match session.poll_open_bi(&waiter, SendOptions::default()) {
						Poll::Ready(Ok((send, recv))) => Some((send, Some(recv))),
						_ => None,
					},
				};
				if let Some((send, recv)) = opened {
					self.streams.push(PeerStream {
						tag: self.opened,
						bi: recv.is_some(),
						send: Some(send),
						recv,
						written: 0,
						finished: false,
						cut: false,
						echo: Reader::default(),
						echo_fin: false,
					});
					self.opened += 1;
				}
			}
			Action::Write { stream, len } => {
				let Some(stream) = self.stream(stream) else { return };
				let Some(send) = stream.send.as_mut().filter(|_| !stream.finished) else {
					return;
				};
				let data = fill(stream.tag, stream.written, usize::from(len).min(WRITE));
				match send.poll_write(&waiter, &data) {
					Poll::Ready(Ok(n)) => stream.written += n as u64,
					Poll::Ready(Err(_)) => stream.cut = true,
					Poll::Pending => {}
				}
			}
			Action::Finish { stream } => {
				let Some(stream) = self.stream(stream) else { return };
				if let Some(send) = &mut stream.send {
					match send.finish() {
						Ok(()) => stream.finished = true,
						Err(_) => stream.cut = true,
					}
				}
			}
			Action::Reset { stream, code } => {
				let Some(stream) = self.stream(stream) else { return };
				if let Some(send) = &mut stream.send {
					send.reset(code.into());
					stream.cut = true;
				}
			}
			Action::Stop { stream, code } => {
				let Some(stream) = self.stream(stream) else { return };
				if let Some(recv) = &mut stream.recv {
					recv.stop(code.into());
					stream.cut = true;
				}
			}
			Action::Read { stream } => {
				let tainted = self.book.tainted.get();
				if let Some(stream) = self.stream(stream) {
					stream.read(tainted);
				}
			}
			Action::Datagram { len } => {
				let Some(session) = &self.session else { return };
				let len = usize::from(len).min(WRITE).min(session.max_datagram_size());
				let data = fill(self.datagrams, 0, len);
				self.datagrams = self.datagrams.wrapping_add(1);
				let _ = session.send_datagram(data.into(), SendOptions::default());
			}
			Action::Record(record) => {
				self.book.tainted.set(true);
				let mut injected = self.injected.borrow_mut();
				injected.records.push_back(record.into());
				injected.waiters.wake();
			}
			Action::Raw(data) => {
				self.book.tainted.set(true);
				self.tap.raw(&data);
			}
			Action::Stall(stall) => self.tap.stall(stall),
			Action::Advance { ticks } => {
				let advance = self.advance.min(TICK * u32::from(ticks));
				self.advance -= advance;
				kio::clock::sleep(advance).await;
			}
			Action::Close { code } => {
				if let Some(session) = self.session.take() {
					session.close(code.into(), "");
				}
				self.streams.clear();
			}
			Action::Abort => {
				self.session = None;
				self.streams.clear();
				if let Some(driver) = self.driver.take() {
					driver.abort();
				}
				self.tap.abort();
			}
		}
	}

	fn stream(&mut self, index: u8) -> Option<&mut PeerStream> {
		let len = self.streams.len();
		self.streams.get_mut(usize::from(index) % len.max(1))
	}

	fn read_all(&mut self) {
		let tainted = self.book.tainted.get();
		for stream in &mut self.streams {
			stream.read(tainted);
		}
	}

	/// The round trip held, unless something besides well-formed traffic went out or a side
	/// ended the session.
	fn verify(&self, mode: Mode) {
		let Some(session) = &self.session else { return };
		if self.book.tainted.get() || !mode.reads {
			return;
		}
		if session.poll_closed(&kio::Waiter::noop()).is_ready() {
			return;
		}
		let uni = self.book.uni.borrow();
		for stream in self.streams.iter().filter(|stream| !stream.cut) {
			match stream.bi {
				false => {
					let report = uni[usize::from(stream.tag)];
					// A stream with nothing written never reached the server.
					if stream.written == 0 {
						continue;
					}
					assert_eq!(report.received, stream.written, "stream {} lost bytes", stream.tag);
					assert_eq!(report.fin, stream.finished, "stream {} ended wrong", stream.tag);
				}
				true => {
					assert_eq!(
						stream.echo.offset, stream.written,
						"stream {} echo lost bytes",
						stream.tag
					);
					assert_eq!(
						stream.echo_fin, stream.finished,
						"stream {} echo ended wrong",
						stream.tag
					);
				}
			}
		}
	}

	/// Close the session and let the driver finish delivering it. Dropping the handles,
	/// the socket's among them, leaves the driver the socket's last owner, so it closes
	/// once the driver is done, as a peer's TCP connection does.
	fn leave(self) {}
}

impl PeerStream {
	fn read(&mut self, tainted: bool) {
		let Some(recv) = &mut self.recv else { return };
		loop {
			match recv.poll_read_chunk(&kio::Waiter::noop(), usize::MAX) {
				Poll::Ready(Ok(Some(chunk))) => self.echo.feed(&chunk, tainted),
				Poll::Ready(Ok(None)) => {
					self.echo_fin = true;
					break;
				}
				Poll::Ready(Err(_)) => {
					self.cut = true;
					break;
				}
				Poll::Pending => break,
			}
		}
		if self.echo_fin || self.cut {
			self.recv = None;
		}
	}
}
