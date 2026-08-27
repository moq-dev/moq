//! Transport doubles shared by the lite tests: a session whose streams record
//! everything written and every reset code, peers that never speak, and peers
//! whose incoming streams die before their first byte.

use std::{
	sync::{
		Arc, Mutex,
		atomic::{AtomicUsize, Ordering},
	},
	task::Poll,
};

#[derive(Debug, Clone, Default)]
pub struct SinkError;

impl std::fmt::Display for SinkError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "sink transport error")
	}
}

impl std::error::Error for SinkError {}

impl web_transport_trait::Error for SinkError {
	fn session_error(&self) -> Option<(u32, String)> {
		Some((0, "closed".to_string()))
	}
}

/// What the session's send streams recorded. Resets are a list, not a last-value, so a
/// stray second reset (a drop fallback firing behind an abort) is visible.
#[derive(Clone, Default)]
pub struct Log {
	pub writes: Arc<Mutex<Vec<u8>>>,
	pub resets: Arc<Mutex<Vec<u32>>>,
	stops: Arc<Mutex<Vec<u32>>>,
	closes: Arc<Mutex<Vec<(u32, String)>>>,
	bi_opens: Arc<AtomicUsize>,
	priorities: Arc<Mutex<Vec<u8>>>,
}

impl Log {
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

	/// The session-level closes, as (code, reason). A list rather than a last-value so
	/// a second close racing the first is visible.
	pub fn closes(&self) -> Vec<(u32, String)> {
		self.closes.lock().unwrap().clone()
	}

	/// How many bidi streams the session has opened, so a test can pin down how many
	/// requests were sent and not just what they said.
	pub fn bi_opens(&self) -> usize {
		self.bi_opens.load(Ordering::Relaxed)
	}
}

pub struct SinkSend {
	pub log: Log,
	/// Writes park until this flips to true; `None` writes immediately. See
	/// [`SinkSession::gated_bi`].
	gate: Option<kio::Consumer<bool>>,
	/// Set by [`finish`](web_transport_trait::SendStream::finish). A finished stream is what
	/// [`closed`](web_transport_trait::SendStream::closed) waits on, mirroring a peer that
	/// acknowledges the FIN; an unfinished one parks like a peer that never answers.
	finished: bool,
}

impl SinkSend {
	pub fn new(log: Log) -> Self {
		Self {
			log,
			gate: None,
			finished: false,
		}
	}
}

impl web_transport_trait::SendStream for SinkSend {
	type Error = SinkError;

	async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
		if let Some(gate) = &self.gate {
			// A closed gate parks forever, which surfaces as a hung test rather than a
			// write that silently skipped the gate.
			let _ = gate
				.wait(|open| if **open { Poll::Ready(()) } else { Poll::Pending })
				.await;
		}
		self.log.writes.lock().unwrap().extend_from_slice(buf);
		Ok(buf.len())
	}

	fn set_priority(&mut self, order: u8) {
		self.log.priorities.lock().unwrap().push(order);
	}

	fn finish(&mut self) -> Result<(), Self::Error> {
		self.finished = true;
		Ok(())
	}

	/// Always recorded, even after a finish: quinn resets a stream that has sent its FIN but
	/// still has unacknowledged data, which is exactly the case that loses a final message.
	fn reset(&mut self, code: u32) {
		self.log.resets.lock().unwrap().push(code);
	}

	async fn closed(&mut self) -> Result<(), Self::Error> {
		match self.finished {
			true => Ok(()),
			// Nothing to acknowledge yet, so park like a peer that never answers.
			false => std::future::pending().await,
		}
	}
}

/// A peer that never speaks and never closes, so a test only ever acts on
/// model-side events.
pub struct PendingRecv;

impl web_transport_trait::RecvStream for PendingRecv {
	type Error = SinkError;

	async fn read(&mut self, _dst: &mut [u8]) -> Result<Option<usize>, Self::Error> {
		std::future::pending().await
	}

	fn stop(&mut self, _code: u32) {}

	async fn closed(&mut self) -> Result<(), Self::Error> {
		std::future::pending().await
	}
}

