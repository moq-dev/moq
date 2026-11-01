//! Opt-in public STUN Binding on a separate UDP socket.

use std::collections::{BTreeSet, HashMap};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::Context;
use kio::clock::Instant;
use serde::{Deserialize, Serialize};
use stun_types::attribute::XorMappedAddress;
use stun_types::message::{BINDING, Message, MessageClass, MessageHeader, MessageWriteMutSlice};
use stun_types::prelude::*;

/// Configuration for the optional public STUN Binding listener.
#[derive(usage_rs::Args, Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
#[non_exhaustive]
pub struct Config {
	/// UDP address serving STUN Binding requests, disabled when unset.
	#[usage(long = "stun-bind", name = "stun-bind", env = "MOQ_STUN_BIND", setting = "stun.bind")]
	pub bind: Option<SocketAddr>,
}

const SOURCE_RATE: u64 = 10;
const SOURCE_BURST: u64 = 20;
const GLOBAL_RATE: u64 = 1000;
const GLOBAL_BURST: u64 = 2000;
const SOURCES_MAX: usize = 4096;
const SOURCE_IDLE: Duration = Duration::from_secs(SOURCE_BURST / SOURCE_RATE);
const TOKEN: u64 = 1_000_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Drop {
	Invalid = 0,
	Source = 1,
	Global = 2,
	Capacity = 3,
	Io = 4,
}

#[derive(Default)]
struct Totals {
	replies: AtomicU64,
	io_errors: AtomicU64,
	dropped: [AtomicU64; 5],
}

#[derive(Clone, Default)]
pub(crate) struct Counters(Arc<Totals>);

impl Counters {
	fn drop(&self, reason: Drop) {
		self.0.dropped[reason as usize].fetch_add(1, Ordering::Relaxed);
	}

	pub(crate) fn render(&self, out: &mut String) {
		use std::fmt::Write;
		let _ = writeln!(out, "# HELP moq_relay_stun_replies_total STUN Binding replies sent.");
		let _ = writeln!(out, "# TYPE moq_relay_stun_replies_total counter");
		let _ = writeln!(
			out,
			"moq_relay_stun_replies_total {}",
			self.0.replies.load(Ordering::Relaxed)
		);
		let _ = writeln!(
			out,
			"# HELP moq_relay_stun_dropped_total STUN datagrams dropped without replying."
		);
		let _ = writeln!(out, "# TYPE moq_relay_stun_dropped_total counter");
		for (reason, count) in ["invalid", "source", "global", "capacity", "io"]
			.into_iter()
			.zip(&self.0.dropped)
		{
			let _ = writeln!(
				out,
				"moq_relay_stun_dropped_total{{reason=\"{}\"}} {}",
				crate::internal::Label(reason),
				count.load(Ordering::Relaxed)
			);
		}
		let _ = writeln!(
			out,
			"# HELP moq_relay_stun_io_errors_total Recoverable STUN socket I/O errors."
		);
		let _ = writeln!(out, "# TYPE moq_relay_stun_io_errors_total counter");
		let _ = writeln!(
			out,
			"moq_relay_stun_io_errors_total {}",
			self.0.io_errors.load(Ordering::Relaxed)
		);
	}
}

struct Bucket {
	credit: u64,
	updated: Instant,
}

impl Bucket {
	fn new(burst: u64, now: Instant) -> Self {
		Self {
			credit: burst * TOKEN,
			updated: now,
		}
	}

	fn take(&mut self, now: Instant, rate: u64, burst: u64) -> bool {
		// Nanotokens preserve sub-token refill without floats or clock-dependent rounding.
		let refill = now
			.saturating_duration_since(self.updated)
			.as_nanos()
			.saturating_mul(u128::from(rate));
		self.credit = (u128::from(self.credit) + refill).min(u128::from(burst * TOKEN)) as u64;
		self.updated = now;
		if self.credit < TOKEN {
			return false;
		}
		self.credit -= TOKEN;
		true
	}
}

