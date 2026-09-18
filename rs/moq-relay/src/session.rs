//! Live sessions on this node: a registry an operator can list and nudge.
//!
//! Each admitted session (QUIC, WebSocket, stream) registers the request the
//! auth server already saw. A [`Filter`] selects by any subset of those fields;
//! a push asks the lease to re-check now, and the server's reply is the verdict.
//! One-shot HTTP and LAN peers are not sessions and stay out.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use moq_auth::{Pattern, Peer, Request, Role, Transport};
use serde::{Deserialize, Serialize};
use serde_with::{TimestampSeconds, serde_as};

/// The live sessions on this node, cloned into every connection and the
/// internal listener.
#[derive(Clone, Default)]
pub struct Registry {
	inner: Arc<Mutex<HashMap<String, Entry>>>,
}

struct Entry {
	request: Request,
	started: SystemTime,
	handle: Handle,
}

/// A cloneable wake: a push increments `generation` and every waiter parked on
/// it observes the new value once, so a burst coalesces.
#[derive(Clone, Default)]
struct Handle {
	generation: Arc<AtomicU64>,
	notify: Arc<tokio::sync::Notify>,
}

impl Handle {
	fn wake(&self) {
		self.generation.fetch_add(1, Ordering::Release);
		self.notify.notify_waiters();
	}

	async fn wait(&self, seen: u64) -> u64 {
		loop {
			let notified = self.notify.notified();
			let now = self.generation.load(Ordering::Acquire);
			if now > seen {
				return now;
			}
			notified.await;
		}
	}
}

/// The guard a session holds: dropping it unregisters, and [`nudged`](Self::nudged)
/// resolves when a push matches it.
pub struct Registration {
	registry: Registry,
	id: String,
	handle: Handle,
	seen: AtomicU64,
}

impl Registration {
	/// Wait until a push has asked this session to re-check since the last observation.
	pub async fn nudged(&self) {
		let seen = self.seen.load(Ordering::Relaxed);
		let now = self.handle.wait(seen).await;
		self.seen.store(now, Ordering::Relaxed);
	}
}

impl Drop for Registration {
	fn drop(&mut self) {
		self.registry.inner.lock().unwrap().remove(&self.id);
	}
}

impl Registry {
	/// An empty registry.
	pub fn new() -> Self {
		Self::default()
	}

	/// Track an admitted session until the returned guard is dropped.
	pub fn register(&self, request: Request) -> Registration {
		let handle = Handle::default();
		let id = request.id.clone();
		self.inner.lock().unwrap().insert(
			id.clone(),
			Entry {
				request,
				started: SystemTime::now(),
				handle: handle.clone(),
			},
		);
		Registration {
			registry: self.clone(),
			id,
			handle,
			seen: AtomicU64::new(0),
		}
	}

	/// The sessions matching `filter`, as the list route returns them.
	pub fn list(&self, filter: &Filter) -> Vec<View> {
		self.inner
			.lock()
			.unwrap()
			.values()
			.filter(|entry| filter.matches(&entry.request))
			.map(|entry| View::from_session(&entry.request, entry.started))
			.collect()
	}

	/// Nudge every matching session and return their ids, in the order they
	/// currently sit in the table.
	pub fn revalidate(&self, filter: &Filter) -> Vec<String> {
		let mut ids = Vec::new();
		for (id, entry) in self.inner.lock().unwrap().iter() {
			if filter.matches(&entry.request) {
				entry.handle.wake();
				ids.push(id.clone());
			}
		}
		ids
	}
}

/// A partial request: every given field must match, and an empty filter is
/// every session on this node.
///
/// `id` and every scalar are exact. `path` is a [`Pattern`] against the dialed
/// path. `remote` is an IP or CIDR with the port dropped and IPv4-mapped IPv6
/// folded. `query` is not a field: it may carry the credential.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Filter {
	/// Session id.
	pub id: Option<String>,
	/// The relay's node name.
	pub node: Option<String>,
	/// How the session reached the relay.
	pub transport: Option<Transport>,
	/// Peer address as an IP or CIDR, port dropped.
	pub remote: Option<Remote>,
	/// The relay's local socket, exact including port.
	pub local: Option<SocketAddr>,
	/// SNI or the host the client addressed.
	pub server_name: Option<String>,
	/// Negotiated application protocol.
	pub alpn: Option<String>,
	/// Pattern matched against the dialed path.
	pub path: Option<Pattern>,
	/// SETUP role, when the client declared one.
	pub role: Option<Role>,
	/// Certificate SAN/CN/fingerprint.
	pub tls_name: Option<String>,
	/// Certificate SHA-256, hex.
	pub tls_fingerprint: Option<String>,
	/// Certificate issuer DN.
	pub tls_issuer: Option<String>,
}

