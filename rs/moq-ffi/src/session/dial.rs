//! A browser connection: one dial its session owns from the start, then the session it
//! establishes.

use std::future::Future;
use std::task::Poll;

use super::{Accepted, MoqConnectionPhase, MoqConnectionState};
use crate::error::MoqError;

/// Why a dial ended without a session.
#[derive(Clone)]
enum Failure {
	/// The caller stopped it.
	Stopped,
	/// WebTransport did not connect.
	Transport(String),
	/// The MoQ handshake failed.
	Handshake(moq_net::Error),
}

impl Failure {
	/// The error a failed dial reports, or `None` for a local stop.
	fn error(&self) -> Option<MoqError> {
		match self {
			Self::Stopped => None,
			Self::Transport(reason) => Some(MoqError::Connect { reason: reason.clone() }),
			Self::Handshake(err) => Some(err.clone().into()),
		}
	}
}

#[derive(Default)]
struct State {
	/// The established session.
	session: Option<Accepted>,
	/// Why the dial ended without one.
	failed: Option<Failure>,
	/// A stop request, which the dial and its session both honor.
	stop: Option<moq_net::Error>,
}

/// One browser dial and the session it establishes, shared by every handle of its session.
///
/// Revisions run 0 while dialing, 1 once the dial settles (connected or failed), and 2
/// once an established session closes.
#[derive(Clone)]
pub(crate) struct Dial {
	state: kio::Producer<State>,
	/// Persistent from before the handshake: the session probes for these reservations.
	bandwidth: moq_net::bandwidth::Allocator,
	/// The session's receive estimate, forwarded while a consumer holds it.
	recv: moq_net::bandwidth::Weak,
	/// Persistent from before the handshake, so a grant consumer taken while dialing follows
	/// the session; ended with the dial.
	auth: moq_net::auth::Handle,
}