struct Limits {
	global: Bucket,
	sources: HashMap<IpAddr, Bucket>,
	// One entry per IP, so activity cannot grow a stale-expiry queue indefinitely.
	oldest: BTreeSet<(Instant, IpAddr)>,
}

impl Limits {
	fn new(now: Instant) -> Self {
		Self {
			global: Bucket::new(GLOBAL_BURST, now),
			sources: HashMap::new(),
			oldest: BTreeSet::new(),
		}
	}

	fn allow(&mut self, source: IpAddr, now: Instant) -> Result<(), Drop> {
		if !self.global.take(now, GLOBAL_RATE, GLOBAL_BURST) {
			return Err(Drop::Global);
		}
		let result = self.source(source.to_canonical(), now);
		// An abusive source must not spend the global reply budget on its own refused packets.
		if result.is_err() {
			self.global.credit += TOKEN;
		}
		result
	}

	fn source(&mut self, source: IpAddr, now: Instant) -> Result<(), Drop> {
		if !self.sources.contains_key(&source) && self.sources.len() == SOURCES_MAX {
			let &(last, ip) = self.oldest.first().expect("a full source table has an oldest entry");
			if now.saturating_duration_since(last) < SOURCE_IDLE {
				return Err(Drop::Capacity);
			}
			self.oldest.remove(&(last, ip));
			self.sources.remove(&ip);
		}
		let bucket = self
			.sources
			.entry(source)
			.or_insert_with(|| Bucket::new(SOURCE_BURST, now));
		self.oldest.remove(&(bucket.updated, source));
		let allowed = bucket.take(now, SOURCE_RATE, SOURCE_BURST);
		self.oldest.insert((bucket.updated, source));
		if allowed { Ok(()) } else { Err(Drop::Source) }
	}
}

/// Decode through the maintained codec; UDP must contain exactly one whole message.
fn binding(data: &[u8], source: SocketAddr, output: &mut [u8; 44]) -> Option<usize> {
	let header = MessageHeader::from_bytes(data).ok()?;
	if usize::from(header.data_length()) + MessageHeader::LENGTH != data.len() {
		return None;
	}
	if header.get_type().class() != MessageClass::Request || header.get_type().method() != BINDING {
		return None;
	}
	let request = Message::from_bytes(data).ok()?;
	// The general codec may ignore attributes after an ending attribute. This
	// endpoint refuses those bytes and required extensions it does not implement.
	let mut end = MessageHeader::LENGTH;
	for (offset, attribute) in request.iter_attributes() {
		if attribute.get_type().comprehension_required() {
			return None;
		}
		end = offset + attribute.padded_len();
	}
	if end != data.len() {
		return None;
	}
	let mut reply = Message::builder_success(&request, MessageWriteMutSlice::new(output));
	let mapped = SocketAddr::new(source.ip().to_canonical(), source.port());
	reply
		.add_attribute(&XorMappedAddress::new(mapped, request.transaction_id()))
		.ok()?;
	Some(reply.finish())
}

// Unconnected UDP can report a previous peer's ICMP error on the next receive.
// It says nothing about the listener or the media relay's other connections.
fn recoverable(error: &std::io::Error) -> bool {
	matches!(
		error.kind(),
		std::io::ErrorKind::ConnectionRefused
			| std::io::ErrorKind::ConnectionReset
			| std::io::ErrorKind::HostUnreachable
			| std::io::ErrorKind::NetworkUnreachable
			| std::io::ErrorKind::Interrupted
	)
}

pub(crate) struct Server {
	socket: tokio::net::UdpSocket,
	pub(crate) counters: Counters,
}

impl Server {
	#[cfg(all(test, feature = "stun"))]
	pub(crate) fn addr(&self) -> SocketAddr {
		self.socket.local_addr().unwrap()
	}

	pub(crate) async fn bind(config: Config) -> anyhow::Result<Option<Self>> {
		let Some(bind) = config.bind else {
			return Ok(None);
		};
		let socket = tokio::net::UdpSocket::bind(bind)
			.await
			.context("failed to bind STUN listener")?;
		tracing::info!(addr = %socket.local_addr()?, kind = "stun", "listening");
		Ok(Some(Self {
			socket,
			counters: Counters::default(),
		}))
	}