/// What a reset stream's reads report: RESET_STREAM with application code 0,
/// the code a routine group drop carries. `Error::from_transport` decodes it
/// back to `Error::Cancel`.
#[derive(Debug, Clone, Default)]
pub struct ResetError;

impl std::fmt::Display for ResetError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "stream reset by peer (code 0)")
	}
}

impl std::error::Error for ResetError {}

impl web_transport_trait::Error for ResetError {
	fn session_error(&self) -> Option<(u32, String)> {
		None
	}

	fn stream_error(&self) -> Option<u32> {
		Some(0)
	}
}

/// A stream that died before delivering a single byte: every read reports a
/// code-0 RESET_STREAM ([`ResetError`]), the wire shape of a reset arriving
/// ahead of any payload. QUIC does not order a reset behind the data, so this
/// reaches an accept loop in normal operation, not just from a misbehaving peer.
pub struct DeadRecv;

impl web_transport_trait::RecvStream for DeadRecv {
	type Error = ResetError;

	async fn read(&mut self, _dst: &mut [u8]) -> Result<Option<usize>, Self::Error> {
		Err(ResetError)
	}

	fn stop(&mut self, _code: u32) {}

	async fn closed(&mut self) -> Result<(), Self::Error> {
		Err(ResetError)
	}
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
}

impl DeadStreamSession {
	/// `count` uni streams that die before their first byte, then silence.
	pub fn unis(count: usize) -> Self {
		Self {
			log: Log::default(),
			unis: Arc::new(Mutex::new(count)),
			bis: Arc::new(Mutex::new(0)),
		}
	}

	/// `count` bidi streams that die before their first byte, then silence.
	pub fn bis(count: usize) -> Self {
		Self {
			log: Log::default(),
			unis: Arc::new(Mutex::new(0)),
			bis: Arc::new(Mutex::new(count)),
		}
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

impl web_transport_trait::Session for DeadStreamSession {
	type SendStream = SinkSend;
	type RecvStream = DeadRecv;
	type Error = SinkError;

	async fn accept_uni(&self) -> Result<Self::RecvStream, Self::Error> {
		match Self::take(&self.unis) {
			true => Ok(DeadRecv),
			false => std::future::pending().await,
		}
	}

	async fn accept_bi(&self) -> Result<(Self::SendStream, Self::RecvStream), Self::Error> {
		match Self::take(&self.bis) {
			true => Ok((SinkSend::new(self.log.clone()), DeadRecv)),
			false => std::future::pending().await,
		}
	}

	async fn open_bi(&self) -> Result<(Self::SendStream, Self::RecvStream), Self::Error> {
		self.log.bi_opens.fetch_add(1, Ordering::Relaxed);
		Ok((SinkSend::new(self.log.clone()), DeadRecv))
	}

	async fn open_uni(&self) -> Result<Self::SendStream, Self::Error> {
		Ok(SinkSend::new(self.log.clone()))
	}

	fn send_datagram(&self, _payload: bytes::Bytes) -> Result<(), Self::Error> {
		Ok(())
	}

	async fn recv_datagram(&self) -> Result<bytes::Bytes, Self::Error> {
		std::future::pending().await
	}

	fn max_datagram_size(&self) -> usize {
		0
	}

	fn protocol(&self) -> Option<&str> {
		None
	}

	fn close(&self, code: u32, reason: &str) {
		self.log.closes.lock().unwrap().push((code, reason.to_owned()));
	}

	async fn closed(&self) -> Self::Error {
		std::future::pending().await
	}

	fn stats(&self) -> impl web_transport_trait::Stats {
		SinkStats::default()
	}
}

/// Send streams record their bytes while peer reads park forever.
#[derive(Clone, Default)]
pub struct SinkSession {
	pub log: Log,
	/// Set by [`Self::gated_bi`]. `None` parks `open_bi` itself forever, which is all
	/// a test driving only uni streams needs.
	bi_gate: Option<kio::Consumer<bool>>,
	/// Set by [`Self::gated_uni`]. `None` writes immediately.
	uni_gate: Option<kio::Consumer<bool>>,
	/// The ALPN to report, for a test that needs a specific negotiated version rather
	/// than the SETUP-negotiated fallback an absent one selects.
	protocol: Option<&'static str>,
	/// What the transport claims to measure. Defaults to nothing, like a transport
	/// with no congestion controller exposed. Shared and mutable so a test can
	/// change it mid-session, the way a real transport's figures move.
	stats: Arc<Mutex<SinkStats>>,
}

impl SinkSession {
	pub fn new(log: Log) -> Self {
		Self {
			log,
			bi_gate: None,
			uni_gate: None,
			protocol: None,
			stats: Arc::new(Mutex::new(SinkStats::default())),
		}
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
			log: Log::default(),
			bi_gate: Some(gate),
			uni_gate: None,
			protocol: None,
			stats: Arc::new(Mutex::new(SinkStats::default())),
		}
	}

	/// Serve uni streams, holding every write while `gate` reads false.
	///
	/// Groups travel on uni streams, so this is how a test stalls delivery the way
	/// QUIC flow control does: the publisher keeps its consumer and its place in the
	/// group, but no byte reaches the wire until the gate reopens.
	pub fn gated_uni(gate: kio::Consumer<bool>) -> Self {
		Self {
			log: Log::default(),
			bi_gate: None,
			uni_gate: Some(gate),
			protocol: None,
			stats: Arc::new(Mutex::new(SinkStats::default())),
		}
	}
}

impl web_transport_trait::Session for SinkSession {
	type SendStream = SinkSend;
	type RecvStream = PendingRecv;
	type Error = SinkError;