impl Dial {
	/// Start dialing, returning before the transport or the handshake completes.
	pub fn start<T, E>(client: moq_net::Client, transport: impl Future<Output = Result<T, E>> + 'static) -> Self
	where
		T: moq_net::transport::poll::Boxable,
		E: std::fmt::Display,
	{
		let state = kio::Producer::new(State::default());
		let send = moq_net::bandwidth::Producer::new();
		let recv = moq_net::bandwidth::Producer::new();
		let bandwidth = moq_net::bandwidth::Allocator::new(send.consume());
		let auth = moq_net::auth::Handle::new();
		let client = client.with_allocator(bandwidth.clone()).with_auth(auth.clone());
		let dial = Self {
			state: state.clone(),
			bandwidth,
			recv: recv.weak(),
			auth: auth.clone(),
		};

		let handshake = async move {
			let transport = transport.await.map_err(|err| Failure::Transport(err.to_string()))?;
			client
				.connect(moq_clock::now(), transport)
				.await
				.map_err(Failure::Handshake)
		};
		crate::ffi::spawn(async move {
			let err = run(state, handshake, send, recv).await;
			// No session will present the tokens again, so they end with the dial. A token
			// reads `Cancel` as its peer ending the grant cleanly.
			auth.abort(match err {
				moq_net::Error::Cancel => moq_net::Error::Closed,
				err => err,
			});
		});
		dial
	}

	/// Stop the dial, or abort the session it established, with `err`.
	pub fn stop(&self, err: moq_net::Error) {
		// Under the lock the dial publishes its session with, so a session landing now is
		// either aborted here or refused by the dial.
		if let Ok(mut state) = self.state.write() {
			if let Some(session) = &state.session {
				session.abort(err.clone());
			}
			state.stop.get_or_insert(err);
		}
	}

	/// The established session, if the dial has completed.
	pub fn session(&self) -> Option<moq_net::Session> {
		self.state
			.read()
			.session
			.as_ref()
			.map(|accepted| accepted.session.clone())
	}

	async fn settled(&self) -> Result<Option<Accepted>, MoqError> {
		let settled = self
			.state
			.consume()
			.wait(|state| match (&state.session, &state.failed) {
				(Some(session), _) => Poll::Ready(Ok(Some(session.clone()))),
				(None, Some(failure)) => Poll::Ready(failure.error().map_or(Ok(None), Err)),
				(None, None) => Poll::Pending,
			})
			.await;
		// The dial task keeps the channel open for as long as any handle exists.
		settled.unwrap_or(Err(MoqError::Closed))
	}

	pub fn state(&self) -> MoqConnectionState {
		let state = self.state.read();
		if let Some(session) = &state.session {
			let mut state = session.state();
			state.revision += 1;
			return state;
		}
		let (revision, phase) = match state.failed {
			Some(_) => (1, MoqConnectionPhase::Closed),
			None => (0, MoqConnectionPhase::Connecting),
		};
		MoqConnectionState {
			revision,
			phase,
			connects: 0,
			failure: state.failed.as_ref().and_then(Failure::error),
			version: None,
		}
	}

	pub async fn state_changed(&self, after: u64) -> Result<MoqConnectionState, MoqError> {
		loop {
			let state = self.state();
			if state.revision != after {
				return Ok(state);
			}
			if state.phase == MoqConnectionPhase::Closed {
				return Err(state.failure.unwrap_or(MoqError::Closed));
			}
			match self.settled().await {
				Ok(Some(session)) if state.phase == MoqConnectionPhase::Connected => {
					let _ = session.closed::<()>().await;
				}
				_ => {}
			}
		}
	}

	/// Wait for the session to end: `Ok` after a local stop, else why it failed or closed.
	pub async fn closed(&self) -> Result<(), MoqError> {
		match self.settled().await? {
			Some(session) => session.closed().await,
			None => Ok(()),
		}
	}

	/// Drain an established session; a dial still running is stopped instead.
	pub async fn close(&self) -> Result<(), MoqError> {
		let session = self.state.read().session.clone();
		match session {
			Some(session) => session.close().await,
			None => {
				self.stop(moq_net::Error::Cancel);
				// A handshake that won the race is published already and aborted by the stop.
				Ok(())
			}
		}
	}

	pub fn allocator(&self) -> moq_net::bandwidth::Allocator {
		self.bandwidth.clone()
	}

	pub fn recv_bandwidth(&self) -> moq_net::bandwidth::Consumer {
		self.recv.consume()
	}

	/// The auth handle every session of this dial is built on.
	pub fn auth(&self) -> moq_net::auth::Handle {
		self.auth.clone()
	}
}

/// Dial, publish the outcome, then drive the session until it closes, returning why it ended.
async fn run(
	state: kio::Producer<State>,
	handshake: impl Future<Output = Result<(moq_net::Session, moq_net::Driver), Failure>>,
	send: moq_net::bandwidth::Producer,
	recv: moq_net::bandwidth::Producer,
) -> moq_net::Error {
	let stopped = state.consume();
	let stopped = stopped.wait(|state| match state.stop {
		Some(_) => Poll::Ready(()),
		None => Poll::Pending,
	});
	let result = tokio::select! {
		biased;
		_ = stopped => Err(Failure::Stopped),
		result = handshake => result,
	};

	let Ok(mut published) = state.write() else {
		return moq_net::Error::Closed;
	};
	let (session, driver) = match result {
		Ok(established) if published.stop.is_none() => established,
		Ok((session, _)) => {
			// The stop arrived during the handshake's last step.
			let err = published.stop.clone().unwrap_or(moq_net::Error::Cancel);
			session.abort(err.clone());
			published.failed = Some(Failure::Stopped);
			return err;
		}
		Err(failure) => {
			let err = match &failure {
				Failure::Stopped => published.stop.clone().unwrap_or(moq_net::Error::Cancel),
				Failure::Transport(reason) => moq_net::Error::Transport(reason.clone()),
				Failure::Handshake(err) => err.clone(),
			};
			published.failed = Some(failure);
			return err;
		}
	};
	// Before the session publishes, so a watcher that sees it already reads its estimate.
	let _ = send.set(session.send_bandwidth().and_then(|bw| bw.peek()));
	published.session = Some(Accepted {
		session: session.clone(),
	});
	drop(published);

	// Mirror the session's estimates into the persistent ones its handles hold.
	let forward = async {
		tokio::join!(
			forward_send(session.send_bandwidth(), &send),
			forward_recv(&session, &recv)
		);
		std::future::pending::<()>().await
	};
	tokio::select! {
		_ = moq_net::time::run(driver) => {}
		_ = forward => {}
	}
	let _ = send.set(None);
	let _ = recv.set(None);
	session.closed().await
}

async fn forward_send(source: Option<moq_net::bandwidth::Consumer>, out: &moq_net::bandwidth::Producer) {
	let Some(mut source) = source else { return };
	while let Ok(rate) = source.changed().await {
		let _ = out.set(rate);
	}
}

/// Forward the receive estimate only while someone holds `out`, since holding the
/// session's consumer is what requests PROBE.
async fn forward_recv(session: &moq_net::Session, out: &moq_net::bandwidth::Producer) {
	let mut demand = out.demand();
	loop {
		loop {
			match demand.wanted_changed().await {
				Ok(true) => break,
				Ok(false) => {}
				Err(_) => return,
			}
		}
		let Some(mut source) = session.recv_bandwidth() else {
			return;
		};
		loop {
			tokio::select! {
				biased;
				wanted = demand.wanted_changed() => {
					match wanted {
						Ok(true) => continue,
						Ok(false) => {}
						Err(_) => return,
					}
					let _ = out.set(None);
					break;
				}
				rate = source.changed() => match rate {
					Ok(rate) => {
						let _ = out.set(rate);
					}
					Err(_) => return,
				},
			}
		}
	}
}