/// An IP or a CIDR used to match a session's remote address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Remote {
	/// A single address; IPv4-mapped IPv6 folds to IPv4.
	Ip(IpAddr),
	/// A network; the session IP is folded the same way, then prefix-matched.
	Cidr { addr: IpAddr, prefix: u8 },
}

impl std::str::FromStr for Remote {
	type Err = String;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		if let Some((addr, prefix)) = s.split_once('/') {
			let addr: IpAddr = addr.parse().map_err(|_| format!("invalid remote CIDR: {s}"))?;
			let mut prefix: u8 = prefix.parse().map_err(|_| format!("invalid remote CIDR: {s}"))?;
			let canonical = addr.to_canonical();
			// An IPv4-mapped IPv6 network names IPv4 bits after the 96-bit mapping prefix.
			if canonical.is_ipv4() && !addr.is_ipv4() {
				if prefix < 96 {
					return Err(format!("invalid remote CIDR: {s}"));
				}
				prefix -= 96;
			}
			let max = if canonical.is_ipv4() { 32 } else { 128 };
			if prefix > max {
				return Err(format!("invalid remote CIDR: {s}"));
			}
			Ok(Self::Cidr {
				addr: canonical,
				prefix,
			})
		} else {
			let addr: IpAddr = s.parse().map_err(|_| format!("invalid remote address: {s}"))?;
			Ok(Self::Ip(addr.to_canonical()))
		}
	}
}

impl Filter {
	/// Parse the query string both routes take. An unknown field is an error
	/// naming it, never ignored; `query` is refused the same way.
	pub fn from_query(query: Option<&str>) -> Result<Self, Error> {
		let mut filter = Self::default();
		let Some(query) = query.filter(|query| !query.is_empty()) else {
			return Ok(filter);
		};
		for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
			match key.as_ref() {
				"id" => filter.id = Some(value.into_owned()),
				"node" => filter.node = Some(value.into_owned()),
				"transport" => {
					filter.transport = Some(parse_transport(&value).ok_or_else(|| Error::Invalid {
						field: "transport",
						value: value.into_owned(),
					})?);
				}
				"remote" => {
					filter.remote = Some(value.parse().map_err(|err| Error::Invalid {
						field: "remote",
						value: err,
					})?);
				}
				"local" => {
					filter.local = Some(value.parse().map_err(|_| Error::Invalid {
						field: "local",
						value: value.into_owned(),
					})?);
				}
				"server_name" => filter.server_name = Some(value.into_owned()),
				"alpn" => filter.alpn = Some(value.into_owned()),
				"path" => {
					filter.path = Some(value.parse().map_err(|_| Error::Invalid {
						field: "path",
						value: value.into_owned(),
					})?);
				}
				"role" => {
					filter.role = Some(parse_role(&value).ok_or_else(|| Error::Invalid {
						field: "role",
						value: value.into_owned(),
					})?);
				}
				"tls.name" => filter.tls_name = Some(value.into_owned()),
				"tls.fingerprint" => filter.tls_fingerprint = Some(value.into_owned()),
				"tls.issuer" => filter.tls_issuer = Some(value.into_owned()),
				other => return Err(Error::Unknown(other.to_string())),
			}
		}
		Ok(filter)
	}

	/// Whether every given field matches `request`.
	pub fn matches(&self, request: &Request) -> bool {
		if self.id.as_ref().is_some_and(|id| id != &request.id) {
			return false;
		}
		if self.node.as_ref().is_some_and(|node| node != &request.node) {
			return false;
		}
		if self.transport.is_some_and(|transport| transport != request.transport) {
			return false;
		}
		if let Some(remote) = &self.remote {
			let Some(addr) = request.remote else {
				return false;
			};
			if !remote.matches(addr.ip()) {
				return false;
			}
		}
		if self.local.is_some_and(|local| request.local != Some(local)) {
			return false;
		}
		if self
			.server_name
			.as_ref()
			.is_some_and(|name| request.server_name.as_ref() != Some(name))
		{
			return false;
		}
		if self
			.alpn
			.as_ref()
			.is_some_and(|alpn| request.alpn.as_ref() != Some(alpn))
		{
			return false;
		}
		if let Some(path) = &self.path
			&& !path.matches(request.path.trim_start_matches('/'))
		{
			return false;
		}
		if self.role.is_some_and(|role| request.role != Some(role)) {
			return false;
		}
		if !tls_matches(&self.tls_name, request.tls.as_ref().map(|tls| tls.name.as_str())) {
			return false;
		}
		if !tls_matches(
			&self.tls_fingerprint,
			request.tls.as_ref().map(|tls| tls.fingerprint.as_str()),
		) {
			return false;
		}
		if !tls_matches(&self.tls_issuer, request.tls.as_ref().map(|tls| tls.issuer.as_str())) {
			return false;
		}
		true
	}
}