	pub(crate) async fn run(self) -> anyhow::Result<()> {
		let mut limits = Limits::new(Instant::now());
		// A full UDP payload avoids treating truncation as a smaller valid request.
		let mut input = vec![0; 65536];
		let mut output = [0; 44];
		loop {
			let (len, source) = match self.socket.recv_from(&mut input).await {
				Ok(packet) => packet,
				Err(error) if recoverable(&error) => {
					self.counters.0.io_errors.fetch_add(1, Ordering::Relaxed);
					continue;
				}
				Err(error) => return Err(error).context("STUN receive failed"),
			};
			if let Err(reason) = limits.allow(source.ip(), Instant::now()) {
				self.counters.drop(reason);
				continue;
			}
			let Some(len) = binding(&input[..len], source, &mut output) else {
				self.counters.drop(Drop::Invalid);
				continue;
			};
			match self.socket.send_to(&output[..len], source).await {
				Ok(_) => {
					self.counters.0.replies.fetch_add(1, Ordering::Relaxed);
				}
				Err(error) if recoverable(&error) => {
					self.counters.0.io_errors.fetch_add(1, Ordering::Relaxed);
					self.counters.drop(Drop::Io);
				}
				Err(error) => return Err(error).context("STUN reply failed"),
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use stun_types::message::{MessageType, MessageWriteVec};

	fn request() -> Vec<u8> {
		Message::builder(
			MessageType::from_class_method(MessageClass::Request, BINDING),
			0x0102030405060708090a0b0cu128.into(),
			MessageWriteVec::new(),
		)
		.finish()
	}

	#[test]
	fn binding_reply_contains_only_the_sender_mapping_and_transaction() {
		let request = request();
		assert_eq!(request.len(), 20);
		for addr in ["192.0.2.1:3478", "[2001:db8::1]:4567", "[::ffff:192.0.2.1]:3478"] {
			let source: SocketAddr = addr.parse().unwrap();
			let mut output = [0; 44];
			let len = binding(&request, source, &mut output).unwrap();
			let reply = Message::from_bytes(&output[..len]).unwrap();
			assert_eq!(reply.class(), MessageClass::Success);
			assert_eq!(
				reply.transaction_id(),
				Message::from_bytes(&request).unwrap().transaction_id()
			);
			assert_eq!(reply.iter_attributes().count(), 1);
			assert_eq!(
				reply
					.attribute::<XorMappedAddress>()
					.unwrap()
					.addr(reply.transaction_id()),
				SocketAddr::new(source.ip().to_canonical(), source.port())
			);
			assert_eq!(len, if source.ip().to_canonical().is_ipv4() { 32 } else { 44 });
		}
		let mut output = [0; 44];
		binding(&request, "192.0.2.1:3478".parse().unwrap(), &mut output).unwrap();
		// Independent RFC 8489 IPv4 XOR address bytes, without relying on the decoder.
		assert_eq!(
			&output[20..32],
			&[0, 0x20, 0, 8, 0, 1, 0x2c, 0x84, 0xe1, 0x12, 0xa6, 0x43]
		);
	}

	#[test]
	fn malformed_non_binding_and_required_extensions_get_no_reply() {
		let source = "192.0.2.1:3478".parse().unwrap();
		let mut input = vec![vec![], vec![0; 20], request()[..19].to_vec()];
		let mut trailing = request();
		trailing.push(0);
		input.push(trailing);
		for class in [MessageClass::Success, MessageClass::Indication, MessageClass::Error] {
			input.push(
				Message::builder(
					MessageType::from_class_method(class, BINDING),
					1.into(),
					MessageWriteVec::new(),
				)
				.finish(),
			);
		}
		let mut method = request();
		method[1] = 3;
		input.push(method);
		let mut broken_attr = request();
		broken_attr[3] = 4;
		broken_attr.extend_from_slice(&[0x80, 0x22, 0, 16]);
		input.push(broken_attr);
		let mut required = request();
		required[3] = 4;
		required.extend_from_slice(&[0, 3, 0, 0]);
		input.push(required);
		for data in input {
			assert!(binding(&data, source, &mut [0; 44]).is_none(), "{data:?}");
		}
	}

	#[test]
	fn fingerprint_is_validated_and_must_end_the_request() {
		let source = "192.0.2.1:3478".parse().unwrap();
		let mut writer = Message::builder_request(BINDING, MessageWriteVec::new());
		writer.add_fingerprint().unwrap();
		let valid = writer.finish();
		assert!(binding(&valid, source, &mut [0; 44]).is_some());
		let mut invalid = valid.clone();
		*invalid.last_mut().unwrap() ^= 1;
		assert!(binding(&invalid, source, &mut [0; 44]).is_none());
		let mut extra = valid;
		extra[3] += 4;
		extra.extend_from_slice(&[0x80, 0x22, 0, 0]);
		assert!(binding(&extra, source, &mut [0; 44]).is_none());
	}

	#[tokio::test(start_paused = true)]
	async fn source_bucket_refills_fractional_tokens_with_mock_time() {
		let mut limits = Limits::new(Instant::now());
		let source = "192.0.2.1".parse().unwrap();
		for _ in 0..SOURCE_BURST {
			limits.allow(source, Instant::now()).unwrap();
		}
		assert_eq!(limits.allow(source, Instant::now()), Err(Drop::Source));
		tokio::time::advance(Duration::from_millis(99)).await;
		assert_eq!(limits.allow(source, Instant::now()), Err(Drop::Source));
		tokio::time::advance(Duration::from_millis(1)).await;
		limits.allow(source, Instant::now()).unwrap();
		assert_eq!(limits.allow(source, Instant::now()), Err(Drop::Source));
		// An IPv4-mapped representation is the same source IP, regardless of port/family.
		assert_eq!(
			limits.allow("::ffff:192.0.2.1".parse().unwrap(), Instant::now()),
			Err(Drop::Source)
		);
		assert_eq!(limits.sources.len(), 1);
	}

	#[tokio::test(start_paused = true)]
	async fn global_budget_bounds_distributed_sources_and_refills() {
		let mut limits = Limits::new(Instant::now());
		for ip in 0..GLOBAL_BURST {
			limits.allow(IpAddr::V4((ip as u32).into()), Instant::now()).unwrap();
		}
		assert_eq!(
			limits.allow("192.0.2.1".parse().unwrap(), Instant::now()),
			Err(Drop::Global)
		);
		tokio::time::advance(Duration::from_millis(1)).await;
		limits.allow("192.0.2.1".parse().unwrap(), Instant::now()).unwrap();
		assert_eq!(
			limits.allow("192.0.2.2".parse().unwrap(), Instant::now()),
			Err(Drop::Global)
		);
	}

	#[tokio::test(start_paused = true)]
	async fn source_drops_do_not_spend_another_sources_global_budget() {
		let mut limits = Limits::new(Instant::now());
		let ip = "192.0.2.1".parse().unwrap();
		for _ in 0..SOURCE_BURST {
			limits.allow(ip, Instant::now()).unwrap();
		}
		for _ in 0..GLOBAL_BURST {
			assert_eq!(limits.allow(ip, Instant::now()), Err(Drop::Source));
		}
		assert_eq!(limits.global.credit, (GLOBAL_BURST - SOURCE_BURST) * TOKEN);
		limits.allow("192.0.2.2".parse().unwrap(), Instant::now()).unwrap();
	}

	#[tokio::test(start_paused = true)]
	async fn source_table_refuses_growth_until_an_old_entry_has_fully_refilled() {
		let mut limits = Limits::new(Instant::now());
		for ip in 0..SOURCES_MAX {
			limits.source(IpAddr::V4((ip as u32).into()), Instant::now()).unwrap();
		}
		assert_eq!(
			limits.source("192.0.2.1".parse().unwrap(), Instant::now()),
			Err(Drop::Capacity)
		);
		tokio::time::advance(SOURCE_IDLE).await;
		limits.source("192.0.2.1".parse().unwrap(), Instant::now()).unwrap();
		assert_eq!(limits.sources.len(), SOURCES_MAX);
		assert_eq!(limits.oldest.len(), SOURCES_MAX);
	}

	#[tokio::test]
	async fn udp_listener_replies_and_disabled_listener_binds_nothing() {
		assert!(Server::bind(Config::default()).await.unwrap().is_none());
		let server = Server::bind(Config {
			bind: Some("127.0.0.1:0".parse().unwrap()),
		})
		.await
		.unwrap()
		.unwrap();
		let addr = server.socket.local_addr().unwrap();
		let counters = server.counters.clone();
		let serving = tokio::spawn(server.run());
		let client = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
		client.send_to(&request(), addr).await.unwrap();
		let mut output = [0; 44];
		let (len, sender) = kio::clock::timeout(Duration::from_secs(5), client.recv_from(&mut output))
			.await
			.unwrap()
			.unwrap();
		assert_eq!(sender, addr);
		let reply = Message::from_bytes(&output[..len]).unwrap();
		assert_eq!(
			reply
				.attribute::<XorMappedAddress>()
				.unwrap()
				.addr(reply.transaction_id()),
			client.local_addr().unwrap()
		);
		assert_eq!(counters.0.replies.load(Ordering::Relaxed), 1);
		serving.abort();
		assert!(serving.await.unwrap_err().is_cancelled());
	}
	#[test]
	fn peer_icmp_errors_do_not_classify_the_listener_as_failed() {
		use std::io::{Error, ErrorKind};
		for kind in [
			ErrorKind::ConnectionRefused,
			ErrorKind::ConnectionReset,
			ErrorKind::HostUnreachable,
			ErrorKind::NetworkUnreachable,
			ErrorKind::Interrupted,
		] {
			assert!(recoverable(&Error::from(kind)));
		}
		for kind in [
			ErrorKind::PermissionDenied,
			ErrorKind::InvalidInput,
			ErrorKind::OutOfMemory,
		] {
			assert!(!recoverable(&Error::from(kind)));
		}
	}

	#[test]
	fn drop_metrics_preserve_reason_counts_without_source_labels() {
		let counters = Counters::default();
		for reason in [Drop::Invalid, Drop::Source, Drop::Global, Drop::Capacity, Drop::Invalid] {
			counters.drop(reason);
		}
		let mut output = String::new();
		counters.render(&mut output);
		assert!(output.contains("moq_relay_stun_replies_total 0"));
		assert!(output.contains("reason=\"invalid\"} 2"));
		for reason in ["source", "global", "capacity"] {
			assert!(output.contains(&format!("reason=\"{reason}\"}} 1")));
		}
		assert!(!output.contains("192.0.2"));
	}

	#[test]
	#[ignore = "benchmark"]
	#[expect(
		clippy::disallowed_methods,
		clippy::disallowed_types,
		reason = "the benchmark measures real per-request cost"
	)]
	fn benchmark_stun_sources() {
		println!("sources,requests_per_source,ns_per_request");
		for sources in [1u32, 64, SOURCES_MAX as u32] {
			for requests in [1u32, 20, 100] {
				let now = Instant::now();
				let mut limits = Limits::new(now);
				for ip in 0..sources {
					limits.source(IpAddr::V4(ip.into()), now).unwrap();
				}
				let started = std::time::Instant::now();
				for request in 0..requests {
					let now = now + Duration::from_millis(100 * u64::from(request + 1));
					for ip in 0..sources {
						limits.source(std::hint::black_box(IpAddr::V4(ip.into())), now).unwrap();
					}
				}
				println!(
					"{sources},{requests},{}",
					started.elapsed().as_nanos() / (u128::from(sources) * u128::from(requests))
				);
				assert_eq!(limits.oldest.len(), sources as usize);
				assert_eq!(limits.sources.len(), sources as usize);
			}
		}
	}
}