	async fn accept_uni(&self) -> Result<Self::RecvStream, Self::Error> {
		std::future::pending().await
	}

	async fn accept_bi(&self) -> Result<(Self::SendStream, Self::RecvStream), Self::Error> {
		std::future::pending().await
	}

	async fn open_bi(&self) -> Result<(Self::SendStream, Self::RecvStream), Self::Error> {
		let Some(gate) = self.bi_gate.clone() else {
			return std::future::pending().await;
		};
		self.log.bi_opens.fetch_add(1, Ordering::Relaxed);

		let send = SinkSend {
			log: self.log.clone(),
			gate: Some(gate),
			finished: false,
		};
		Ok((send, PendingRecv))
	}

	async fn open_uni(&self) -> Result<Self::SendStream, Self::Error> {
		Ok(SinkSend {
			log: self.log.clone(),
			gate: self.uni_gate.clone(),
			finished: false,
		})
	}

	fn send_datagram(&self, _payload: bytes::Bytes) -> Result<(), Self::Error> {
		Ok(())
	}

	async fn recv_datagram(&self) -> Result<bytes::Bytes, Self::Error> {
		std::future::pending().await
	}

	fn max_datagram_size(&self) -> usize {
		0
	}

	fn protocol(&self) -> Option<&str> {
		self.protocol
	}

	fn close(&self, code: u32, reason: &str) {
		self.log.closes.lock().unwrap().push((code, reason.to_owned()));
	}

	async fn closed(&self) -> Self::Error {
		std::future::pending().await
	}

