//! Cancellation, duplex traffic, and unreliable traffic sharing the simulation's connection.

use super::*;

const CODE: VarInt = VarInt(42);
const DATAGRAMS: usize = 16;

#[derive(Debug, Clone, Copy)]
enum End {
	Finish,
	Reset,
	ReliableReset,
	Stop,
}

#[derive(Debug, Clone)]
pub(super) struct Spec {
	/// Both sides initiate these streams; the other direction always finishes normally.
	streams: Vec<(End, usize, usize)>,
	migrate: bool,
}

pub(super) fn spec() -> impl Strategy<Value = Spec> {
	(
		proptest::collection::vec((0u8..4, 2usize..=32_768, 1usize..=32_768), 1..=6),
		any::<bool>(),
	)
		.prop_map(|(streams, migrate)| Spec {
			streams: streams
				.into_iter()
				.map(|(end, len, cut)| {
					let end = [End::Finish, End::Reset, End::ReliableReset, End::Stop][end as usize];
					(end, len, cut.min(len - 1))
				})
				.collect(),
			migrate,
		})
}

struct Transfer {
	end: End,
	len: usize,
	cut: usize,
	written: usize,
	read: usize,
	sent: bool,
	received: bool,
}

impl Transfer {
	fn new(end: End, len: usize, cut: usize) -> Self {
		Self {
			end,
			len,
			cut,
			written: 0,
			read: 0,
			sent: false,
			received: false,
		}
	}

	fn send(&mut self, conn: &mut Connection, id: StreamId, key: usize) -> TestCaseResult {
		if self.sent {
			return Ok(());
		}
		let mut stream = conn.send_stream(id);
		if matches!(self.end, End::Stop)
			&& let Some(send::End::Stopped(code)) = stream.end().unwrap()
		{
			prop_assert_eq!(code, CODE);
			stream.reset(code).unwrap();
			self.sent = true;
			return Ok(());
		}
		let target = match self.end {
			End::Reset | End::ReliableReset => self.cut,
			_ => self.len,
		};
		while self.written < target {
			match stream.write(&mut &payload(key, self.written..target)[..]) {
				Ok(written) => self.written += written,
				Err(send::Error::Blocked) => return Ok(()),
				Err(send::Error::Stopped(code)) if matches!(self.end, End::Stop) => {
					prop_assert_eq!(code, CODE);
					stream.reset(code).unwrap();
					self.sent = true;
					return Ok(());
				}
				Err(error) => return Err(TestCaseError::fail(format!("{id} write: {error}"))),
			}
		}
		match self.end {
			End::Finish => stream.finish().unwrap(),
			End::Reset => stream.reset(CODE).unwrap(),
			End::ReliableReset => {
				let reliable = stream.offset().rewind((self.cut / 2).max(1) as u64);
				stream.reset_at(reliable, CODE).unwrap()
			}
			// Keep the send half open until the receiver's STOP_SENDING arrives.
			End::Stop => return Ok(()),
		}
		self.sent = true;
		Ok(())
	}

	fn receive(&mut self, conn: &mut Connection, id: StreamId, key: usize) -> TestCaseResult {
		if self.received {
			if matches!(self.end, End::Stop) {
				prop_assert!(matches!(conn.recv_stream(id).read(), Err(recv::Error::Closed)));
			}
			return Ok(());
		}
		let mut stream = conn.recv_stream(id);
		let mut chunks = stream.read().unwrap();
		loop {
			// Stop inside a chunk too, so a fully buffered stream cannot skip the stop path.
			let max = match self.end {
				End::Stop => self.cut - self.read,
				_ => usize::MAX,
			};
			match chunks.next(max) {
				Ok(Some(chunk)) => {
					let end = self.read + chunk.bytes.len();
					prop_assert!(end <= self.len);
					prop_assert_eq!(chunk.bytes.as_ref(), payload(key, self.read..end));
					self.read = end;
					if matches!(self.end, End::Stop) && self.read == self.cut {
						break;
					}
				}
				Ok(None) => {
					prop_assert!(matches!(self.end, End::Finish));
					prop_assert_eq!(self.read, self.len);
					self.received = true;
					break;
				}
				Err(recv::Error::Blocked) => break,
				Err(recv::Error::Reset(code)) => {
					prop_assert_eq!(code, CODE);
					prop_assert!(matches!(self.end, End::Reset | End::ReliableReset));
					prop_assert!(self.read <= self.cut);
					if matches!(self.end, End::ReliableReset) {
						prop_assert!(self.read >= (self.cut / 2).max(1));
					}
					self.received = true;
					break;
				}
				Err(error) => return Err(TestCaseError::fail(format!("{id} read: {error}"))),
			}
		}
		let _ = chunks.finalize();
		if matches!(self.end, End::Stop) && self.read == self.cut {
			stream.stop(CODE).unwrap();
			self.received = true;
		}
		Ok(())
	}
}

struct Duplex {
	initiator: usize,
	id: Option<StreamId>,
	accepted: bool,
	/// Indexed by sender: client, then server.
	transfers: [Transfer; 2],
}

pub(super) struct Run {
	handles: [ConnectionHandle; 2],
	streams: Vec<Duplex>,
	datagrams_sent: [usize; 2],
	datagrams_received: [BTreeSet<usize>; 2],
	migrate: bool,
	steps: usize,
}