impl Remote {
	fn matches(&self, ip: IpAddr) -> bool {
		let ip = ip.to_canonical();
		match self {
			Self::Ip(want) => ip == *want,
			Self::Cidr { addr, prefix } => in_cidr(ip, *addr, *prefix),
		}
	}
}

fn in_cidr(ip: IpAddr, network: IpAddr, prefix: u8) -> bool {
	match (ip.to_canonical(), network.to_canonical()) {
		(IpAddr::V4(ip), IpAddr::V4(net)) => {
			if prefix > 32 {
				return false;
			}
			let mask = if prefix == 0 { 0 } else { u32::MAX << (32 - prefix) };
			u32::from(ip) & mask == u32::from(net) & mask
		}
		(IpAddr::V6(ip), IpAddr::V6(net)) => {
			if prefix > 128 {
				return false;
			}
			let mask = if prefix == 0 { 0 } else { u128::MAX << (128 - prefix) };
			u128::from(ip) & mask == u128::from(net) & mask
		}
		_ => false,
	}
}

fn tls_matches(want: &Option<String>, have: Option<&str>) -> bool {
	match want {
		Some(want) => have == Some(want.as_str()),
		None => true,
	}
}

fn parse_transport(value: &str) -> Option<Transport> {
	Some(match value {
		"quic" => Transport::Quic,
		"iroh" => Transport::Iroh,
		"websocket" => Transport::WebSocket,
		"tcp" => Transport::Tcp,
		"unix" => Transport::Unix,
		"http" => Transport::Http,
		_ => return None,
	})
}

fn parse_role(value: &str) -> Option<Role> {
	Some(match value {
		"publisher" => Role::Publisher,
		"subscriber" => Role::Subscriber,
		_ => return None,
	})
}

/// Why a filter could not be built from a query string.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
	/// A field the routes do not filter on, including `query`.
	Unknown(String),
	/// A known field whose value did not parse.
	Invalid { field: &'static str, value: String },
}

impl std::fmt::Display for Error {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Self::Unknown(field) => write!(f, "unknown filter field: {field}"),
			Self::Invalid { field, value } => write!(f, "invalid {field}: {value}"),
		}
	}
}

impl std::error::Error for Error {}

impl IntoResponse for Error {
	fn into_response(self) -> Response {
		(StatusCode::BAD_REQUEST, self.to_string()).into_response()
	}
}

/// One live session as the list route returns it: the request the server saw,
/// minus `query`, plus when it was admitted.
#[serde_as]
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct View {
	/// Random 128-bit hex, unique per session.
	pub id: String,
	/// The operator's name for the relay.
	pub node: String,
	/// How the session reached the relay.
	pub transport: Transport,
	/// The peer's socket address.
	pub remote: Option<SocketAddr>,
	/// The relay's socket address the session arrived on.
	pub local: Option<SocketAddr>,
	/// The SNI the client presented.
	pub server_name: Option<String>,
	/// The negotiated application protocol.
	pub alpn: Option<String>,
	/// The path exactly as dialed.
	pub path: String,
	/// The direction the client declared at SETUP.
	pub role: Option<Role>,
	/// The verified client certificate, when one was presented.
	pub tls: Option<Peer>,
	/// When the session was admitted, as unix seconds.
	#[serde_as(as = "TimestampSeconds<i64>")]
	pub started: SystemTime,
}

