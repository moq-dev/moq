//! Transport doubles shared by the lite tests: a session whose streams record
//! everything written and every reset code, and peers that never speak.

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
	bi_opens: Arc<AtomicUsize>,
}

impl Log {
	pub fn resets(&self) -> Vec<u32> {
		self.resets.lock().unwrap().clone()
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
}

impl SinkSend {
	pub fn new(log: Log) -> Self {
		Self { log, gate: None }
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

	fn set_priority(&mut self, _order: u8) {}

	fn finish(&mut self) -> Result<(), Self::Error> {
		Ok(())
	}

	fn reset(&mut self, code: u32) {
		self.log.resets.lock().unwrap().push(code);
	}

	async fn closed(&mut self) -> Result<(), Self::Error> {
		std::future::pending().await
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

/// Send streams record their bytes while peer reads park forever.
#[derive(Clone, Default)]
pub struct SinkSession {
	pub log: Log,
	/// Set by [`Self::gated_bi`]. `None` parks `open_bi` itself forever, which is all
	/// a test driving only uni streams needs.
	bi_gate: Option<kio::Consumer<bool>>,
}

impl SinkSession {
	pub fn new(log: Log) -> Self {
		Self { log, bi_gate: None }
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
		};
		Ok((send, PendingRecv))
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

	fn close(&self, _code: u32, _reason: &str) {}

	async fn closed(&self) -> Self::Error {
		std::future::pending().await
	}

	fn stats(&self) -> impl web_transport_trait::Stats {
		SinkStats
	}
}

pub struct SinkStats;

impl web_transport_trait::Stats for SinkStats {
	fn estimated_send_rate(&self) -> Option<u64> {
		None
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

	fn stop(&mut self, _code: u32) {}

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
}

impl ScriptedSession {
	pub fn new(script: Vec<u8>) -> Self {
		Self {
			log: Log::default(),
			eof: false,
			script: Arc::new(Mutex::new(script)),
			queue: None,
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
		self.log.bi_opens.fetch_add(1, Ordering::Relaxed);
		let script = match &self.queue {
			Some(queue) => Arc::new(Mutex::new(queue.lock().unwrap().pop_front().unwrap_or_default())),
			None => self.script.clone(),
		};
		Ok((SinkSend::new(self.log.clone()), ScriptedRecv { script, eof: self.eof }))
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

	fn close(&self, _code: u32, _reason: &str) {}

	async fn closed(&self) -> Self::Error {
		std::future::pending().await
	}

	fn stats(&self) -> impl web_transport_trait::Stats {
		SinkStats
	}
}