	fn stats(&self) -> impl web_transport_trait::Stats {
		*self.stats.lock().unwrap()
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

impl web_transport_trait::Stats for SinkStats {
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
pub struct ScriptedRecv {
	script: Arc<Mutex<Vec<u8>>>,
	/// Report EOF once the script is exhausted rather than parking, so a test can drive
	/// a read loop all the way through its exit path. See [`ScriptedSession::eof`].
	eof: bool,
	log: Log,
}

impl web_transport_trait::RecvStream for ScriptedRecv {
	type Error = SinkError;

	async fn read(&mut self, dst: &mut [u8]) -> Result<Option<usize>, Self::Error> {
		let take = {
			let mut script = self.script.lock().unwrap();
			if script.is_empty() {
				0
			} else {
				let take = dst.len().min(script.len());
				dst[..take].copy_from_slice(&script[..take]);
				script.drain(..take);
				take
			}
		};

		match take {
			0 if self.eof => Ok(None),
			0 => std::future::pending().await,
			take => Ok(Some(take)),
		}
	}

	fn stop(&mut self, code: u32) {
		self.log.stops.lock().unwrap().push(code);
	}

	async fn closed(&mut self) -> Result<(), Self::Error> {
		std::future::pending().await
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
	/// Whether an exhausted script reports EOF instead of parking.
	eof: bool,
	script: Arc<Mutex<Vec<u8>>>,
	/// Per-stream scripts popped by `open_bi` in order; `None` shares `script`
	/// across every stream.
	queue: Option<Arc<Mutex<std::collections::VecDeque<Vec<u8>>>>>,
	/// Set by [`Self::gated_open`]: parks `open_bi` until the gate opens, standing in for
	/// a peer that has granted no more concurrent streams.
	open_gate: Option<kio::Consumer<bool>>,
}

impl ScriptedSession {
	pub fn new(script: Vec<u8>) -> Self {
		Self {
			log: Log::default(),
			eof: false,
			script: Arc::new(Mutex::new(script)),
			queue: None,
			open_gate: None,
		}
	}

	/// Replay `script`, then close the stream instead of parking.
	///
	/// Parking is the right default for asserting that a loop is still running, but a
	/// test for what a loop does on the way *out* needs the read to actually end.
	pub fn eof(script: Vec<u8>) -> Self {
		Self {
			eof: true,
			..Self::new(script)
		}
	}

	/// Each `open_bi` replays the next script in order. An exhausted queue (or an
	/// empty entry) reads as a peer that never speaks.
	pub fn per_stream(scripts: Vec<Vec<u8>>) -> Self {
		Self {
			log: Log::default(),
			eof: false,
			script: Arc::new(Mutex::new(Vec::new())),
			queue: Some(Arc::new(Mutex::new(scripts.into_iter().collect()))),
			open_gate: None,
		}
	}

	/// Answer each stream from `scripts`, but only once the gate opens: a peer that
	/// replies normally and is simply out of stream credit until then.
	pub fn gated_open(scripts: Vec<Vec<u8>>, gate: kio::Consumer<bool>) -> Self {
		Self {
			open_gate: Some(gate),
			..Self::per_stream(scripts)
		}
	}
}

impl web_transport_trait::Session for ScriptedSession {
	type SendStream = SinkSend;
	type RecvStream = ScriptedRecv;
	type Error = SinkError;

	async fn accept_uni(&self) -> Result<Self::RecvStream, Self::Error> {
		std::future::pending().await
	}

	async fn accept_bi(&self) -> Result<(Self::SendStream, Self::RecvStream), Self::Error> {
		std::future::pending().await
	}

	async fn open_bi(&self) -> Result<(Self::SendStream, Self::RecvStream), Self::Error> {
		if let Some(gate) = &self.open_gate {
			let _ = gate
				.wait(|open| if **open { Poll::Ready(()) } else { Poll::Pending })
				.await;
		}

		self.log.bi_opens.fetch_add(1, Ordering::Relaxed);
		let script = match &self.queue {
			Some(queue) => Arc::new(Mutex::new(queue.lock().unwrap().pop_front().unwrap_or_default())),
			None => self.script.clone(),
		};
		Ok((
			SinkSend::new(self.log.clone()),
			ScriptedRecv {
				script,
				eof: self.eof,
				log: self.log.clone(),
			},
		))
	}

	async fn open_uni(&self) -> Result<Self::SendStream, Self::Error> {
		Ok(SinkSend::new(self.log.clone()))
	}

	fn send_datagram(&self, _payload: bytes::Bytes) -> Result<(), Self::Error> {
		Ok(())
	}

	async fn recv_datagram(&self) -> Result<bytes::Bytes, Self::Error> {
		std::future::pending().await
	}

	fn max_datagram_size(&self) -> usize {
		0
	}

	fn protocol(&self) -> Option<&str> {
		None
	}

	fn close(&self, code: u32, reason: &str) {
		self.log.closes.lock().unwrap().push((code, reason.to_owned()));
	}

	async fn closed(&self) -> Self::Error {
		std::future::pending().await
	}

	fn stats(&self) -> impl web_transport_trait::Stats {
		SinkStats::default()
	}
}
