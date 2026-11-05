//! QMux record I/O over WebSocket: one Binary message per record.

use std::future::Future;
use std::pin::Pin;
use std::task::{Poll, ready};
use std::time::Duration;

use bytes::Bytes;
use futures::{Sink, Stream};
use kio::clock::{Interval, MissedTickBehavior, Sleep};
use tokio_tungstenite::tungstenite;

use super::Transport;
use crate::ws::KeepAlive;
use crate::{Config, Error, MAX_FRAME_SIZE};

type Message = tungstenite::Message;

/// The `Stream + Sink` bound a WebSocket needs to carry a session.
pub trait WsStream:
	Stream<Item = Result<Message, tungstenite::Error>> + Sink<Message, Error = tungstenite::Error> + Unpin
{
}
impl<T> WsStream for T where
	T: Stream<Item = Result<Message, tungstenite::Error>> + Sink<Message, Error = tungstenite::Error> + Unpin
{
}

/// A WebSocket carrying a session, with its keep-alive.
pub struct WsTransport<T> {
	ws: T,
	keep_alive: Option<KeepAlive>,
	/// Sends a Ping on each tick, once bound.
	ping: Option<Interval>,
	/// A Ping is due but the socket had no room for it yet.
	ping_due: bool,
	/// Reset by every message; the peer is gone once it elapses.
	deadline: Option<(Sleep, Duration)>,
	/// The largest message the session accepts.
	recv_limit: usize,
	socket_stats: Option<crate::SharedSocketStats>,
	address: moq_net::transport::Address,
}

impl<T: WsStream> WsTransport<T> {
	pub(crate) fn new(ws: T, address: moq_net::transport::Address) -> Self {
		Self {
			ws,
			keep_alive: None,
			ping: None,
			ping_due: false,
			deadline: None,
			recv_limit: MAX_FRAME_SIZE,
			socket_stats: None,
			address,
		}
	}

	/// Refuses a keep-alive that would close a healthy idle peer: a zero interval, or a
	/// timeout no longer than the interval, which expires before the first Pong.
	pub(crate) fn with_keep_alive(mut self, keep_alive: KeepAlive) -> Result<Self, Error> {
		if keep_alive.interval.is_zero() {
			return Err(Error::InvalidConfig("zero keep-alive interval"));
		}
		if keep_alive.timeout <= keep_alive.interval {
			return Err(Error::InvalidConfig("keep-alive timeout not above its interval"));
		}
		self.keep_alive = Some(keep_alive);
		Ok(self)
	}

	pub(crate) fn with_socket_stats(mut self, stats: crate::SharedSocketStats) -> Self {
		self.socket_stats = Some(stats);
		self
	}

	/// Queue a keep-alive Ping once one is due and the socket has room; the driver's next flush
	/// sends it.
	fn poll_ping(&mut self, waiter: &kio::Waiter) -> Result<(), Error> {
		let Some(ping) = &mut self.ping else {
			return Ok(());
		};
		let mut cx = waiter.context();
		while ping.poll_tick(&mut cx).is_ready() {
			self.ping_due = true;
		}
		if self.ping_due && Pin::new(&mut self.ws).poll_ready(&mut cx).is_ready() {
			Pin::new(&mut self.ws)
				.start_send(Message::Ping(Bytes::new()))
				.map_err(|_| Error::Closed)?;
			self.ping_due = false;
		}
		Ok(())
	}
}

impl<T: WsStream> Transport for WsTransport<T> {
	fn bind(&mut self, config: &Config) {
		self.recv_limit = match config.version.uses_records() {
			true => usize::try_from(config.max_record_size).unwrap_or(usize::MAX),
			false => MAX_FRAME_SIZE,
		};
		if let Some(keep_alive) = self.keep_alive {
			// The first tick is one interval out, so we don't ping on connect. A long pause
			// sends one Ping, not a catch-up burst.
			let mut ping = kio::clock::interval_at(kio::clock::now() + keep_alive.interval, keep_alive.interval);
			ping.set_missed_tick_behavior(MissedTickBehavior::Delay);
			self.ping = Some(ping);
			self.deadline = Some((kio::clock::sleep(keep_alive.timeout), keep_alive.timeout));
		}
	}

	fn poll_recv(&mut self, waiter: &kio::Waiter) -> Poll<Result<Bytes, Error>> {
		self.poll_ping(waiter)?;

		let mut cx = waiter.context();
		// Prefer a message over the deadline: if both are ready, the peer is alive.
		while let Poll::Ready(message) = Pin::new(&mut self.ws).poll_next(&mut cx) {
			// Any message, data or control, proves the peer is alive.
			if let Some((sleep, timeout)) = &mut self.deadline {
				sleep.reset(kio::clock::now() + *timeout);
			}
			let message = match message {
				Some(Ok(message)) => message,
				// Keep the WebSocket error, so callers can still tell a protocol fault apart.
				Some(Err(err)) => return Poll::Ready(Err(err.into())),
				None => return Poll::Ready(Err(Error::Closed)),
			};
			match message {
				// The binding makes an empty message a no-op, unlike an empty record.
				Message::Binary(data) if data.is_empty() => {}
				Message::Binary(data) if data.len() > self.recv_limit => return Poll::Ready(Err(Error::FrameTooLarge)),
				Message::Binary(data) => return Poll::Ready(Ok(data)),
				Message::Close(_) => return Poll::Ready(Err(Error::Closed)),
				// QMux rides binary messages only; text means the peer speaks something else.
				Message::Text(_) => return Poll::Ready(Err(Error::ProtocolViolation)),
				// tungstenite queues a Pong reply when it reads a Ping; the next flush sends it.
				Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {}
			}
		}

		if let Some((sleep, _)) = &mut self.deadline {
			ready!(Pin::new(sleep).poll(&mut cx));
			tracing::debug!("websocket keep_alive timeout");
			return Poll::Ready(Err(Error::Closed));
		}
		Poll::Pending
	}

	fn poll_ready(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		Pin::new(&mut self.ws)
			.poll_ready(&mut waiter.context())
			.map_err(|_| Error::Closed)
	}

	fn send(&mut self, record: Bytes) -> Result<(), Error> {
		// Still one message per record, since the message boundary is the record boundary, but
		// tungstenite only appends it to its write buffer until the flush.
		Pin::new(&mut self.ws)
			.start_send(Message::Binary(record))
			.map_err(|_| Error::Closed)
	}

	fn poll_flush(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		Pin::new(&mut self.ws)
			.poll_flush(&mut waiter.context())
			.map_err(|_| Error::Closed)
	}

	fn poll_close(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		Pin::new(&mut self.ws)
			.poll_close(&mut waiter.context())
			.map_err(|_| Error::Closed)
	}

	fn socket_stats(&self) -> Option<crate::SharedSocketStats> {
		self.socket_stats.clone()
	}

	fn address(&self) -> moq_net::transport::Address {
		self.address
	}
}

#[cfg(test)]
mod tests;
