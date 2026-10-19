//! One-shot ingress bursts for transport tests, enabled by the `test` feature.

use super::{Draw, Rate, UdpSocket};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;

/// The direction from clients to the target, or back to clients.
#[derive(Clone, Copy, Debug)]
pub enum Direction {
	/// Datagrams sent by clients toward the target.
	Up,
	/// Datagrams sent by the target toward clients.
	Down,
}

/// An armed ingress burst; dropping it cancels and discards held datagrams.
#[must_use = "dropping the burst cancels it and discards held datagrams"]
pub struct Burst {
	state: Arc<Mutex<State>>,
	done: oneshot::Receiver<()>,
}

impl Burst {
	/// Wait until the collected datagrams have been applied to the rate bucket.
	pub async fn completed(mut self) -> anyhow::Result<()> {
		(&mut self.done)
			.await
			.map_err(|_| anyhow::anyhow!("ingress burst ended before applying"))
	}
}

impl Drop for Burst {
	fn drop(&mut self) {
		self.state.lock().expect("ingress poisoned").cancel();
	}
}

#[derive(Default)]
pub(super) struct State {
	flow: Option<u64>,
	bytes: u64,
	held: Vec<Held>,
	pub(super) cancelled: bool,
	pub(super) finished: bool,
	done: Option<oneshot::Sender<()>>,
}

impl State {
	fn cancel(&mut self) {
		self.cancelled = true;
		self.held.clear();
		self.done.take();
	}

	pub(super) fn hold(&mut self, rate: &Rate, packet: Held) -> Option<Released> {
		self.bytes += packet.datagram.len() as u64;
		self.held.push(packet);
		// Even an initially full bucket cannot fit these bytes, independent of
		// how ingress was scheduled.
		if rate.cost(self.bytes) <= rate.cost(rate.burst) + rate.queue {
			return None;
		}
		Some(Released {
			packets: std::mem::take(&mut self.held),
			done: self.done.take(),
		})
	}
}

pub(super) struct Held {
	pub datagram: Vec<u8>,
	pub socket: Arc<UdpSocket>,
	pub dest: SocketAddr,
	pub draw: Draw,
}

pub(super) struct Released {
	pub packets: Vec<Held>,
	pub done: Option<oneshot::Sender<()>>,
}

#[derive(Clone, Default)]
pub(super) struct Control(Arc<Mutex<Option<Arc<Mutex<State>>>>>);

impl Control {
	pub(super) fn arm(&self) -> anyhow::Result<Burst> {
		let mut active = self.0.lock().expect("ingress poisoned");
		if let Some(state) = active.as_ref() {
			let state = state.lock().expect("ingress poisoned");
			anyhow::ensure!(state.cancelled || state.finished, "an ingress burst is already armed");
		}
		let (send, done) = oneshot::channel();
		let state = Arc::new(Mutex::new(State {
			done: Some(send),
			..Default::default()
		}));
		*active = Some(state.clone());
		Ok(Burst { state, done })
	}

	pub(super) fn selected(&self, flow: u64) -> Option<Arc<Mutex<State>>> {
		let state = self.0.lock().expect("ingress poisoned").as_ref()?.clone();
		{
			let mut held = state.lock().expect("ingress poisoned");
			if held.cancelled || held.done.is_none() || held.flow.is_some_and(|selected| selected != flow) {
				return None;
			}
			held.flow = Some(flow);
		}
		Some(state)
	}

	pub(super) fn cancel(&self) {
		if let Some(state) = self.0.lock().expect("ingress poisoned").take() {
			state.lock().expect("ingress poisoned").cancel();
		}
	}
}
