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