impl Run {
	pub(super) fn new(spec: &Spec, client: ConnectionHandle, server: ConnectionHandle) -> Self {
		Self {
			handles: [client, server],
			streams: (0..2)
				.flat_map(|initiator| {
					spec.streams.iter().map(move |&(end, len, cut)| Duplex {
						initiator,
						id: None,
						accepted: false,
						transfers: std::array::from_fn(|side| {
							Transfer::new(if side == initiator { end } else { End::Finish }, len, cut)
						}),
					})
				})
				.collect(),
			datagrams_sent: [0; 2],
			datagrams_received: Default::default(),
			migrate: spec.migrate,
			steps: 0,
		}
	}

	pub(super) fn send(&mut self, pair: &mut Pair) -> TestCaseResult {
		self.steps += 1;
		if self.migrate && matches!(self.steps, 2 | 4) {
			let port = pair.next_port();
			// A port change is NAT rebinding; an IP change is active migration to another path.
			pair.client.addr = SocketAddr::new(
				if self.steps == 2 {
					Ipv6Addr::LOCALHOST
				} else {
					"::2".parse().unwrap()
				}
				.into(),
				port,
			);
			pair.client_conn_mut(self.handles[0]).ping();
		}
		for (index, duplex) in self.streams.iter_mut().enumerate() {
			if duplex.id.is_none() {
				duplex.id = conn(pair, self.handles, duplex.initiator)
					.streams()
					.open(Dir::Bi, Default::default());
			}
			let Some(id) = duplex.id else { continue };
			for side in 0..2 {
				if side == duplex.initiator || duplex.accepted {
					duplex.transfers[side].send(conn(pair, self.handles, side), id, index * 2 + side)?;
				}
			}
		}
		for side in 0..2 {
			if self.datagrams_sent[side] < DATAGRAMS {
				let index = self.datagrams_sent[side];
				conn(pair, self.handles, side)
					.datagrams()
					.send(Bytes::from(datagram(side, index)), Default::default())
					.unwrap();
				self.datagrams_sent[side] += 1;
			}
		}
		Ok(())
	}

	pub(super) fn receive(&mut self, pair: &mut Pair) -> TestCaseResult {
		for side in 0..2 {
			let conn = conn(pair, self.handles, side);
			while let Some(id) = conn.streams().accept(Dir::Bi) {
				let duplex = self.streams.iter_mut().find(|d| d.id == Some(id)).unwrap();
				prop_assert_eq!(duplex.initiator, 1 - side);
				prop_assert!(!duplex.accepted);
				duplex.accepted = true;
			}
			for (index, duplex) in self.streams.iter_mut().enumerate() {
				if let Some(id) = duplex.id
					&& duplex.accepted
				{
					duplex.transfers[1 - side].receive(conn, id, index * 2 + 1 - side)?;
				}
			}
			while let Some(bytes) = conn.datagrams().recv() {
				prop_assert_eq!(bytes.len(), 256);
				let index = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
				prop_assert!(index < self.datagrams_sent[1 - side]);
				prop_assert_eq!(bytes.as_ref(), datagram(1 - side, index));
				prop_assert!(
					self.datagrams_received[side].insert(index),
					"duplicate datagram {index}"
				);
			}
		}
		Ok(())
	}

	pub(super) fn done(&self) -> bool {
		self.steps >= 16
			&& self
				.streams
				.iter()
				.all(|d| d.transfers.iter().all(|t| t.sent && t.received))
	}

	pub(super) fn finish(&mut self, pair: &mut Pair) -> TestCaseResult {
		// The last datagram may trail the final stream byte. Drain the network before counting it.
		pair.drive();
		self.receive(pair)?;
		let lossless = [&pair.to_server, &pair.to_client]
			.iter()
			.all(|link| link.loss == 0.0 && link.bottleneck.as_ref().is_none_or(|b| b.stats.dropped == 0));
		for side in 0..2 {
			// Retired addresses can drop in-flight packets even when the links have no loss.
			if lossless && !self.migrate {
				prop_assert_eq!(self.datagrams_received[side].len(), DATAGRAMS);
			}
			let conn = conn(pair, self.handles, side);
			prop_assert_eq!(conn.streams().remote_open_streams(Dir::Bi), 0);
			prop_assert_eq!(conn.streams().send_streams(), 0);
		}
		if self.migrate {
			prop_assert_eq!(pair.server_conn_mut(self.handles[1]).remote_address(), pair.client.addr);
		}
		Ok(())
	}
}

fn conn(pair: &mut Pair, handles: [ConnectionHandle; 2], side: usize) -> &mut Connection {
	match side {
		0 => pair.client_conn_mut(handles[0]),
		_ => pair.server_conn_mut(handles[1]),
	}
}

fn datagram(side: usize, index: usize) -> Vec<u8> {
	let mut data = payload(side, 0..256);
	data[..4].copy_from_slice(&(index as u32).to_le_bytes());
	data
}

/// Every cancellation returns stream credit before the next stream can open, in both directions.
#[test]
fn cancellation_and_migration_with_one_stream_credit() {
	for migrate in [false, true] {
		let case = Case {
			seed: 1,
			latency: Duration::from_millis(10),
			to_server: Link::default(),
			to_client: Link::default(),
			limits: Limits {
				stream_window: 1_200,
				window: 4_800,
				max_streams: 1,
			},
			read_interval: Duration::from_millis(3),
			streams: vec![super::Spec {
				from_client: true,
				len: 32_768,
				priority: 0,
			}],
			extra: Some(Spec {
				streams: [End::Reset, End::ReliableReset, End::Stop, End::Finish]
					.into_iter()
					.map(|end| (end, 16_384, 8_192))
					.collect(),
				migrate,
			}),
		};
		let outcome = run(&case).unwrap();
		assert_eq!(outcome, run(&case).unwrap());
	}
}