impl View {
	fn from_session(request: &Request, started: SystemTime) -> Self {
		Self {
			id: request.id.clone(),
			node: request.node.clone(),
			transport: request.transport,
			remote: request.remote,
			local: request.local,
			server_name: request.server_name.clone(),
			alpn: request.alpn.clone(),
			path: request.path.clone(),
			role: request.role,
			tls: request.tls.clone(),
			started,
		}
	}
}

/// `GET /sessions` body.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct List {
	/// Matching sessions, `query` omitted.
	pub sessions: Vec<View>,
}

/// `POST /sessions/revalidate` body.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Nudged {
	/// Matching session ids; the re-checks run in the background.
	pub ids: Vec<String>,
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::time::Duration;

	fn request(id: &str, path: &str, remote: &str) -> Request {
		let mut request = Request::new("relay-1", Transport::Quic, path);
		request.id = id.to_owned();
		request.remote = Some(remote.parse().unwrap());
		request.query = Some("jwt=secret".into());
		request
	}

	#[test]
	fn an_empty_filter_matches_every_session() {
		let filter = Filter::from_query(None).unwrap();
		assert!(filter.matches(&request("a", "/room", "203.0.113.9:4433")));
	}

	#[test]
	fn query_and_unknown_fields_are_refused() {
		assert!(matches!(Filter::from_query(Some("query=jwt")), Err(Error::Unknown(field)) if field == "query"));
		assert!(matches!(Filter::from_query(Some("foo=bar")), Err(Error::Unknown(field)) if field == "foo"));
	}

	#[test]
	fn path_and_cidr_select_the_right_subset() {
		let a = request("a", "/demo/one", "203.0.113.9:1");
		let b = request("b", "/demo/two", "198.51.100.2:1");
		let c = request("c", "/other", "203.0.113.10:1");

		let path = Filter::from_query(Some("path=demo/**")).unwrap();
		assert!(path.matches(&a) && path.matches(&b) && !path.matches(&c));

		let cidr = Filter::from_query(Some("remote=203.0.113.0/24")).unwrap();
		assert!(cidr.matches(&a) && !cidr.matches(&b) && cidr.matches(&c));

		let mapped = request("m", "/demo/one", "[::ffff:203.0.113.9]:1");
		assert!(cidr.matches(&mapped));
		assert!(Filter::from_query(Some("remote=203.0.113.9")).unwrap().matches(&mapped));
	}

	#[test]
	fn mapped_cidr_prefix_folds_to_ipv4() {
		let mapped: Remote = "::ffff:203.0.113.0/120".parse().unwrap();
		assert_eq!(
			mapped,
			Remote::Cidr {
				addr: "203.0.113.0".parse().unwrap(),
				prefix: 24,
			}
		);
		let a = request("a", "/demo/one", "203.0.113.9:1");
		let b = request("b", "/demo/two", "198.51.100.2:1");
		let filter = Filter {
			remote: Some(mapped),
			..Filter::default()
		};
		assert!(filter.matches(&a));
		assert!(!filter.matches(&b));

		assert!("::ffff:203.0.113.0/80".parse::<Remote>().is_err());
		assert!("::ffff:203.0.113.0/129".parse::<Remote>().is_err());
	}

	#[test]
	fn list_omits_query() {
		let registry = Registry::new();
		let _reg = registry.register(request("abc", "/room", "127.0.0.1:1"));
		let list = registry.list(&Filter::default());
		assert_eq!(list.len(), 1);
		assert_eq!(list[0].id, "abc");
		let json = serde_json::to_value(&list[0]).unwrap();
		assert!(json.get("query").is_none());
	}

	#[tokio::test]
	async fn a_push_wakes_the_registration_once() {
		let registry = Registry::new();
		let request = request("abc", "/room", "127.0.0.1:1");
		let reg = registry.register(request);
		let ids = registry.revalidate(&Filter::from_query(Some("id=abc")).unwrap());
		assert_eq!(ids, ["abc"]);
		tokio::time::timeout(Duration::from_secs(1), reg.nudged())
			.await
			.expect("the registration woke");
		assert!(
			tokio::time::timeout(std::time::Duration::from_millis(20), reg.nudged())
				.await
				.is_err(),
			"a single push wakes once"
		);
	}
}
