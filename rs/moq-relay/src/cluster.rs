use std::{
	collections::HashMap,
	path::PathBuf,
	sync::{
		Arc, Mutex, PoisonError,
		atomic::{AtomicU64, Ordering},
	},
	time::Duration,
};

use anyhow::Context;
use futures::FutureExt as _;
use moq_net::origin;
use moq_net::{Hop, Path, stats::Tier};
use reqwest_middleware::ClientWithMiddleware;
use std::task::Poll;

use crate::critical::Child;
use tracing::Instrument as _;
use url::Url;

use crate::auth;

/// Dynamic cluster peer-list configuration.
pub mod api;

/// The request path prefix a LAN mesh dial presents, marking it as a peer rather
/// than an ordinary publisher or viewer on the same listener.
pub(crate) const CLUSTER_PATH: &str = "/.cluster";

/// How often the relay re-checks an http(s) `--cluster-api-url` endpoint. The
/// HTTP cache middleware suppresses the actual network round-trip while the cached
/// list is still fresh (per the response's `Cache-Control`), so this is the floor
/// on responsiveness, not on origin load: a tighter `max-age` means more of these
/// ticks turn into real conditional GETs.
const CONNECT_API_POLL_INTERVAL: Duration = Duration::from_secs(30);

/// One cluster peer to dial, as listed in [`Config::connect`] or returned
/// by an `api.url` endpoint.
///
/// Accepts a URL string or an object with `url` plus
/// policy: `cost` prices the link, `egress` declares this relay's own price to
/// the peer, `token` carries the peer's credential, and `upstream` marks the
/// link upstream, so it is never offered a route learned on another upstream
/// link. `egress` defaults to `cost`; a different effective value is rejected
/// until asymmetric routing lands. An object `token` behaves exactly like an inline `?jwt=` and is
/// redacted the same way. Unknown object fields are rejected, as is an object
/// that sets policy while its `url` still carries `?cost=` or `?jwt=`.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Peer {
	url: String,
	cost: Option<u64>,
	egress: Option<u64>,
	token: Option<String>,
	upstream: bool,
}

impl std::fmt::Debug for Peer {
	/// Redact credentials: [`crate::Config::load`] traces the whole resolved config,
	/// so a derived `Debug` would print cluster credentials into logs.
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		let url = peer_url(&self.url).ok();
		f.debug_struct("Peer")
			.field("url", &url.as_ref().map(moq_tokio::RedactedUrl::new))
			.field("cost", &self.cost)
			.field("egress", &self.egress)
			.field("token", &self.token.as_ref().map(|_| "..."))
			.field("upstream", &self.upstream)
			.finish()
	}
}

impl Peer {
	/// Dial this URL, with no policy.
	pub fn new(url: impl Into<String>) -> Self {
		Self {
			url: url.into(),
			cost: None,
			egress: None,
			token: None,
			upstream: false,
		}
	}

	/// The peer address, as configured. May carry `?cost=` / `?jwt=` in the
	/// legacy string form.
	pub fn url(&self) -> &str {
		&self.url
	}

	/// What this relay charges to pull from the peer. None prices the link at 1.
	pub fn cost(&self) -> Option<u64> {
		self.cost
	}

	/// What this relay declares in SETUP as its own price toward the peer.
	/// Defaults to [`Self::cost`]; anything else is rejected for now.
	pub fn egress(&self) -> Option<u64> {
		self.egress
	}

	/// The peer's credential, replacing an inline `?jwt=`.
	pub fn token(&self) -> Option<&str> {
		self.token.as_deref()
	}

	/// Whether the link is upstream: it is never offered a route learned on
	/// another upstream link.
	pub fn upstream(&self) -> bool {
		self.upstream
	}

	/// Price the link this peer is dialed on.
	pub fn with_cost(mut self, cost: u64) -> Self {
		self.cost = Some(cost);
		self
	}

	/// Declare this relay's own price toward the peer. Must match the cost.
	pub fn with_egress(mut self, egress: u64) -> Self {
		self.egress = Some(egress);
		self
	}

	/// Present this credential instead of an inline `?jwt=`.
	pub fn with_token(mut self, token: impl Into<String>) -> Self {
		self.token = Some(token.into());
		self
	}

	/// Mark the link upstream.
	pub fn with_upstream(mut self, upstream: bool) -> Self {
		self.upstream = upstream;
		self
	}
}

impl std::str::FromStr for Peer {
	type Err = anyhow::Error;

	/// Parse a bare peer URL, preserving CLI input. Fails on an invalid URL
	/// without echoing any inline credential.
	fn from_str(s: &str) -> Result<Self, Self::Err> {
		peer_url(s)?;
		Ok(Self::new(s))
	}
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PeerObject {
	url: String,
	#[serde(default)]
	cost: Option<u64>,
	#[serde(default)]
	egress: Option<u64>,
	#[serde(default)]
	token: Option<String>,
	#[serde(default)]
	upstream: bool,
}

/// A bare string is the URL form; a map is the object form. Dispatching on the
/// shape ourselves (rather than an untagged enum) keeps serde's real error, so a
/// typo'd field names itself instead of "did not match any variant".
impl<'de> serde::Deserialize<'de> for Peer {
	fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
	where
		D: serde::Deserializer<'de>,
	{
		struct Visitor;

		impl<'de> serde::de::Visitor<'de> for Visitor {
			type Value = Peer;

			fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
				f.write_str("a peer URL string or an object with url, cost, egress, token, upstream")
			}

			fn visit_str<E: serde::de::Error>(self, url: &str) -> Result<Peer, E> {
				Ok(Peer::new(url))
			}

			fn visit_map<A: serde::de::MapAccess<'de>>(self, map: A) -> Result<Peer, A::Error> {
				let object: PeerObject =
					serde::Deserialize::deserialize(serde::de::value::MapAccessDeserializer::new(map))?;
				Ok(Peer {
					url: object.url,
					cost: object.cost,
					egress: object.egress,
					token: object.token,
					upstream: object.upstream,
				})
			}
		}

		deserializer.deserialize_any(Visitor)
	}
}

impl serde::Serialize for Peer {
	fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: serde::Serializer,
	{
		if self.cost.is_none() && self.egress.is_none() && self.token.is_none() && !self.upstream {
			return serializer.serialize_str(&self.url);
		}
		use serde::ser::SerializeStruct as _;
		let mut len = 1;
		if self.cost.is_some() {
			len += 1;
		}
		if self.egress.is_some() {
			len += 1;
		}
		if self.token.is_some() {
			len += 1;
		}
		if self.upstream {
			len += 1;
		}
		let mut state = serializer.serialize_struct("Peer", len)?;
		state.serialize_field("url", &self.url)?;
		if let Some(cost) = &self.cost {
			state.serialize_field("cost", cost)?;
		}
		if let Some(egress) = &self.egress {
			state.serialize_field("egress", egress)?;
		}
		if let Some(token) = &self.token {
			state.serialize_field("token", token)?;
		}
		if self.upstream {
			state.serialize_field("upstream", &true)?;
		}
		state.end()
	}
}

/// One peer's stable identity and the dial-affecting configuration currently
/// supplied by a discovery source.
#[derive(Clone, Debug, PartialEq, Eq)]
struct DialTarget {
	key: String,
	url: Url,
	/// Extra candidates after [`Self::url`], in dial order. Empty for a
	/// static or API peer, which has one address.
	urls: Vec<Url>,
	cost: Option<u64>,
	/// Advertised certificate fingerprint to pin. LAN only.
	fingerprint: Option<String>,
	/// Present the mDNS credential on `/.cluster/<credential>` and skip
	/// [`Config::token`]. LAN only.
	lan: bool,
	/// Never offer this peer a route learned on another upstream link.
	upstream: bool,
}

impl DialTarget {
	/// Parse a bare peer URL, keeping its inline `?cost=` / `?jwt=`.
	#[cfg(test)]
	fn parse(peer: &str) -> anyhow::Result<Self> {
		Self::from_peer(&Peer::new(peer))
	}

	/// Normalize a configured [`Peer`] into dial state. A bare URL keeps its
	/// inline `?cost=` / `?jwt=`; an object carries policy in its fields instead.
	/// Mixing the two (object policy plus URL params) is rejected rather than
	/// given a precedence a migration could silently get wrong. `egress`
	/// defaults to `cost`; anything else is rejected, never ignored.
	fn from_peer(peer: &Peer) -> anyhow::Result<Self> {
		let mut url = peer_url(&peer.url)?;
		let has_cost = url.query_pairs().any(|(key, _)| key == "cost");
		let has_jwt = url.query_pairs().any(|(key, value)| key == "jwt" && !value.is_empty());
		if peer.cost.is_some() || peer.egress.is_some() {
			anyhow::ensure!(
				!has_cost,
				"cluster peer sets cost/egress alongside a URL ?cost=; use one or the other"
			);
		}
		let token = peer.token.as_deref().filter(|token| !token.is_empty());
		if token.is_some() {
			anyhow::ensure!(
				!has_jwt,
				"cluster peer sets token alongside a URL ?jwt=; use one or the other"
			);
		}
		let cost = peer.cost.or(take_cost(&mut url)?);
		// An unpriced link costs 1 (see `moq_net::Client::with_cost`), so `egress`
		// alone is symmetric only at that price.
		let charged = cost.unwrap_or(1);
		anyhow::ensure!(
			peer.egress.unwrap_or(charged) == charged,
			"cluster peer sets egress different from cost; asymmetric costs are not supported yet"
		);
		if let Some(token) = token {
			url.query_pairs_mut().append_pair("jwt", token);
		}
		let key = {
			let mut identity = url.clone();
			identity.set_query(None);
			identity.into()
		};
		Ok(Self {
			key,
			url,
			urls: Vec::new(),
			cost,
			fingerprint: None,
			lan: false,
			upstream: peer.upstream,
		})
	}

	/// Every address to try, [`Self::url`] first.
	fn addrs(&self) -> Vec<Url> {
		if self.urls.is_empty() {
			vec![self.url.clone()]
		} else {
			self.urls.clone()
		}
	}

	/// A LAN peer's advertised addresses, each carrying that peer's credential.
	#[cfg(feature = "cluster-lan")]
	fn from_lan_peer(peer: &moq_tokio::mdns::Peer) -> anyhow::Result<Self> {
		let mut urls = peer.urls();
		anyhow::ensure!(!urls.is_empty(), "peer advertised no reachable address");
		let mut cost = None;
		for url in &mut urls {
			if cost.is_none() {
				cost = take_cost(url)?;
			} else {
				let _ = take_cost(url)?;
			}
			strip_jwt(url);
			url.set_path(&format!("{CLUSTER_PATH}/{}", peer.credential));
		}
		Ok(Self {
			key: canonicalize_peer_key(&peer.id),
			url: urls[0].clone(),
			urls,
			cost,
			fingerprint: peer.fingerprint.clone(),
			lan: true,
			upstream: false,
		})
	}
}

/// Parse a complete dynamic peer list before mutating the live dial set. A bad
/// entry or conflicting duplicate rejects the whole update, preserving the
/// last-known-good topology.
fn parse_peer_list(list: Vec<Peer>, node: Option<&str>) -> anyhow::Result<HashMap<String, DialTarget>> {
	let self_key = node.map(canonicalize_peer_key);
	let mut desired = HashMap::new();
	for (index, peer) in list.into_iter().enumerate() {
		let target = DialTarget::from_peer(&peer).with_context(|| format!("invalid peer at index {index}"))?;
		if Some(&target.key) == self_key.as_ref() {
			continue;
		}
		if let Some(previous) = desired.insert(target.key.clone(), target.clone())
			&& previous != target
		{
			anyhow::bail!("peer identity {} has conflicting configurations", target.key);
		}
	}
	Ok(desired)
}

/// A mechanism that wants a dial kept alive. A single peer can be wanted by more
/// than one at once (e.g. found on the LAN *and* listed by `--cluster-api-url`), so
/// [`DialEntry`] tracks a set of these and only tears the dial down when the last
/// one releases it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DialSource {
	/// Seeded from `--cluster-connect`. Never released, so the dial retries forever
	/// (operator intent says "always dial").
	Static,
	/// Supplied by `--cluster-api-url`. Released when a fetched peer list no
	/// longer contains the peer.
	Api,
	/// Discovered on the LAN via mDNS (`--cluster-lan`). Released as soon as the
	/// advertisement goes away.
	#[cfg(feature = "cluster-lan")]
	Mdns,
}

/// The set of [`DialSource`]s currently keeping a dial alive.
#[derive(Clone, Default)]
struct DialSources {
	seeded: Option<DialTarget>,
	api: Option<DialTarget>,
	#[cfg(feature = "cluster-lan")]
	mdns: Option<DialTarget>,
}

impl DialSources {
	fn get(&self, source: DialSource) -> Option<&DialTarget> {
		match source {
			DialSource::Static => self.seeded.as_ref(),
			DialSource::Api => self.api.as_ref(),
			#[cfg(feature = "cluster-lan")]
			DialSource::Mdns => self.mdns.as_ref(),
		}
	}

	fn set(&mut self, source: DialSource, target: DialTarget) {
		match source {
			DialSource::Static => self.seeded = Some(target),
			DialSource::Api => self.api = Some(target),
			#[cfg(feature = "cluster-lan")]
			DialSource::Mdns => self.mdns = Some(target),
		}
	}

	fn clear(&mut self, source: DialSource) {
		match source {
			DialSource::Static => self.seeded = None,
			DialSource::Api => self.api = None,
			#[cfg(feature = "cluster-lan")]
			DialSource::Mdns => self.mdns = None,
		}
	}

	/// The source to fall back to when the active one is released, most durable
	/// first: operator intent, then the API list, then LAN discovery, whose
	/// targets come and go on their own.
	fn fallback(&self) -> Option<(DialSource, &DialTarget)> {
		let fallback = self
			.seeded
			.as_ref()
			.map(|target| (DialSource::Static, target))
			.or_else(|| self.api.as_ref().map(|target| (DialSource::Api, target)));
		#[cfg(feature = "cluster-lan")]
		let fallback = fallback.or_else(|| self.mdns.as_ref().map(|target| (DialSource::Mdns, target)));
		fallback
	}
}

/// One entry in [`DialMap`].
struct DialEntry {
	/// The live dial, or a finished one's stale key, which names nothing.
	task: kio::TaskKey,
	sources: DialSources,
	active: DialSource,
}

/// The in-flight cluster dials, keyed by canonical peer identity, and the tasks
/// running them. A replaced or released dial's task is removed, which cancels it,
/// and dropping the map cancels them all.
///
/// Generic over the dial task so tests can stand in a placeholder.
struct DialMap<T> {
	entries: HashMap<String, DialEntry>,
	tasks: kio::Tasks<T>,
}

impl<T> Default for DialMap<T> {
	fn default() -> Self {
		Self {
			entries: HashMap::new(),
			tasks: kio::Tasks::new(),
		}
	}
}

impl<T> DialMap<T> {
	/// True if `peer` is already being dialed.
	fn contains(&self, peer: &str) -> bool {
		self.entries.contains_key(peer)
	}

	/// Add or update one source's target, replacing the live dial only when that
	/// source already owns it. Other sources retain their latest target as a
	/// fallback without changing the first source's active configuration. `dial`
	/// builds the task for a target that needs one.
	fn upsert(&mut self, target: DialTarget, source: DialSource, dial: &mut impl FnMut(DialTarget) -> T) {
		if let Some(entry) = self.entries.get_mut(&target.key) {
			let replace = entry.active == source && entry.sources.get(source) != Some(&target);
			entry.sources.set(source, target.clone());
			if replace {
				self.tasks.remove(entry.task);
				entry.task = self.tasks.insert(dial(target));
			}
			return;
		}

		let key = target.key.clone();
		let task = self.tasks.insert(dial(target.clone()));
		let mut sources = DialSources::default();
		sources.set(source, target);
		self.entries.insert(
			key,
			DialEntry {
				task,
				sources,
				active: source,
			},
		);
	}

	/// Release one source. If it owned the live dial, switch to a remaining
	/// source's latest target or abandon the peer when no source remains.
	fn release(&mut self, peer: &str, source: DialSource, dial: &mut impl FnMut(DialTarget) -> T) {
		let Some(entry) = self.entries.get_mut(peer) else {
			return;
		};
		let Some(active_target) = entry.sources.get(source).cloned() else {
			return;
		};
		entry.sources.clear(source);
		if entry.active != source {
			return;
		}

		if let Some((next_source, next_target)) = entry.sources.fallback() {
			let next_target = next_target.clone();
			entry.active = next_source;
			if active_target != next_target {
				self.tasks.remove(entry.task);
				entry.task = self.tasks.insert(dial(next_target));
			}
			return;
		}

		if let Some(entry) = self.entries.remove(peer) {
			self.tasks.remove(entry.task);
		}
	}

	/// Reconcile the API source against `desired`, including changes to a peer's
	/// dial-affecting URL or link cost while its canonical identity stays fixed.
	fn reconcile_api(&mut self, desired: &HashMap<String, DialTarget>, dial: &mut impl FnMut(DialTarget) -> T) {
		for target in desired.values() {
			self.upsert(target.clone(), DialSource::Api, dial);
		}

		let removed: Vec<String> = self
			.entries
			.iter()
			.filter(|(peer, entry)| entry.sources.api.is_some() && !desired.contains_key(*peer))
			.map(|(peer, _)| peer.clone())
			.collect();
		for peer in removed {
			self.release(&peer, DialSource::Api, dial);
		}
	}
}

/// A change to the dial set from a peer source running beside the dials, which
/// [`Mesh`] applies on its own poll: the sources cannot reach the [`DialMap`]
/// whose task set is polling them.
enum DialCommand {
	/// The latest valid `--cluster-api-url` peer list.
	Api(HashMap<String, DialTarget>),
	/// A LAN peer this node should dial appeared or changed.
	#[cfg(feature = "cluster-lan")]
	Found(DialTarget),
	/// A LAN peer's advertisement went away, by canonical key.
	#[cfg(feature = "cluster-lan")]
	Lost(String),
}

/// Where the peer sources queue their [`DialCommand`]s.
type DialCommands = Arc<kio::Queue<DialCommand>>;

/// Every dial task and QUIC client the cluster dials peers with, so
/// [`Cluster::close`] can end each link and send the peer its close before the
/// runtime stops: a dropped QUIC link only queues the close, and the peer then
/// keeps the link and its routes until its idle timeout.
#[derive(Clone)]
struct Dialers {
	state: Arc<Mutex<DialersPhase>>,
	/// Set on close. Each dial task holds a receiver, so [`watch::Sender::closed`]
	/// waits for every one of them to end.
	///
	/// [`watch::Sender::closed`]: tokio::sync::watch::Sender::closed
	stop: Arc<tokio::sync::watch::Sender<bool>>,
}

struct DialersPhase {
	/// `None` once closed, which refuses any further dial.
	open: Option<DialersState>,
	/// The shutdown the first close started, which every close awaits, so none
	/// returns before it is done.
	closing: Option<futures::future::Shared<futures::future::BoxFuture<'static, ()>>>,
}

struct DialersState {
	clients: HashMap<u64, moq_tokio::Client>,
	next: u64,
	/// The closes of clients whose [`Held`] guard dropped, polled by
	/// [`Started::run`] and drained by the shutdown.
	closing: kio::Tasks<Child<()>>,
}

impl Dialers {
	fn new() -> Self {
		Self {
			state: Arc::new(Mutex::new(DialersPhase {
				open: Some(DialersState {
					clients: HashMap::new(),
					next: 0,
					closing: kio::Tasks::new(),
				}),
				closing: None,
			})),
			stop: Arc::new(tokio::sync::watch::Sender::new(false)),
		}
	}

	fn lock(&self) -> std::sync::MutexGuard<'_, DialersPhase> {
		self.state.lock().unwrap_or_else(PoisonError::into_inner)
	}

	/// Keep `client` until [`Self::close`].
	fn insert(&self, client: &moq_tokio::Client) -> anyhow::Result<u64> {
		let mut phase = self.lock();
		let state = phase.open.as_mut().context("cluster closed")?;
		let id = state.next;
		state.next += 1;
		state.clients.insert(id, client.clone());
		Ok(id)
	}

	/// Keep `client` until [`Self::close`] or until the guard drops, which queues its
	/// close for [`Self::poll_closing`].
	fn hold(&self, client: &moq_tokio::Client) -> anyhow::Result<Held> {
		Ok(Held {
			id: self.insert(client)?,
			dialers: self.clone(),
		})
	}

	/// Run one dial task until it ends or [`Self::close`] stops it.
	async fn supervise(&self, dial: impl std::future::Future<Output = ()>) {
		let mut stop = self.stop.subscribe();
		tokio::select! {
			// First, so a dial spawned just before the close never starts.
			biased;
			_ = stop.wait_for(|stopped| *stopped) => {}
			() = dial => {}
		}
	}

	/// Advance the closes of clients whose guard dropped. Never resolves the dials'
	/// lifetime: an empty set is only drained for now.
	fn poll_closing(&self, waiter: &kio::Waiter) {
		if let Some(state) = self.lock().open.as_mut() {
			let _ = state.closing.poll_each(waiter, |_, res| {
				if let Err(err) = res {
					tracing::warn!(%err, "cluster client close failed");
				}
			});
		}
	}

	/// Stop every dial task and close every client, including those a dropped
	/// guard is still closing, and refuse further dials.
	async fn close(&self) {
		let closing = {
			let mut phase = self.lock();
			let phase = &mut *phase;
			phase
				.closing
				.get_or_insert_with(|| {
					let state = phase.open.take().expect("open until the first close");
					Self::shutdown(state, self.stop.clone()).boxed().shared()
				})
				.clone()
		};
		closing.await;
	}

	async fn shutdown(state: DialersState, stop: Arc<tokio::sync::watch::Sender<bool>>) {
		stop.send_replace(true);
		let DialersState {
			clients, mut closing, ..
		} = state;
		futures::future::join_all(clients.into_values().map(moq_tokio::Client::close)).await;
		kio::wait(|waiter| closing.poll_each(waiter, |_, _| {})).await;
		// Each stopped task dropped its link, which ends a non-QUIC one.
		stop.closed().await;
	}
}

/// A dial's claim on its client in [`Dialers`].
struct Held {
	dialers: Dialers,
	id: u64,
}

impl Drop for Held {
	fn drop(&mut self) {
		let mut phase = self.dialers.lock();
		// Already taken by `Dialers::close`, which closes the client itself.
		let Some(state) = phase.open.as_mut() else {
			return;
		};
		let Some(client) = state.clients.remove(&self.id) else {
			return;
		};
		state.closing.push(Child::new("cluster client close", client.close()));
	}
}

/// Configuration for relay clustering.
///
/// [`Self::connect`] / [`Self::api`] list peers to dial, and
/// [`Self::node`] is this relay's own URL (identity). A relay only dials peers
/// configured here or found on the LAN; it never dials a URL learned from an
/// announcement.
///
/// Hop-based routing on broadcasts prevents announcement loops regardless of topology.
#[serde_with::serde_as]
#[derive(usage_rs::Args, Clone, Debug, serde::Serialize, serde::Deserialize, Default)]
#[usage(unknown_flags = "error", args_override_self = false)]
#[serde_with::skip_serializing_none]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct Config {
	/// Fixed origin (hop) id for this relay, identifying it in the hop chains
	/// carried on each broadcast for loop detection and shortest-path routing.
	///
	/// Unset (the default) picks a fresh random id on every start. Set it to give
	/// a node a stable identity across restarts. Must be non-zero; 0 errors at
	/// startup. Keep it below 2^53 for compatibility with older `@moq/lite` JS
	/// clients, which decode hop ids as a `u53` and reject anything larger. A
	/// moq-lite-06 or older link carries an id at or above 2^62 as 0, losing loop
	/// detection through this relay on that link.
	#[usage(
		name = "cluster-id",
		long = "cluster-id",
		env = "MOQ_CLUSTER_ID",
		setting = "cluster.id"
	)]
	pub id: Option<u64>,

	/// Longest hop path accepted or forwarded, from 1 to 32 (default 32).
	#[usage(
		name = "cluster-max-hops",
		long = "cluster-max-hops",
		env = "MOQ_CLUSTER_MAX_HOPS",
		setting = "cluster.max_hops"
	)]
	pub max_hops: Option<usize>,

	/// Connect to one or more other cluster nodes. Each entry is a full URL, e.g.
	/// `https://host/?jwt=TOKEN`, or an object with `url`, `cost`, `egress`,
	/// `token`, and `upstream`. A bare host or `host:port` is refused; pass the
	/// full URL instead. Accepts a comma-separated list on the CLI or repeat the
	/// flag; in config files use a TOML array of URLs and/or objects.
	///
	/// A `?cost=N` query param (or object `cost`) prices the link (moq-lite-06+):
	/// every announcement crossing it adds `N` to its route cost, so routing
	/// prefers cheap paths over short ones. Use `0` for a same-datacenter sibling
	/// and something large for a metered backbone; an unpriced link costs 1,
	/// which reproduces plain hop counting. The param is consumed by this relay,
	/// not sent to the peer. Object `egress` defaults to `cost`; anything else is
	/// rejected until asymmetric routing lands.
	#[usage(
		name = "cluster-connect",
		long = "cluster-connect",
		env = "MOQ_CLUSTER_CONNECT",
		delimiter = ',',
		setting = "cluster.connect"
	)]
	#[serde_as(as = "serde_with::OneOrMany<_>")]
	pub connect: Vec<Peer>,

	/// Dynamic peer-list source and its independent TLS trust.
	#[usage(flatten)]
	pub api: api::Config,

	#[usage(long = "cluster-connect-api", env = "MOQ_CLUSTER_CONNECT_API", hide = true)]
	#[serde(skip)]
	pub(crate) legacy_api: Option<String>,
	#[usage(
		long = "cluster-connect-api-tls-root",
		env = "MOQ_CLUSTER_CONNECT_API_TLS_ROOT",
		hide = true
	)]
	#[serde(skip)]
	pub(crate) legacy_api_root: Vec<PathBuf>,
	#[usage(skip)]
	#[serde(rename = "connect_api", skip_serializing)]
	pub(crate) removed_api: Option<serde::de::IgnoredAny>,
	#[usage(skip)]
	#[serde(rename = "connect_api_tls_root", skip_serializing)]
	pub(crate) removed_api_root: Option<serde::de::IgnoredAny>,

	/// This relay's own externally-reachable URL (identity). Sent to
	/// `--cluster-api-url` as a `?node=` query param so the endpoint can return
	/// this node's peers, and advertised over mDNS when LAN discovery is on. On
	/// its own it neither opens nor accepts a connection.
	#[usage(
		name = "cluster-node",
		long = "cluster-node",
		env = "MOQ_CLUSTER_NODE",
		setting = "cluster.node"
	)]
	pub node: Option<String>,

	/// LAN discovery over mDNS (`[cluster.lan]`).
	#[cfg(feature = "cluster-lan")]
	#[usage(flatten)]
	#[serde(default)]
	pub lan: LanConfig,

	/// JWT presented on outbound cluster dials, read from this file. Applied to
	/// any static or API peer whose URL doesn't already carry a `?jwt=` and
	/// whose object entry sets no `token`. An inline `?jwt=` or object `token`
	/// provides a per-peer credential instead. LAN peers never receive it; they
	/// authenticate with their mDNS credential.
	#[usage(
		name = "cluster-token",
		long = "cluster-token",
		env = "MOQ_CLUSTER_TOKEN",
		setting = "cluster.token"
	)]
	pub token: Option<PathBuf>,

	/// Billing tier label that cluster-peer (relay-to-relay) traffic records
	/// stats under: `/`-joined segments of ASCII letters, digits, `.`, `_`, and `-`.
	/// Defaults to the unprefixed tier.
	#[usage(
		name = "cluster-tier",
		long = "cluster-tier",
		env = "MOQ_CLUSTER_TIER",
		setting = "cluster.tier"
	)]
	pub tier: Option<String>,
}

impl Config {
	/// Released spellings this config was parsed from, each paired with what replaced it.
	pub fn deprecated(&self) -> moq_tokio::cli::Deprecated {
		let mut found = moq_tokio::cli::Deprecated::default();
		if self.legacy_api.is_some() {
			found.flag(
				"--cluster-connect-api",
				Some("MOQ_CLUSTER_CONNECT_API"),
				"--cluster-api-url",
			);
		}
		if !self.legacy_api_root.is_empty() {
			found.flag(
				"--cluster-connect-api-tls-root",
				Some("MOQ_CLUSTER_CONNECT_API_TLS_ROOT"),
				"--cluster-api-tls-root",
			);
		}
		if self.removed_api.is_some() {
			found.toml("[cluster] connect_api", "[cluster.api] url", None);
		}
		if self.removed_api_root.is_some() {
			found.toml("[cluster] connect_api_tls_root", "[cluster.api.tls] root", None);
		}

		if self.connect.iter().any(|peer| is_legacy_peer(peer.url())) {
			found.changed(
				"--cluster-connect",
				Some("MOQ_CLUSTER_CONNECT"),
				"a full URL like https://host/?jwt=TOKEN",
				"a bare host or host:port is no longer accepted",
			);
		}
		found
	}
}

/// LAN discovery configuration (`[cluster.lan]`).
///
/// Advertises this process over mDNS and dials the peers that advertise back,
/// so a rack or a home lab meshes with no seed list and no shared rendezvous.
/// A LAN peer authenticates with its mDNS credential on `/.cluster/<credential>`
/// and is never handed [`Config::token`].
#[derive(usage_rs::Args, Clone, Debug, serde::Serialize, serde::Deserialize, Default)]
#[usage(unknown_flags = "error", args_override_self = false)]
#[serde_with::skip_serializing_none]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
#[cfg(feature = "cluster-lan")]
pub struct LanConfig {
	/// Enable mDNS discovery. Boolean flag: pass `--cluster-lan` (or `=true` /
	/// `=false`). Advertises the listener fingerprint when the certificate was
	/// generated or supplied in-memory, the `--cluster-node` URL when one
	/// is configured, and needs at least one of them.
	#[usage(
		name = "cluster-lan",
		long = "cluster-lan",
		env = "MOQ_CLUSTER_LAN",
		setting = "cluster.lan.enabled",
		bool_value
	)]
	pub enabled: bool,

	/// A file holding the shared key admitting a peer to the LAN mesh, as 64
	/// hexadecimal characters (`openssl rand -hex 32`).
	///
	/// Optional. Without it, anyone who can reach the listener joins, so leave
	/// it unset only on networks you trust. With it, only peers that prove they
	/// hold the same key are discovered or accepted.
	#[usage(
		name = "cluster-lan-secret",
		long = "cluster-lan-secret",
		env = "MOQ_CLUSTER_LAN_SECRET",
		setting = "cluster.lan.secret"
	)]
	#[usage(value_hint = usage_rs::ValueHint::FilePath)]
	pub secret: Option<std::path::PathBuf>,

	/// DNS-SD application this relay advertises under. Peers using a different
	/// name never discover this one. Defaults to `default`, which moq-cli
	/// shares so they find each other with no configuration. An application
	/// built on the library picks its own name.
	#[usage(
		name = "cluster-lan-app",
		long = "cluster-lan-app",
		env = "MOQ_CLUSTER_LAN_APP",
		value_name = "NAME",
		setting = "cluster.lan.app"
	)]
	pub app: Option<moq_tokio::mdns::App>,
}

#[cfg(feature = "cluster-lan")]
impl LanConfig {
	/// Reject a secret or app configured without the mesh that would read it.
	pub fn validate(&self) -> anyhow::Result<()> {
		anyhow::ensure!(
			self.secret.is_none() || self.enabled,
			"--cluster-lan-secret requires --cluster-lan=true"
		);
		anyhow::ensure!(
			self.app.is_none() || self.enabled,
			"--cluster-lan-app requires --cluster-lan=true"
		);
		Ok(())
	}
}

/// What a LAN advertisement publishes besides the listen port.
///
/// Pass to [`Cluster::with_advertise`] after the QUIC listener is bound. The
/// fingerprint is the generated or in-memory certificate's, when there is one;
/// a loaded certificate is dialed by name via [`Config::node`] instead.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct LanAdvertise {
	/// The bound QUIC listen port, advertised as the DNS-SD SRV port.
	pub port: u16,
	/// Hex SHA-256 fingerprint of a generated or in-memory certificate, to pin when dialing.
	pub fingerprint: Option<String>,
}

impl LanAdvertise {
	/// Advertise this listen port, with no fingerprint.
	pub fn new(port: u16) -> Self {
		Self {
			port,
			fingerprint: None,
		}
	}

	/// Pin this generated or in-memory certificate fingerprint when peers dial.
	pub fn with_fingerprint(mut self, fingerprint: impl Into<String>) -> Self {
		self.fingerprint = Some(fingerprint.into());
		self
	}
}

/// The per-advertisement credential [`Connection::authenticate`] checks on
/// `/.cluster/<credential>`. Shared via `Arc` so clones taken before
/// [`Cluster::start`] still see it.
#[cfg(feature = "cluster-lan")]
struct LanAuth {
	credential: String,
}

/// Construction settings for a [`Cluster`]: identity, discovery, and the origin cache.
///
/// The origin is built once from these, so cache settings cannot detach a handle
/// taken after construction. Independent services ([`Cluster::with_connect`],
/// [`Cluster::with_stats`]) attach afterwards without rebuilding it.
#[derive(Default)]
#[non_exhaustive]
pub struct Options {
	/// Session limits for ordinary client sessions.
	pub session: crate::limits::Client,
	/// Session and QUIC limits for authenticated peers and outbound cluster links.
	pub peer: crate::limits::Peer,
	/// Cluster identity, peers, and discovery.
	pub config: Config,

	/// Shared group pool and per-track retention ceiling.
	///
	/// `None` uses an unbounded pool with the standard LRU window and no
	/// media-timestamp ceiling.
	pub cache: Option<crate::cache::Cache>,
}

impl Options {
	/// Construct from cluster config, leaving the origin cache at its defaults.
	pub fn new(config: Config) -> Self {
		Self {
			config,
			..Default::default()
		}
	}

	/// Use this resolved cache when constructing the origin.
	pub fn with_cache(mut self, cache: crate::cache::Cache) -> Self {
		self.cache = Some(cache);
		self
	}
}

/// The work a [`Cluster`] needs to serve at all: its origin's lifecycle driver and,
/// when the cache has one, the headroom governor. Returned by [`Cluster::new`].
///
/// Nothing runs on its own: poll it, or await [`run`](Self::run), for as long as the
/// cluster serves. Dropping it tears the origin down, failing every later mutation.
#[must_use = "the origin makes no progress unless its driver is polled"]
pub struct Driver {
	work: kio::Tasks<Child<()>>,
}

impl Driver {
	/// Drive the cluster's work until all of it has finished, which the origin only
	/// does once every owner of it is gone, or until any of it panics.
	///
	/// Resolves `Ok` once finished, or with the first panic as
	/// `"<task> panicked: <message>"`, the error [`Relay::run`](crate::Relay::run)
	/// exits with. Polling on after that keeps driving the rest.
	pub fn poll(&mut self, waiter: &kio::Waiter) -> Poll<anyhow::Result<()>> {
		let mut failed = None;
		let poll = self.work.poll_each(waiter, |_, res| {
			if let Err(err) = res {
				failed.get_or_insert(err);
			}
		});
		match failed {
			Some(err) => Poll::Ready(Err(err)),
			None => poll.map(Ok),
		}
	}

	/// [`poll`](Self::poll) until it resolves.
	pub async fn run(mut self) -> anyhow::Result<()> {
		kio::wait(move |waiter| self.poll(waiter)).await
	}
}

impl std::fmt::Debug for Driver {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Driver").field("tasks", &self.work.len()).finish()
	}
}

/// A [`Cluster`] whose config is validated and whose resources are bound,
/// produced by [`Cluster::start`] and run with [`run`](Self::run).
///
/// It owns the cluster it came from rather than being handed back to one. The
/// two carry matched state (a resolved node URL, a token, a live mDNS
/// advertisement), so letting a caller pair them up would let one cluster run on
/// another's identity, or a standalone startup silently disable a configured
/// cluster. Owning it makes that unrepresentable instead of merely wrong.
///
/// Holding one means the config parsed and the LAN advertisement (if any) is
/// live, so dropping it without running retires that advertisement.
pub struct Started {
	cluster: Cluster,
	/// `None` when nothing is configured, so [`run`](Self::run) dials nothing.
	work: Option<Work>,
}

impl Started {
	/// True when nothing is configured, so [`run`](Self::run) dials nothing.
	pub fn standalone(&self) -> bool {
		self.work.is_none()
	}

	/// Run the cluster's dials and peer sources on this task until they stop, or
	/// until one of them or a task the relay cannot serve without (its stats
	/// publisher, ACME renewal, or certificate reload) panics. The origin's own work
	/// is the [`Driver`]'s.
	///
	/// Standalone, this only waits for such a panic. Returning, a failure
	/// included, closes every dial as [`Cluster::close`] does. A cancelled `run`
	/// drops its dials but leaves their links' close to [`Cluster::close`], so a
	/// caller that must have peers told before exiting awaits it or lets `run`
	/// return.
	pub async fn run(self) -> anyhow::Result<()> {
		// Registered only now, so a start that fails or never runs drops its client
		// and frees the bind. A closed cluster has nothing left to run.
		if let Some(dial) = &self.cluster.dial
			&& self.cluster.dialers.insert(dial).is_err()
		{
			return Ok(());
		}
		let critical = self.cluster.critical.clone();
		let dialers = self.cluster.dialers.clone();
		let work = async move {
			match self.work {
				Some(work) => {
					let mut mesh = self.cluster.mesh(work)?;
					kio::wait(move |waiter| mesh.poll(waiter)).await
				}
				None => std::future::pending().await,
			}
		};
		let result = tokio::select! {
			res = work => res,
			err = critical.failed() => Err(err),
		};
		// The dial tasks went with `work`, and a caller that consumed the cluster
		// has no handle left to close it, so each peer gets its close here.
		dialers.close().await;
		result
	}
}

/// Hand-written because the bound mDNS advertisement isn't `Debug`, and the
/// contents are internal anyway: what a reader wants is whether there is
/// anything to run.
impl std::fmt::Debug for Started {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Started")
			.field("standalone", &self.work.is_none())
			.finish()
	}
}

/// The resolved settings behind a [`Started`] that has work to do.
struct Work {
	node: Option<String>,
	token: String,
	http: Option<ClientWithMiddleware>,
	/// The live mDNS advertisement, bound by [`Cluster::start`].
	#[cfg(feature = "cluster-lan")]
	discovery: Option<moq_tokio::mdns::Discovery>,
}

/// The peer dials and the sources feeding them, all polled on the task running
/// [`Started::run`] rather than spawned.
struct Mesh {
	cluster: Cluster,
	/// The shared cluster token, for dials whose URL carries none.
	token: String,
	/// Every peer dial, however many sources want it.
	dialed: DialMap<Child<()>>,
	/// The `api.url` reconciler and LAN discovery, which feed `commands`.
	sources: kio::Tasks<Child<anyhow::Result<()>>>,
	commands: DialCommands,
}

impl Mesh {
	/// Drive the dials and their sources.
	///
	/// Resolves `Ok` once nothing is left to run: every dial ended and no source
	/// can add another. Resolves with an error once a source fails or any of it
	/// panics.
	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<anyhow::Result<()>> {
		let mut failed = None;
		// Sources first, so whatever a finishing source queued is applied below.
		let sources = self.sources.poll_each(waiter, |_, res| {
			if let Err(err) = res.and_then(|res| res) {
				failed.get_or_insert(err);
			}
		});
		while let Poll::Ready(Ok(command)) = self.commands.poll_pop(waiter) {
			self.apply(command);
		}
		// Each dial attempt contains its own panic, so one here is in the dial loop itself.
		let dials = self.dialed.tasks.poll_each(waiter, |_, res| {
			if let Err(err) = res {
				failed.get_or_insert(err);
			}
		});
		self.cluster.dialers.poll_closing(waiter);

		if let Some(err) = failed {
			return Poll::Ready(Err(err));
		}
		match (sources, dials) {
			(Poll::Ready(()), Poll::Ready(())) => Poll::Ready(Ok(())),
			_ => Poll::Pending,
		}
	}

	/// Apply one source's change to the dial set.
	fn apply(&mut self, command: DialCommand) {
		let mut dial = dialer(&self.cluster, &self.token);
		match command {
			DialCommand::Api(desired) => self.dialed.reconcile_api(&desired, &mut dial),
			#[cfg(feature = "cluster-lan")]
			DialCommand::Found(target) => self.dialed.upsert(target, DialSource::Mdns, &mut dial),
			#[cfg(feature = "cluster-lan")]
			DialCommand::Lost(peer) => self.dialed.release(&peer, DialSource::Mdns, &mut dial),
		}
	}
}

/// Build the task that dials and redials one peer.
fn dialer<'a>(cluster: &'a Cluster, token: &'a str) -> impl FnMut(DialTarget) -> Child<()> + 'a {
	move |target| {
		// Logged by key, never by the target's URL, so an inline query stays out of the logs.
		tracing::info!(peer = %target.key, lan = target.lan, "dialing cluster peer");
		Child::new(
			"cluster dial",
			cluster.clone().supervise_remote(target, token.to_string()),
		)
	}
}

/// A relay cluster built around a single [`origin::Producer`].
///
/// Local sessions and remote cluster connections all publish into the same
/// origin. Loop prevention and route preference come from the hop list carried
/// on each announced route (see [`moq_net::origin::Route`]).
///
/// Construct with [`Cluster::new`], then attach a dial template and (optionally)
/// a [`stats::Registry`](moq_net::stats::Registry) with the `with_*` builder
/// methods. Those builders do not rebuild the origin. A cluster without a
/// dial template can serve local sessions but cannot dial remote peers.
#[derive(Clone)]
pub struct Cluster {
	session_limits: moq_net::session::Limits,
	peer_limits: moq_net::session::Limits,
	peer_quic: crate::limits::Quic,
	config: Config,
	/// Dial template every peer dial is built from; see [`Self::peer_client`].
	connect: Option<moq_tokio::connect::Config>,
	quic: Option<moq_tokio::quic::Config>,
	#[cfg(feature = "iroh")]
	iroh: Option<moq_tokio::iroh::Endpoint>,
	/// The client static and API peers share when the template binds a fixed
	/// `[connect]` port, so that port stays the cluster's egress port. Otherwise
	/// `None`, and every session builds its own client on an ephemeral port. Built
	/// by [`Self::start`] and joins [`Self::dialers`] once [`Started::run`] begins.
	dial: Option<moq_tokio::Client>,
	/// Every dial client, shared by all clones, for [`Self::close`].
	dialers: Dialers,
	/// Bound listen port and optional generated-certificate fingerprint.
	advertise: Option<LanAdvertise>,
	/// Set by [`Self::start`] when LAN discovery is on, so inbound
	/// `/.cluster/<credential>` can be verified on every clone.
	#[cfg(feature = "cluster-lan")]
	lan_auth: Arc<std::sync::OnceLock<LanAuth>>,
	pub(crate) nodes: crate::nodes::Nodes,

	/// Sessions admission turned away, by reason. Shared by every listener and
	/// runtime so `/metrics` sees every refusal on the node.
	pub(crate) refusals: crate::refusals::Refusals,

	/// Hands out the `conn` id every session logs under, inbound and outbound
	/// alike, so one id space covers the whole process and an id in the `/nodes`
	/// view always points at the same session in the logs.
	connection_ids: Arc<AtomicU64>,

	/// The stats publisher, ACME renewal, and certificate reload: a panic in any
	/// of them stops [`Relay::run`](crate::Relay::run).
	pub(crate) critical: crate::critical::Critical,

	/// All broadcasts, local and remote. Downstream sessions read from here
	/// (filtered by their auth token) and remote dials both read and write here.
	pub origin: origin::Producer,

	/// Stats registry. One instance per relay; sessions pick a billing tier via
	/// [`stats::Registry::tier`](moq_net::stats::Registry::tier) at acceptance time
	/// (the default tier unless configured otherwise, or any label the auth API
	/// returns) so traffic classes land in separate counter sets. Defaults
	/// to a disabled (no-op) registry until [`with_stats`](Self::with_stats) is called.
	pub stats: moq_net::stats::Registry,

	/// Keeps the stats publish task running for as long as any handle on this
	/// cluster lives. The task holds only a `Weak` to its producer, so it stops
	/// when the last [`moq_stats::Producer`] clone drops; parking one here means
	/// serving and publishing end together, instead of publishing ending early
	/// because a caller dropped a producer it never asked for.
	_stats_publisher: Option<moq_stats::Producer>,
}

/// A gateway or network session admitted with its lease, scoped origins, and stats.
#[non_exhaustive]
pub(crate) struct Admitted {
	/// The live authorization that must be held for the session's lifetime.
	pub lease: auth::Lease,
	/// Where an admitted publisher writes its broadcasts.
	pub publisher: Option<origin::Producer>,
	/// Where an admitted subscriber reads broadcasts.
	pub subscriber: Option<origin::Consumer>,
	/// The session's root and tier attribution.
	pub stats: moq_net::stats::Session,
}

impl Cluster {
	/// The request caps for an admitted session: only a `peer` grant gets the peer policy.
	pub(crate) fn limits(&self, lease: &auth::Lease) -> moq_net::session::Limits {
		if lease.token().peer {
			self.peer_limits
		} else {
			self.session_limits
		}
	}

	/// The QUIC limits an admitted session runs under once it is answered: a `peer`
	/// grant's wider ones, and the lease's bitrate caps. A receive window left unset opens
	/// to the configured one.
	pub(crate) fn quic_limits(&self, lease: &auth::Lease) -> moq_quic::Limits {
		let mut limits = match lease.token().peer {
			true => self.peer_quic.limits(),
			false => moq_quic::Limits::default(),
		};
		let caps = lease.caps().limits();
		limits.receive_rate = caps.receive_rate;
		limits.send_rate = caps.send_rate;
		limits
	}

	/// The origin ID used by this relay on the wire.
	pub fn id(&self) -> u64 {
		self.origin.hop().id()
	}

	/// Creates a cluster with one origin, using [`Options`] for identity
	/// and cache.
	///
	/// Use [`with_connect`](Self::with_connect) to enable dialing remote peers
	/// (required when `config.connect` is non-empty), and
	/// [`with_stats`](Self::with_stats) to enable metrics publishing. Those
	/// builders do not rebuild the origin.
	///
	/// Returns the cluster with its [`Driver`]: the origin's lifecycle work and the
	/// cache governor, which run only while the owner polls it. Poll it for as long
	/// as the cluster serves, whether or not [`start`](Self::start) (the mesh half)
	/// is ever called; [`Relay::run`](crate::Relay::run) does. Dropping it tears the
	/// origin down.
	///
	/// Errors if `config.id` is set to 0. An unset id picks a fresh random origin.
	/// Errors too if the `peer.quic` receive window does not fit a QUIC varint, or
	/// `config.tier` is not a valid stats tier.
	pub fn new(options: Options) -> anyhow::Result<(Self, Driver)> {
		let Options {
			config,
			cache,
			session,
			peer,
		} = options;
		let id = match config.id {
			Some(id) => Hop::new(id).context("--cluster-id must be non-zero")?,
			None => Hop::random(),
		};
		let deprecated = config.deprecated();
		anyhow::ensure!(deprecated.is_empty(), "{deprecated}");
		// Reject a repeated static identity with conflicting policy up front,
		// matching the `--cluster-api-url` validation, instead of silently
		// keeping the first entry when dials spawn.
		parse_peer_list(config.connect.clone(), None).context("invalid --cluster-connect peer list")?;
		if let Some(tier) = &config.tier {
			moq_auth::tier::validate(tier).context("invalid --cluster-tier")?;
		}
		crate::configured_tier(config.tier.clone()).context("invalid --cluster-tier")?;
		peer.quic.validate()?;
		let mut work = kio::Tasks::new();
		let mut origin_config = origin::Config::new(id);
		if let Some(max) = config.max_hops {
			origin_config.max_hops = origin::HopLimit::new(max).context("--cluster-max-hops must be from 1 to 32")?;
		}
		if let Some(cache) = cache {
			origin_config.pool = cache.pool;
			origin_config.cache_duration = cache.duration;
			if let Some(governor) = cache.governor {
				work.push(Child::new("cache governor", governor));
			}
		}
		let (origin, driver) = origin::Producer::new(origin_config);
		work.push(Child::new("origin driver", async move {
			moq_net::time::run(driver).await;
		}));
		let nodes = crate::nodes::Nodes::default();
		tracing::info!(hop_id = %origin.hop(), configured = config.id.is_some(), "cluster initialized");
		let cluster = Cluster {
			session_limits: session.resolve(),
			peer_limits: peer.resolve(),
			peer_quic: peer.quic,
			config,
			connect: None,
			quic: None,
			dial: None,
			dialers: Dialers::new(),
			#[cfg(feature = "iroh")]
			iroh: None,
			advertise: None,
			#[cfg(feature = "cluster-lan")]
			lan_auth: Arc::new(std::sync::OnceLock::new()),
			nodes,
			refusals: Default::default(),
			connection_ids: Arc::default(),
			critical: Default::default(),
			origin,
			stats: moq_net::stats::Registry::disabled(),
			_stats_publisher: None,
		};
		Ok((cluster, Driver { work }))
	}

	/// Admit a gateway session and scope both origin directions from its grant.
	///
	/// Waits on the decider for as long as the caller does; the caller owns the
	/// deadline, as for [`auth::Auth::admit`].
	#[cfg(test)]
	pub(crate) async fn admit(&self, auth: &auth::Auth, request: moq_auth::Request) -> Result<Admitted, auth::Error> {
		let result = async {
			if !matches!(request.event, moq_auth::Event::Connect) {
				return Err(auth::Error::Request("admission requires a connect event".into()));
			}
			let lease = auth.admit(request.clone()).await?;
			self.scope(lease, &request)
		}
		.await;
		result.inspect_err(|err| self.refusals.record_auth(err))
	}

	/// Network sessions enforce changing grants in AUTH, within a fixed root and SETUP role.
	pub(crate) async fn admit_session(
		&self,
		auth: &auth::Auth,
		request: moq_auth::Request,
		pacer: Option<auth::Pacer>,
	) -> Result<Admitted, auth::Error> {
		let result = async {
			let lease = auth.admit_paced(request.clone(), pacer).await?;
			self.scope_session(lease, &request)
		}
		.await;
		result.inspect_err(|err| self.refusals.record_auth(err))
	}

	pub(crate) fn scope_session(
		&self,
		lease: auth::Lease,
		request: &moq_auth::Request,
	) -> Result<Admitted, auth::Error> {
		let mut admitted = self.scope(lease, request)?;
		let mut bound = admitted.lease.token().clone();
		bound.publish = moq_net::Pattern::all().into();
		bound.subscribe = moq_net::Pattern::all().into();
		if request.role != Some(moq_auth::Role::Subscriber) {
			admitted.publisher = self
				.publisher(&bound)
				.map(|origin| origin.with_stats(admitted.stats.clone()));
		}
		if request.role != Some(moq_auth::Role::Publisher) {
			admitted.subscriber = self.subscriber(&bound).map(|origin| {
				origin
					.consume()
					.with_hidden(bound.peer)
					.with_stats(admitted.stats.clone())
			});
		}
		Ok(admitted)
	}

	/// Resolve origin handles for a lease already admitted by a local relay rule.
	pub(crate) fn scope(&self, lease: auth::Lease, request: &moq_auth::Request) -> Result<Admitted, auth::Error> {
		let token = lease.token();
		let publisher = self.publisher(token);
		let subscriber = self.subscriber(token);
		let allowed = match request.role {
			Some(moq_auth::Role::Publisher) => publisher.is_some(),
			Some(moq_auth::Role::Subscriber) => subscriber.is_some(),
			None => publisher.is_some() || subscriber.is_some(),
		};
		if !allowed {
			let wanted = match request.role {
				Some(moq_auth::Role::Publisher) => "publisher",
				Some(moq_auth::Role::Subscriber) => "subscriber",
				None => "any",
			};
			return Err(auth::Error::Forbidden(format!(
				"grant does not allow {wanted} access to {}",
				token.root
			)));
		}

		let stats = self.stats.tier(token.tier.clone()).session(&token.label);
		tracing::info!(transport = %request.transport, ?request.role, tier = %token.tier, root = ?token.root,
			publish = ?publisher.as_ref().map(origin::Producer::allowed),
			subscribe = ?subscriber.as_ref().map(origin::Producer::allowed),
			"session accepted");
		let publisher = match request.role {
			Some(moq_auth::Role::Subscriber) => None,
			_ => publisher.map(|origin| origin.with_stats(stats.clone())),
		};
		// A cluster peer (a `peer` grant, which the LAN credential also gets) discovers
		// hidden routes whether or not it asks. A peer that predates the hidden opt-in
		// (below moq-lite-07-wip, or moq-transport without MoQ Hidden) would otherwise
		// lose every dot path during a rolling upgrade. A client certificate alone is a
		// fact for the auth server, never this exemption.
		// TODO: drop the exemption once deployed peers all opt in.
		let subscriber = match request.role {
			Some(moq_auth::Role::Publisher) => None,
			_ => subscriber.map(|origin| origin.consume().with_hidden(token.peer).with_stats(stats.clone())),
		};
		Ok(Admitted {
			lease: lease.with_stats(stats.clone()),
			publisher,
			subscriber,
			stats,
		})
	}

	/// Attach the dial template every peer dial is built from, with the
	/// [`Options::peer`] QUIC limits. Each peer session binds its own ephemeral
	/// port on the template's bind address, so a redial leaves from a fresh one.
	/// A fixed bind port is kept instead: static and API peers share one socket
	/// on it, while LAN dials still take ephemeral ports.
	///
	/// Required when any peer is configured; [`start`](Self::start) returns an
	/// error otherwise.
	pub fn with_connect(mut self, connect: moq_tokio::connect::Config, quic: moq_tokio::quic::Config) -> Self {
		self.connect = Some(connect);
		self.quic = Some(quic);
		self
	}

	/// Dial `iroh://` peers through this endpoint.
	#[cfg(feature = "iroh")]
	pub fn with_iroh(mut self, iroh: moq_tokio::iroh::Endpoint) -> Self {
		self.iroh = Some(iroh);
		self
	}

	/// Advertise this bound listener on the LAN.
	///
	/// Required when `--cluster-lan` is on; [`start`](Self::start) returns an
	/// error otherwise. The fingerprint is set when the certificate was
	/// generated or supplied in-memory, so peers can pin it.
	pub fn with_advertise(mut self, advertise: LanAdvertise) -> Self {
		self.advertise = Some(advertise);
		self
	}

	/// Reserve the next `conn` id, the handle a session is logged under.
	///
	/// One counter serves inbound accepts and outbound cluster dials, so an id in
	/// the internal `/nodes` view names exactly one session in the logs. Pass it to
	/// [`Connection::with_id`](crate::Connection::with_id) for every request you
	/// accept, rather than counting separately.
	pub fn next_connection_id(&self) -> u64 {
		self.connection_ids.fetch_add(1, Ordering::Relaxed)
	}

	/// Attach a stats producer, replacing the default disabled registry with its
	/// registry, and run its publisher.
	///
	/// Build both with [`stats::Config::build`](crate::stats::Config::build), passing
	/// [`Self::origin`] so it publishes through the same origin cluster peers read
	/// from. The cluster holds the producer from here on, so the caller may drop
	/// its own handle: publishing lasts exactly as long as the cluster does. A
	/// panic in the publisher ends [`Relay::run`](crate::Relay::run), like the
	/// cluster's other critical tasks. Must be called within a Tokio runtime.
	pub fn with_stats(mut self, stats: moq_stats::Producer, publisher: moq_stats::Publisher) -> Self {
		self.critical.spawn("stats publisher", publisher.run());
		self.stats = stats.registry().clone();
		self.refusals = crate::refusals::Refusals::new(&self.stats);
		self._stats_publisher = Some(stats);
		self
	}

	/// Billing tier cluster-peer traffic records under (`--cluster-tier`).
	/// An absent or empty label selects the default unprefixed tier.
	fn cluster_tier(&self) -> Tier {
		crate::configured_tier(self.config.tier.clone()).expect("validated by Cluster::new")
	}

	/// Returns an [`origin::Producer`] scoped to this session's subscribe permissions.
	///
	/// Passed by reference to [`moq_net::Server::with_publisher`] (or the
	/// equivalent per-request setter), which derives the read handle. On an
	/// upstream grant that handle never sees a route learned from another upstream.
	pub fn subscriber(&self, token: &auth::Token) -> Option<origin::Producer> {
		self.mounted(token)?.scope(&token.root, &token.subscribe).ok()
	}

	/// Returns an [`origin::Producer`] scoped to this session's publish permissions,
	/// linked as [`origin::Link::Peer`] when the grant names a cluster peer and
	/// [`origin::Link::Upstream`] when that peer is upstream.
	/// Nothing is published beneath the grant's mounts.
	pub fn publisher(&self, token: &auth::Token) -> Option<origin::Producer> {
		self.mounted(token)?.scope(&token.root, &token.publish).ok()
	}

	/// The origin marked with the grant's link and with its mounts applied,
	/// before it is scoped to the session. A mount the origin refuses
	/// (overlapping another) admits nothing.
	fn mounted(&self, token: &auth::Token) -> Option<origin::Producer> {
		let mut origin = match (token.upstream, token.peer) {
			(true, _) => self.origin.clone().with_link(origin::Link::Upstream),
			(false, true) => self.origin.clone().with_link(origin::Link::Peer),
			(false, false) => self.origin.clone(),
		};
		for (at, target) in &token.mounts {
			let mounted = token.root.join(at).map_err(moq_net::Error::from);
			origin = match mounted.and_then(|at| origin.mount(at, target)) {
				Ok(origin) => origin,
				Err(err) => {
					tracing::warn!(root = ?token.root, ?at, ?target, ?err, "grant mount refused");
					return None;
				}
			};
		}
		Some(origin)
	}

	/// Whether `--cluster-lan` asked this relay to discover peers over mDNS.
	fn lan(&self) -> bool {
		#[cfg(feature = "cluster-lan")]
		return self.config.lan.enabled;
		#[cfg(not(feature = "cluster-lan"))]
		false
	}

	/// The credential a LAN path carries, if `path` is `/.cluster/<credential>`.
	///
	/// `/.cluster` and `/.cluster/` with no credential, and a path that merely
	/// starts with those letters (`/.clusterish`), yield `None`.
	pub(crate) fn lan_credential(path: &str) -> Option<&str> {
		let rest = path.strip_prefix(CLUSTER_PATH)?;
		match rest.strip_prefix('/') {
			Some(credential) if !credential.is_empty() => Some(credential),
			_ => None,
		}
	}

	/// Whether `path` is the LAN mesh marker, with or without a credential.
	pub fn is_lan_path(path: &str) -> bool {
		path == CLUSTER_PATH || path.starts_with(concat!("/.cluster", "/"))
	}

	/// Verify a presented `/.cluster/<credential>` against the live advertisement.
	///
	/// `None` means this cluster has no LAN discovery, so the path must be
	/// refused rather than routed through JWT or public prefixes.
	#[cfg(feature = "cluster-lan")]
	pub(crate) fn verify_lan_credential(&self, presented: &str) -> Option<bool> {
		self.lan_auth
			.get()
			.map(|auth| moq_tokio::mdns::ct_eq(&auth.credential, presented))
	}

	/// Verify a presented `/.cluster/<credential>` against the live advertisement.
	///
	/// Without the `cluster-lan` feature there is no discovery to consult.
	#[cfg(not(feature = "cluster-lan"))]
	pub(crate) fn verify_lan_credential(&self, _presented: &str) -> Option<bool> {
		None
	}

	/// The grant a LAN peer gets once its membership proof checks out: everything,
	/// billed under `--cluster-tier`. The proof is a secret this relay minted for
	/// itself, so no auth server is asked.
	pub(crate) fn lan_peer_grant(&self) -> moq_auth::Grant {
		let mut grant = moq_auth::Grant::new(moq_auth::Pattern::all(), moq_auth::Pattern::all());
		grant.tier = self.config.tier.clone().filter(|tier| !tier.is_empty());
		grant.peer = true;
		grant
	}

	/// Whether a protocol version carries the request path used to mark a mesh dial.
	pub(crate) fn carries_request_path(version: &moq_net::Version) -> bool {
		!version.is_lite() || version.code() >= 0xff0dad05
	}

	/// Reject version restrictions that leave a LAN dial with no request path.
	pub fn validate_lan_versions(
		client: &moq_tokio::connect::Config,
		server: &moq_tokio::listen::Config,
	) -> anyhow::Result<()> {
		let client = client.versions();
		let server = server.versions();
		anyhow::ensure!(
			client
				.iter()
				.any(|version| Self::carries_request_path(version) && server.contains(version)),
			"--cluster-lan needs --connect-version and --listen-version to share a version that carries a request path (moq-lite-05 and newer, or any moq-transport version)"
		);
		Ok(())
	}

	/// Close every peer link this cluster dialed, on any transport, and wait until
	/// each peer has been sent its close. Call it before the runtime stops: a
	/// dropped link only queues its close, so the peer would keep the link and its
	/// routes until its idle timeout. Dialing stops for good, on every clone, and
	/// every caller waits for the same shutdown. [`Relay::run`](crate::Relay::run)
	/// calls it on the way out.
	pub async fn close(&self) {
		self.dialers.close().await;
	}

	/// Validate the cluster config and bind anything that can fail, returning a
	/// [`Started`] to run.
	///
	/// Split from running because that future is first polled well after the
	/// process reports itself ready. Anything fallible left inside it (a malformed
	/// node URL, an unreadable token file, a bad LAN key, an mDNS bind failure)
	/// would release systemd's dependent units on a relay that is about to exit.
	/// Call this before signalling readiness, and hand the result to `run`.
	///
	/// Bails when peers are configured to dial but no usable dial template was
	/// attached via [`with_connect`](Self::with_connect).
	pub async fn start(mut self) -> anyhow::Result<Started> {
		let node = self.config.node.clone();
		let lan = self.lan();
		#[cfg(feature = "cluster-lan")]
		self.config.lan.validate()?;
		#[cfg(feature = "cluster-lan")]
		if lan {
			let advertise = self.advertise.as_ref();
			anyhow::ensure!(
				advertise.is_some(),
				"`--cluster-lan` needs a QUIC listener (call Cluster::with_advertise). \
				 See https://doc.moq.dev/bin/relay/cluster."
			);
			anyhow::ensure!(
				advertise.is_some_and(|a| a.fingerprint.is_some()) || node.is_some(),
				"`--cluster-lan` needs `--cluster-node <self-url>` or a generated certificate to advertise. \
				 See https://doc.moq.dev/bin/relay/cluster."
			);
			if let Some(connect) = &self.connect {
				anyhow::ensure!(
					connect.versions().iter().any(Self::carries_request_path),
					"--cluster-lan needs --connect-version to include a version that carries a request path (moq-lite-05 and newer, or any moq-transport version)"
				);
			} else {
				anyhow::bail!("`--cluster-lan` needs a dial template (call Cluster::with_connect)");
			}
		}

		let can_dial = !self.config.connect.is_empty() || self.config.api.url.is_some() || lan;
		if !can_dial {
			tracing::info!("no cluster peers configured; running standalone");
			return Ok(Started {
				work: None,
				cluster: self,
			});
		}

		// Build here so malformed API roots fail startup, before background work.
		let http = if self
			.config
			.api
			.url
			.as_deref()
			.map(connect_api_is_http)
			.transpose()?
			.unwrap_or(false)
		{
			let tls = self
				.connect
				.as_ref()
				.context("cluster.api.url with an http(s) URL needs a dial template (call Cluster::with_connect)")?
				.tls
				.build()?;
			Some(crate::http_client::build(&tls, &self.config.api.tls.root)?)
		} else {
			None
		};

		// Token presented on outbound dials whose URL doesn't already carry a
		// `?jwt=`. This remains the shared credential for any peer without a
		// per-peer inline token.
		let token = match &self.config.token {
			Some(path) => std::fs::read_to_string(path)
				.context("failed to read cluster token")?
				.trim()
				.to_string(),
			None => String::new(),
		};

		// Binds the multicast socket and reads the key, so it belongs here rather
		// than in `run`: both fail loudly, and the whole point of `--cluster-lan` is
		// that a relay which cannot join the mesh is misconfigured, not degraded.
		#[cfg(feature = "cluster-lan")]
		let discovery = match lan {
			true => {
				let advertise = self
					.advertise
					.as_ref()
					.expect("--cluster-lan needs Cluster::with_advertise");
				let secret = self.config.lan.secret.as_deref();
				let app = self.config.lan.app.clone().unwrap_or_default();
				Some(lan_discovery(advertise, node.as_deref(), secret, app).await?)
			}
			false => None,
		};

		// Built here, so a bad template fails startup rather than every dial, and
		// last, so a start that fails earlier never binds a fixed `[connect]` port.
		// Static and API peers share this client only on a fixed bind port, which
		// names one source port. Otherwise every session builds its own (see
		// `run_remote_session`), and this one only checks the template.
		let statics = !self.config.connect.is_empty() || self.config.api.url.is_some();
		let fixed = self
			.connect
			.as_ref()
			.is_some_and(|connect| connect.resolve().bind.port() != 0);
		let shared = statics && fixed;
		let client = self
			.peer_client(!shared, None)
			.context("cluster peers configured but the dial template is unusable")?;
		self.dial = shared.then_some(client);

		// After everything fallible, so a failed start leaves no credential behind.
		#[cfg(feature = "cluster-lan")]
		if let Some(discovery) = &discovery {
			let _ = self.lan_auth.set(LanAuth {
				credential: discovery.credential().to_string(),
			});
		}

		Ok(Started {
			work: Some(Work {
				http,
				node,
				token,
				#[cfg(feature = "cluster-lan")]
				discovery,
			}),
			cluster: self,
		})
	}

	/// The dials and peer sources for the work [`start`](Self::start) resolved:
	/// the static peers dialed at once, and the `api.url` reconciler and LAN
	/// discovery queued to feed the same dial set.
	fn mesh(self, work: Work) -> anyhow::Result<Mesh> {
		let Work {
			http,
			node,
			token,
			#[cfg(feature = "cluster-lan")]
			discovery,
		} = work;

		let commands = DialCommands::default();
		let mut mesh = Mesh {
			cluster: self.clone(),
			token,
			dialed: DialMap::default(),
			sources: kio::Tasks::new(),
			commands: commands.clone(),
		};

		for peer in &self.config.connect {
			let target = DialTarget::from_peer(peer).context("invalid --cluster-connect peer URL")?;
			if !mesh.dialed.contains(&target.key) {
				let mut dial = dialer(&mesh.cluster, &mesh.token);
				mesh.dialed.upsert(target, DialSource::Static, &mut dial);
			}
		}

		if let Some(source) = self.config.api.url.clone() {
			let commands = commands.clone();
			// Its ending is ordinary: it only stops when updates are disabled.
			mesh.sources.push(Child::new("cluster.api.url reconciler", async move {
				Self::run_connect_api(source, node, commands, http).await;
				Ok(())
			}));
		}

		#[cfg(feature = "cluster-lan")]
		if let Some(discovery) = discovery {
			// A dial ending is ordinary (that peer went away), but discovery ending is
			// the relay going blind, so `run_mdns` only returns an error.
			mesh.sources
				.push(Child::new("LAN discovery", Self::run_mdns(commands, discovery)));
		}

		Ok(mesh)
	}

	/// Dial every process that advertises itself on the LAN.
	///
	/// Each candidate is [`Peer::urls`](moq_tokio::mdns::Peer::urls) in order
	/// (node first) on `/.cluster/<credential>`, with the advertised fingerprint
	/// pinned. The lower discovery id dials; the other waits inbound. A LAN dial
	/// never carries `?jwt=`: [`Config::token`] is for static and API peers
	/// only.
	#[cfg(feature = "cluster-lan")]
	async fn run_mdns(commands: DialCommands, mut discovery: moq_tokio::mdns::Discovery) -> anyhow::Result<()> {
		use moq_tokio::mdns::Event;

		while let Some(event) = discovery.recv().await {
			match event {
				Event::Found(peer) => {
					if !discovery.should_dial(&peer.id) {
						continue;
					}
					let target = match DialTarget::from_lan_peer(&peer) {
						Ok(target) => target,
						Err(err) => {
							tracing::warn!(%err, peer = %peer.id, "LAN peer advertised nothing reachable; skipping");
							continue;
						}
					};
					let _ = commands.try_push(DialCommand::Found(target));
				}
				Event::Lost(id) => {
					let _ = commands.try_push(DialCommand::Lost(canonicalize_peer_key(&id)));
				}
				_ => continue,
			}
		}

		// Discovery only ends if the daemon or its channel died. The relay would
		// otherwise keep serving, still reporting ready, while permanently blind to
		// LAN peers. Startup refuses to come up without mDNS, so losing it later is
		// the same failure arriving late.
		anyhow::bail!("LAN discovery stopped unexpectedly");
	}

	/// Drive `--cluster-api-url`: an http(s) URL is polled, a local path (or
	/// `file://` URL) is watched for changes. Either way the source yields a JSON
	/// array of peers that's reconciled into the shared dial map.
	async fn run_connect_api(
		source: String,
		node: Option<String>,
		commands: DialCommands,
		http: Option<ClientWithMiddleware>,
	) {
		match Url::parse(&source) {
			Ok(url) if matches!(url.scheme(), "http" | "https") => {
				let http = http.expect("http(s) connect_api client built at startup");
				Self::run_connect_api_http(url, node, commands, http).await;
			}
			Ok(url) if url.scheme() == "file" => match url.to_file_path() {
				Ok(path) => Self::run_connect_api_file(path, node, commands).await,
				Err(()) => tracing::error!(%source, "cluster.api.url file URL is not a valid local path"),
			},
			// Anything that isn't a URL we recognize is treated as a filesystem path.
			_ => Self::run_connect_api_file(PathBuf::from(&source), node, commands).await,
		}
	}

	/// Poll an http(s) endpoint for the peer list on a fixed cadence
	/// ([`CONNECT_API_POLL_INTERVAL`]). Freshness is the HTTP cache middleware's job:
	/// while the cached list is still fresh, `send` is served from cache with no
	/// network round-trip; once it's stale the middleware issues a conditional GET
	/// (`ETag` / `Last-Modified`) and serves the cached body if revalidation fails.
	/// Fails static: a failed fetch logs and keeps the current dials rather than
	/// tearing the cluster down.
	async fn run_connect_api_http(url: Url, node: Option<String>, commands: DialCommands, http: ClientWithMiddleware) {
		let mut tick = kio::clock::interval(CONNECT_API_POLL_INTERVAL);
		// A slow fetch must not bank missed ticks into a catch-up burst; just resume
		// the cadence from the next whole interval.
		tick.set_missed_tick_behavior(kio::clock::MissedTickBehavior::Skip);
		loop {
			tick.tick().await;

			let mut req_url = url.clone();
			if let Some(node) = &node {
				req_url.query_pairs_mut().append_pair("node", node);
			}

			match Self::fetch_peer_list(&http, req_url).await {
				Ok(list) => Self::apply_peer_list(list, &node, &commands),
				Err(err) => tracing::warn!(%err, "cluster.api.url fetch failed; keeping current peers"),
			}
		}
	}

	/// Watch a local peer-list file, reconciling whenever it changes. Backed by
	/// [`moq_tokio::watch::Files`] (OS notifications with a polling fallback).
	/// Fails static: a missing or malformed file keeps the current dials, and the
	/// next change triggers a fresh attempt.
	async fn run_connect_api_file(path: PathBuf, node: Option<String>, commands: DialCommands) {
		Self::reload_connect_api_file(&path, &node, &commands);

		let mut watcher = match moq_tokio::watch::Files::new(std::slice::from_ref(&path)) {
			Ok(watcher) => watcher,
			Err(err) => {
				tracing::error!(%err, ?path, "failed to watch cluster.api.url file; updates disabled");
				return;
			}
		};

		loop {
			watcher.changed().await;
			Self::reload_connect_api_file(&path, &node, &commands);
		}
	}

	/// Re-read the peer-list file and reconcile. Any read/parse error keeps the
	/// current dials; the [`Files`](moq_tokio::watch::Files) only
	/// re-invokes this on a real change, so a malformed file isn't re-warned on a
	/// loop.
	fn reload_connect_api_file(path: &std::path::Path, node: &Option<String>, commands: &DialCommands) {
		match std::fs::read_to_string(path) {
			Ok(body) => match serde_json::from_str::<Vec<Peer>>(&body) {
				Ok(list) => Self::apply_peer_list(list, node, commands),
				Err(err) => {
					tracing::warn!(%err, ?path, "cluster.api.url file is not a JSON array of peers; keeping current peers")
				}
			},
			Err(err) => tracing::warn!(%err, ?path, "failed to read cluster.api.url file; keeping current peers"),
		}
	}

	/// Fetch and parse the peer list. Caching, conditional revalidation, and
	/// stale-if-error are handled by the HTTP cache middleware on `http`, so this
	/// just issues the request and parses the (possibly cache-served) body.
	async fn fetch_peer_list(http: &ClientWithMiddleware, url: Url) -> anyhow::Result<Vec<Peer>> {
		let response = http.get(url).send().await.context("cluster.api.url request failed")?;
		anyhow::ensure!(
			response.status().is_success(),
			"cluster.api.url returned {}",
			response.status()
		);
		let body = response.text().await.context("failed to read cluster.api.url body")?;

		serde_json::from_str(&body).context("cluster.api.url response is not a JSON array of peers")
	}

	/// Queue a freshly fetched peer list for the dial map to reconcile: dial peers
	/// that are new and drop API peers that disappeared. The relay's own [`node`]
	/// URL is filtered out so it never dials itself, and an invalid list queues
	/// nothing, keeping the current dials.
	fn apply_peer_list(list: Vec<Peer>, node: &Option<String>, commands: &DialCommands) {
		// Dedupe against the shared dial map (and filter out self) on stable identity,
		// while retaining the full dial configuration so a cost or credential update
		// replaces the existing session.
		let desired = match parse_peer_list(list, node.as_deref()) {
			Ok(desired) => desired,
			Err(err) => {
				tracing::warn!(%err, "invalid cluster.api.url peer list; keeping current peers");
				return;
			}
		};

		let _ = commands.try_push(DialCommand::Api(desired));
	}

	async fn supervise_remote(self, target: DialTarget, token: String) {
		let log_peer = target.key.clone();
		let dialers = self.dialers.clone();
		dialers
			.supervise(async move {
				if let Err(err) = self.run_remote(&target, token).await {
					tracing::warn!(%err, peer = %log_peer, "cluster peer connection ended");
				}
			})
			.await;
	}

	#[tracing::instrument("remote", skip_all, err, fields(remote = %target.key))]
	async fn run_remote(self, target: &DialTarget, token: String) -> anyhow::Result<()> {
		let mut urls = target.addrs();
		// Apply the shared cluster token unless the URL already carries its own
		// non-empty `?jwt=` (a per-peer inline token or object `token` wins; the
		// shared token still covers peers that have none). An empty
		// `?jwt=` counts as absent, as `moq auth serve` reads it.
		// LAN dials never get the token: they authenticate with the mDNS credential.
		if !target.lan && !token.is_empty() {
			for url in &mut urls {
				if !url.query_pairs().any(|(key, value)| key == "jwt" && !value.is_empty()) {
					url.query_pairs_mut().append_pair("jwt", &token);
				}
			}
		}

		let addrs = moq_tokio::Addrs::collect(urls).context("peer advertised no reachable address")?;

		// Sessions shorter than this are treated as churn: we keep backing off
		// instead of resetting, otherwise a peer that rejects us instantly would
		// turn into a tight reconnect loop.
		let stable_threshold = std::time::Duration::from_secs(10);

		let mut redial = Redial::default();

		loop {
			let started = kio::clock::now();
			let result = self.run_remote_once(&addrs, target).await;
			let elapsed = started.elapsed();

			match result {
				Ok(()) if elapsed >= stable_threshold => redial = Redial::default(),
				Ok(()) => tracing::warn!(?elapsed, "cluster peer session closed cleanly but quickly; backing off"),
				Err(err) => tracing::warn!(%err, "cluster peer error; will retry"),
			}

			let wait = redial.next(&mut kio::clock::rng());
			kio::clock::sleep(wait).await;
		}
	}

	/// One session to `target` over `addrs`, which carry any credential.
	async fn run_remote_once(&self, addrs: &moq_tokio::Addrs, target: &DialTarget) -> anyhow::Result<()> {
		// Each attempt is its own session, so it gets its own id. Matches the span an
		// accepted connection runs under, so both directions log the same way.
		let id = self.next_connection_id();
		// A panic fails this attempt like any other error, so the peer is redialed.
		moq_tokio::panic::isolate(self.run_remote_session(id, addrs, target))
			.instrument(tracing::info_span!("conn", id))
			.await
			.context("cluster peer session panicked")?
	}

	async fn run_remote_session(&self, id: u64, addrs: &moq_tokio::Addrs, target: &DialTarget) -> anyhow::Result<()> {
		// The peer URL may carry the cluster JWT in its query, so neither the log
		// line nor the node label below may show the raw URL.
		let first = addrs.as_slice().first().expect("Addrs is non-empty").url();
		let redacted = moq_tokio::RedactedUrl::new(first);
		tracing::info!(url = %redacted, "dialing cluster peer");

		// Each session gets its own client, so its own UDP socket and source port,
		// which closes when the session ends. A path that drops one 5-tuple (seen
		// live for a day while fresh ports to the same peer handshook) then strands
		// this peer only until the reconnect loop gives up, and the next session
		// leaves from a fresh port. A fixed `[connect]` bind port names a single
		// source port, so static and API peers share the client bound to it instead.
		let (client, _held) = match (&self.dial, target.lan) {
			(Some(shared), false) => (shared.clone(), None),
			_ => {
				let client = self.peer_client(true, Some(target))?;
				let held = self.dialers.hold(&client)?;
				(client, Some(held))
			}
		};

		// Cluster dials use their configured stats tier. Cluster peers carry no auth
		// root, so presence is keyed under the empty root within the cluster tier.
		// The peer's routes entered the cluster elsewhere, and an upstream peer is
		// never offered another upstream's. A peer that predates the hidden opt-in
		// still discovers our hidden routes; see `Cluster::scope`.
		let origin = match target.upstream {
			true => self.origin.clone().with_link(origin::Link::Upstream),
			false => self.origin.clone().with_link(origin::Link::Peer),
		};
		// The link is one presence across reconnects: each session it dials records how it
		// ended, and the last one to end decides whether the link failed.
		let stats = self.stats.tier(self.cluster_tier()).session(Path::empty());
		let mut client = client
			.with_publisher(origin.consume().with_hidden(true))
			.with_subscriber(origin)
			.with_stats(stats)
			.with_limits(self.peer_limits);
		if let Some(cost) = target.cost {
			client = client.with_cost(cost);
		}
		// The GOAWAY lifecycle lives in the reconnect loop: on an upstream GOAWAY it
		// dials the replacement while the old session keeps serving, so the old
		// routes stay attached and the origin hands live tracks over at a group
		// boundary. A peer that redirects us straight after the handshake counts as
		// a failed attempt, so an A-redirects-to-B-redirects-to-A loop escalates
		// through backoff and eventually gives up instead of migrating forever.
		// Downstream sessions never see a GOAWAY of their own.
		//
		// Forced on, whatever the template client says: a cluster peer link that
		// stopped following GOAWAY would break rolling handoff, redialing the drained
		// URL after the old session finally closed instead of migrating to the
		// replacement.
		//
		// A GOAWAY naming no URI is a relay draining to restart. A peer URL names
		// that one node, so a redial would only reach the draining process again
		// and replay both announce tables there for the rest of its window. Its
		// routes are already re-priced to the maximum, so the session serves on
		// until the node closes it, and the redial after that finds the replacement.
		let mut reconnect = client
			.with_reconnect(true)
			.with_restart(moq_tokio::Restart::Wait)
			.connect(addrs.clone());
		let mut connection = None;
		loop {
			match reconnect.status().await? {
				moq_tokio::Status::Connected if connection.is_none() => {
					connection = Some(self.nodes.connect_outbound(id, redacted.to_string()));
				}
				moq_tokio::Status::Disconnected => connection = None,
				moq_tokio::Status::Migrating => {}
				_ => {}
			}
		}
	}

	/// A peer client from the dial template, with the [`Options::peer`] QUIC
	/// limits, since every dial is a peer.
	///
	/// An `ephemeral` client is one session's alone: it binds an ephemeral port on
	/// the template's bind address, and its reconnect loop always gives up, so a
	/// redial moves to a fresh socket rather than retrying one the path may be
	/// dropping. A zero backoff timeout, which would retry the one socket forever,
	/// takes moq-tokio's default instead; the redial loop already retries forever.
	///
	/// A LAN `target` keeps only request-path versions. A `target` fingerprint is
	/// pinned on a clean TLS config, so the relay's CA roots cannot combine with it.
	fn peer_client(&self, ephemeral: bool, target: Option<&DialTarget>) -> anyhow::Result<moq_tokio::Client> {
		let mut connect = self
			.connect
			.clone()
			.context("cluster dial without a dial template (call Cluster::with_connect)")?;
		if ephemeral {
			let mut bind = connect.resolve().bind;
			bind.set_port(0);
			connect.bind = Some(bind);
			if connect.backoff.timeout().is_zero() {
				connect.backoff.timeout = None;
			}
		}
		if target.is_some_and(|target| target.lan) {
			connect.version = connect
				.versions()
				.iter()
				.filter(|version| Self::carries_request_path(version))
				.copied()
				.collect();
		}
		if let Some(fingerprint) = target.and_then(|target| target.fingerprint.as_deref()) {
			connect.tls = moq_tokio::tls::Connect::default();
			connect.tls.fingerprint = vec![fingerprint.to_string()];
		}
		let mut quic = self.quic.clone().unwrap_or_default();
		self.peer_quic.dial(&mut quic);
		let client = connect.init(quic)?;
		#[cfg(feature = "iroh")]
		let client = match &self.iroh {
			Some(iroh) => client.with_iroh(iroh.clone()),
			None => client,
		};
		Ok(client)
	}

	/// Start a reconnecting LAN session from discovered details. Tests wire this
	/// without multicast.
	#[cfg(all(test, feature = "cluster-lan"))]
	fn dial_lan_target(&self, target: &DialTarget) -> anyhow::Result<moq_tokio::Connection> {
		let addrs = moq_tokio::Addrs::collect(target.addrs()).context("peer advertised no reachable address")?;
		let mut client = self
			.peer_client(true, Some(target))?
			.with_origin(self.origin.clone().with_link(origin::Link::Peer));
		if let Some(cost) = target.cost {
			client = client.with_cost(cost);
		}
		Ok(client.connect(addrs))
	}

	#[cfg(all(test, feature = "cluster-lan"))]
	fn set_lan_credential(&self, credential: impl Into<String>) {
		let _ = self.lan_auth.set(LanAuth {
			credential: credential.into(),
		});
	}
}

/// Advertise this listener on the LAN: fingerprint when the certificate was
/// generated, node URL when configured, secret when shared.
#[cfg(feature = "cluster-lan")]
async fn lan_discovery(
	advertise: &LanAdvertise,
	node: Option<&str>,
	secret: Option<&std::path::Path>,
	app: moq_tokio::mdns::App,
) -> anyhow::Result<moq_tokio::mdns::Discovery> {
	let mut config = moq_tokio::mdns::Config::new(app, advertise.port);
	if let Some(fingerprint) = &advertise.fingerprint {
		config = config.with_fingerprint(fingerprint.clone());
	}
	if let Some(node) = node {
		let url = peer_url(node)?;
		// The advertisement is multicast in the clear. The secret authenticates the
		// record, it does not hide it, so anything in the query is handed to every
		// listener on the network.
		//
		// An allowlist rather than a `jwt` denylist: `cost` is the only query param a
		// peer needs off the advertised URL, and listing what may go out means the
		// next credential-bearing param is refused the day it is added instead of
		// leaking until someone remembers to ban it.
		let published: Vec<String> = url
			.query_pairs()
			.map(|(key, _)| key.into_owned())
			.filter(|key| key != "cost")
			.collect();
		anyhow::ensure!(
			published.is_empty(),
			"`--cluster-node` carries query parameters that `--cluster-lan` would broadcast in the clear ({}). \
			 Only `?cost=` may be advertised; pass credentials with `--cluster-token` instead.",
			published.join(", ")
		);
		config = config.with_node(url);
	}
	if let Some(secret) = secret {
		let secret = moq_tokio::secret::Secret::read(secret).context("invalid --cluster-lan-secret")?;
		config = config.with_secret(secret);
	}
	Ok(config.advertise().await?)
}

/// Extract and remove the `cost` query param from a peer URL.
///
/// The param is dial-side configuration, not something the peer reads off the
/// URL (it rides SETUP instead), so it is stripped before connecting. An
/// unparseable value is an error rather than a silent default: a mispriced link
/// skews routing for every broadcast crossing it.
fn take_cost(url: &mut Url) -> anyhow::Result<Option<u64>> {
	let Some(value) = url
		.query_pairs()
		.find_map(|(key, value)| (key == "cost").then(|| value.into_owned()))
	else {
		return Ok(None);
	};
	let cost: u64 = value
		.parse()
		.with_context(|| format!("invalid cost {value:?} on cluster peer URL"))?;

	let remaining: Vec<(String, String)> = url
		.query_pairs()
		.filter(|(key, _)| key != "cost")
		.map(|(key, value)| (key.into_owned(), value.into_owned()))
		.collect();
	url.set_query(None);
	if !remaining.is_empty() {
		let mut pairs = url.query_pairs_mut();
		for (key, value) in &remaining {
			pairs.append_pair(key, value);
		}
	}

	Ok(Some(cost))
}

/// Drop `?jwt=` so a LAN dial never presents [`Config::token`].
#[cfg(feature = "cluster-lan")]
fn strip_jwt(url: &mut Url) {
	let remaining: Vec<(String, String)> = url
		.query_pairs()
		.filter(|(key, _)| key != "jwt")
		.map(|(key, value)| (key.into_owned(), value.into_owned()))
		.collect();
	url.set_query(None);
	if !remaining.is_empty() {
		let mut pairs = url.query_pairs_mut();
		for (key, value) in &remaining {
			pairs.append_pair(key, value);
		}
	}
}

/// Whether a `--cluster-api-url` source is an http(s) URL (otherwise it's
/// treated as a local file path, which needs no TLS client).
fn connect_api_is_http(source: &str) -> anyhow::Result<bool> {
	let Ok(url) = Url::parse(source) else { return Ok(false) };
	if url.scheme() == "http" {
		let loopback = match url.host() {
			Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
			Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
			Some(url::Host::Domain(host)) => host == "localhost",
			None => false,
		};
		anyhow::ensure!(loopback, "--cluster-api-url requires HTTPS except on loopback");
	}
	Ok(matches!(url.scheme(), "http" | "https"))
}

/// Resolve a cluster peer to the URL we dial.
///
/// The modern form is a full URL, e.g. `https://host/?jwt=TOKEN`, which is used
/// verbatim. A bare host or `host:port` is wrapped in `https://.../` for
/// `--cluster-api-url` lists; user-supplied `--cluster-connect` entries
/// refuse that form through [`Config::deprecated`].
fn peer_url(peer: &str) -> anyhow::Result<Url> {
	// A full URL has a scheme separator; a bare host or `host:port` does not
	// (and `Url::parse` would otherwise mis-read `host:port` as scheme `host`).
	if peer.contains("://") {
		return Url::parse(peer).context("invalid cluster peer URL");
	}

	Url::parse(&format!("https://{peer}/")).context("invalid cluster peer host")
}

/// Whether a peer string uses the released bare-host / `host:port` form rather
/// than a full URL. Used so [`Config::deprecated`] can refuse
/// `--cluster-connect` entries that still use it.
fn is_legacy_peer(peer: &str) -> bool {
	!peer.contains("://")
}

/// Canonical dedupe key for a cluster peer, so the same relay reached via
/// different spellings (a full URL vs a bare `host:port`, with or without an
/// inline `?jwt=`) shares one [`DialMap`] entry instead of opening a duplicate
/// session. Drops the query (the jwt isn't part of a peer's identity) and lets
/// `Url` normalize the scheme, host case, and default port. Falls back to the
/// raw string if the peer can't be parsed.
///
/// It has to absorb every normalization a discovery path applies on the way in,
/// or one relay gets two keys and is dialed twice.
pub(crate) fn canonicalize_peer_key(peer: &str) -> String {
	match peer_url(peer) {
		Ok(mut url) => {
			url.set_query(None);
			// mDNS strips these before advertising, so a key that kept them would
			// give one relay two identities.
			url.set_fragment(None);
			url.set_username("").ok();
			url.set_password(None).ok();
			// Trim and collapse path slashes: a non-special scheme like `moqt` keeps
			// its trailing slash otherwise, and `moqt://a.example:4443/` is an
			// ordinary way to spell the same node as `moqt://a.example:4443`.
			let segments: Vec<&str> = url.path().split('/').filter(|s| !s.is_empty()).collect();
			let path = match segments.is_empty() {
				true => String::new(),
				false => format!("/{}", segments.join("/")),
			};
			url.set_path(&path);
			url.into()
		}
		Err(_) => peer.to_string(),
	}
}

/// The wait between dials of one cluster peer: exponential with full jitter, so a
/// mesh recovering from one outage spreads its redials instead of sending them in
/// lockstep, and capped low enough that a peer back from a long outage is picked
/// up within tens of seconds.
struct Redial {
	/// The longest the next wait may be.
	ceiling: std::time::Duration,
}

impl Redial {
	const FIRST: std::time::Duration = std::time::Duration::from_secs(1);
	const CAP: std::time::Duration = std::time::Duration::from_secs(30);

	/// A wait drawn uniformly below the ceiling, which then doubles up to [`Self::CAP`].
	fn next(&mut self, rng: &mut impl rand::RngExt) -> std::time::Duration {
		let wait = self.ceiling.mul_f64(rng.random());
		self.ceiling = (self.ceiling * 2).min(Self::CAP);
		wait
	}
}

impl Default for Redial {
	fn default() -> Self {
		Self { ceiling: Self::FIRST }
	}
}

#[cfg(test)]
impl Driver {
	/// How many of its tasks are still running.
	pub(crate) fn len(&self) -> usize {
		self.work.len()
	}

	/// Add a task named `name` that panics on its first poll.
	pub(crate) fn inject_panic(&mut self, name: &'static str) {
		self.work.push(Child::new(name, async { panic!("injected") }));
	}
}

#[cfg(test)]
impl Cluster {
	/// [`Self::new`] with its [`Driver`] spawned onto the ambient runtime, the way a
	/// test edge runs it. A failed driver panics its task.
	pub(crate) fn spawn(options: Options) -> anyhow::Result<Self> {
		let (cluster, driver) = Self::new(options)?;
		tokio::spawn(async move {
			if let Err(err) = driver.run().await {
				panic!("cluster driver failed: {err:#}");
			}
		});
		Ok(cluster)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::Config as RelayConfig;

	#[tokio::test]
	async fn session_limits_follow_the_admitted_grant() {
		let mut options = Options::default();
		options.session.announces = 2;
		options.peer.announces = 20;
		let cluster = Cluster::spawn(options).unwrap();
		for (peer, upstream, expected) in [(false, false, 2), (true, false, 20), (true, true, 20)] {
			let mut grant = moq_auth::Grant::new(moq_auth::Patterns::new(), moq_auth::Patterns::new());
			grant.peer = peer;
			grant.upstream = upstream;
			let (_producer, consumer) = moq_auth::lease::Producer::new(grant);
			// A cluster-looking path alone never selects the peer policy.
			let lease = auth::Lease::new("/.cluster/untrusted", consumer).unwrap();
			let limits = cluster.limits(&lease);
			assert_eq!(limits.announces, expected);
		}
	}

	/// A verified certificate is a fact for the auth server: only a `peer` grant
	/// discovers hidden routes without asking, on both scoping paths.
	#[tokio::test]
	async fn only_a_peer_grant_discovers_hidden_routes() {
		let cluster = new_cluster(Config::default()).expect("cluster");
		let _hidden = cluster.origin.publish(".stats/node", origin::Route::default());
		let _visible = cluster.origin.publish("room", origin::Route::default());
		let mut request = moq_auth::Request::new("test", moq_auth::Transport::Quic, "/");
		request.tls = Some(moq_auth::Peer {
			name: "edge0".into(),
			fingerprint: "ab".repeat(32),
			expires: None,
			issuer: "CN=cluster".into(),
		});
		for peer in [false, true] {
			for session in [false, true] {
				let mut grant = moq_auth::Grant::new(moq_auth::Pattern::all(), moq_auth::Pattern::all());
				grant.peer = peer;
				let lease = auth::Lease::new("/", moq_auth::lease::Consumer::fixed(grant)).unwrap();
				let admitted = match session {
					true => cluster.scope_session(lease, &request),
					false => cluster.scope(lease, &request),
				}
				.expect("admitted");
				let mut announced = admitted.subscriber.expect("subscribe grant").announced();
				let mut seen = Vec::new();
				while let Some(route) = try_next_announced(&mut announced) {
					seen.push(route.prefix.to_string());
				}
				let expected: &[&str] = if peer { &[".stats/node", "room"] } else { &["room"] };
				assert_eq!(seen, expected, "peer={peer}, session={session}");
			}
		}
	}

	/// A millisecond track: the frames written here carry timestamps.
	fn timed() -> moq_net::track::Info {
		moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)
	}

	/// The next route and whether it is active.
	#[cfg(feature = "cluster-lan")]
	async fn next_update(announced: &mut moq_net::announce::Consumer) -> Option<(moq_net::announce::Announce, bool)> {
		match announced.next().await? {
			moq_net::announce::Event::Start(route)
			| moq_net::announce::Event::Update(route)
			| moq_net::announce::Event::Restart(route)
			| moq_net::announce::Event::Takeover { announce: route, .. } => Some((route, true)),
			moq_net::announce::Event::End(route) => Some((route, false)),
		}
	}

	/// The next announcement without blocking.
	fn try_next_announced(announced: &mut moq_net::announce::Consumer) -> Option<moq_net::announce::Announce> {
		match announced.try_next()? {
			moq_net::announce::Event::Start(route) => Some(route),
			other => panic!("expected an announcement: got {other:?}"),
		}
	}

	fn new_cluster(config: Config) -> anyhow::Result<Cluster> {
		Cluster::spawn(Options::new(config))
	}

	/// A grant's mount reaches the target for subscribe and refuses publish, both
	/// named from the session's root.
	#[tokio::test]
	async fn session_handles_apply_grant_mounts() {
		let cluster = new_cluster(Config::default()).expect("cluster");
		let everything = || moq_net::Patterns::from(moq_net::Pattern::all());
		let mut grant = moq_auth::Grant::new(everything(), everything());
		grant.mounts.insert(".svc".into(), ".svc/pid".into());
		let token = auth::Token::new("/pid", &grant).unwrap();

		let _worker = cluster
			.origin
			.publish(".svc/pid/foo", origin::Route::default())
			.expect("publish at the fleet path");
		let subscriber = cluster.subscriber(&token).expect("subscribe grant").consume();
		let broadcast = subscriber
			.request(".svc/foo", None)
			.await
			.expect("resolves through the mount");
		assert_eq!(broadcast.info().path.as_str(), ".svc/foo");

		let publisher = cluster.publisher(&token).expect("publish grant");
		assert!(publisher.create_broadcast(".svc/foo").is_err());
		publisher.create_broadcast("cam").expect("publish outside the mount");

		// A mount the origin refuses admits nothing rather than half a grant.
		grant.mounts.insert(".svc/x".into(), ".other".into());
		let token = auth::Token::new("/pid", &grant).unwrap();
		assert!(cluster.subscriber(&token).is_none());
		assert!(cluster.publisher(&token).is_none());
	}

	/// A grant whose mounts chain, or whose mount point is a wildcard, admits
	/// nothing. Mounts apply in key order, so the chain is written with the
	/// mount point on the target sorting last.
	#[tokio::test]
	async fn session_handles_refuse_invalid_grant_mounts() {
		let cluster = new_cluster(Config::default()).expect("cluster");
		let everything = || moq_net::Patterns::from(moq_net::Pattern::all());
		let invalid: [&[(&str, &str)]; 2] = [&[(".a", "pid/.z"), (".z", "secret")], &[("*", ".svc/pid")]];
		for mounts in invalid {
			let mut grant = moq_auth::Grant::new(everything(), everything());
			for (at, target) in mounts {
				grant.mounts.insert((*at).into(), (*target).into());
			}
			let token = auth::Token::new("/pid", &grant).unwrap();
			assert!(cluster.subscriber(&token).is_none(), "{mounts:?}");
			assert!(cluster.publisher(&token).is_none(), "{mounts:?}");
		}
	}

	/// The publish task holds only a `Weak` to its producer, so it stops when the
	/// last `moq_stats::Producer` clone drops. Attaching one must therefore hand
	/// its lifetime to the cluster: an embedder clones handles off a `Relay` and
	/// does not have to keep the producer, and a relay that keeps serving while
	/// silently publishing nothing is exactly the class of failure this API
	/// exists to rule out. Moving the ONLY handle in must keep it alive.
	///
	/// Asserts the broadcast is still live AFTER a few publish intervals, not
	/// merely that one appeared: the task publishes once before it can notice its
	/// producer is gone, so a first announcement races through either way and
	/// proves nothing.
	#[tokio::test(start_paused = true)]
	async fn stats_publishing_outlives_the_producer_handle() {
		let config = crate::stats::Config {
			enabled: true,
			node: Some("test".to_string()),
			..Default::default()
		};

		let cluster = new_cluster(Config::default()).expect("cluster");
		let (stats, publisher) = config.build(cluster.origin.clone()).expect("stats");
		let cluster = cluster.with_stats(stats, publisher);

		let path = moq_net::Path::from_static(".stats/node/test");
		let consumer = cluster.origin.consume();
		let broadcast = kio::clock::timeout(
			std::time::Duration::from_secs(5),
			consumer.follow(&path, None).unwrap().available(),
		)
		.await
		.expect("stats broadcast announced within 5s")
		.expect("stats broadcast resolves");
		assert!(broadcast.info().instance.is_some(), "stats announce under an instance");

		// Several publish intervals (1s each by default) after the handle went out
		// of scope, the task is still running and its broadcast still announced: a
		// request refuses a front whose route retracted.
		kio::clock::sleep(std::time::Duration::from_millis(2500)).await;
		let still_live = consumer.request(&path, None).await;
		assert!(
			still_live.is_ok(),
			"stats broadcast unannounced after the producer handle was dropped: \
			 the publish task stopped while the cluster kept serving"
		);
		drop(broadcast);
	}

	/// Each wait is drawn across the whole range below a ceiling that doubles to the
	/// cap, so peers that failed together spread out instead of redialing in step.
	#[test]
	fn redial_waits_spread_below_a_doubling_capped_ceiling() {
		use rand::SeedableRng;
		let mut rng = rand::rngs::StdRng::seed_from_u64(0x7265_6469_616c);
		let ceilings = [1, 2, 4, 8, 16, 30, 30].map(std::time::Duration::from_secs);
		let mut redials: Vec<Redial> = (0..1000).map(|_| Redial::default()).collect();
		for ceiling in ceilings {
			let waits: Vec<_> = redials.iter_mut().map(|redial| redial.next(&mut rng)).collect();
			assert!(waits.iter().all(|wait| *wait <= ceiling), "a wait passed {ceiling:?}");
			// Uniform over the range: every tenth of it is hit, so none is a lockstep.
			for tenth in 0..10 {
				let (low, high) = (ceiling * tenth / 10, ceiling * (tenth + 1) / 10);
				assert!(
					waits.iter().any(|wait| (low..high).contains(wait)),
					"no wait in {low:?}..{high:?} below {ceiling:?}"
				);
			}
		}
	}

	/// `?cost=` is read and stripped (it rides SETUP, not the URL), other
	/// query params survive, and a garbage value is an error rather than a
	/// silent default.
	#[test]
	fn cost_param_is_consumed() {
		let mut url = Url::parse("https://peer.example/?jwt=abc&cost=0").unwrap();
		assert_eq!(take_cost(&mut url).unwrap(), Some(0));
		assert_eq!(url.as_str(), "https://peer.example/?jwt=abc");

		let mut url = Url::parse("https://peer.example/").unwrap();
		assert_eq!(take_cost(&mut url).unwrap(), None);
		assert_eq!(url.as_str(), "https://peer.example/");

		let mut url = Url::parse("https://peer.example/?cost=cheap").unwrap();
		assert!(take_cost(&mut url).is_err());
	}

	#[tokio::test]
	async fn cluster_tier_defaults_to_unprefixed() {
		let cluster = new_cluster(Config::default()).expect("cluster");
		assert_eq!(cluster.cluster_tier(), Tier::default());

		let cluster = new_cluster(Config {
			tier: Some("region/sjc".to_string()),
			..Default::default()
		})
		.expect("cluster");
		assert_eq!(
			cluster.cluster_tier(),
			Tier::new(moq_net::Path::from_static("region/sjc"))
		);

		let quoted = new_cluster(Config {
			tier: Some("region\"".to_string()),
			..Default::default()
		});
		let err = quoted.err().expect("a tier a metrics label would escape is refused");
		assert!(format!("{err:#}").contains("--cluster-tier"), "{err:#}");
	}

	/// Stand-in dial task: never makes progress.
	type Placeholder = fn(&kio::Waiter) -> Poll<()>;

	fn placeholder(_: &kio::Waiter) -> Poll<()> {
		Poll::Pending
	}

	/// Dials nothing: fails the test if the dial map starts a dial.
	fn no_dial(target: DialTarget) -> Placeholder {
		panic!("must not dial {}", target.key)
	}

	/// Records each dial the map starts.
	fn recording(dialed: &mut Vec<DialTarget>) -> impl FnMut(DialTarget) -> Placeholder + '_ {
		|target| {
			dialed.push(target);
			placeholder
		}
	}

	fn target(key: &str) -> DialTarget {
		DialTarget {
			key: key.to_string(),
			url: Url::parse(&format!("https://{key}/")).expect("test URL"),
			urls: Vec::new(),
			cost: None,
			fingerprint: None,
			lan: false,
			upstream: false,
		}
	}

	fn desired(keys: &[&str]) -> HashMap<String, DialTarget> {
		keys.iter().map(|key| ((*key).to_string(), target(key))).collect()
	}

	/// Seed a dial for `target` under `source`.
	fn seeded(dialed: &mut DialMap<Placeholder>, target: DialTarget, source: DialSource) {
		dialed.upsert(target, source, &mut |_| placeholder);
	}

	/// A peer wanted by both a static seed and the API survives losing either
	/// source: the dial is only torn down once the last source releases it.
	#[test]
	fn multi_source_peer_survives_until_last_release() {
		let mut dialed = DialMap::default();
		// Seeded first, then also appears in the API list.
		seeded(&mut dialed, target("both:4443"), DialSource::Static);
		dialed.reconcile_api(&desired(&["both:4443"]), &mut no_dial);

		// Dropped from the API list -> still wanted by the seed.
		dialed.reconcile_api(&HashMap::new(), &mut no_dial);
		assert!(dialed.contains("both:4443"), "the seed still wants it");
		assert_eq!(dialed.tasks.len(), 1);

		dialed.release("both:4443", DialSource::Static, &mut no_dial);
		assert!(!dialed.contains("both:4443"));
		assert!(dialed.tasks.is_empty(), "the abandoned dial is cancelled");
	}

	/// A second source for an already-dialed peer merges onto the existing entry
	/// rather than opening a second dial.
	#[test]
	fn upsert_merges_redundant_dial() {
		let mut dialed = DialMap::default();
		seeded(&mut dialed, target("p:4443"), DialSource::Static);
		dialed.upsert(target("p:4443"), DialSource::Api, &mut no_dial);
		assert_eq!(dialed.tasks.len(), 1);

		// Dropping the API source leaves the static source holding the dial.
		dialed.reconcile_api(&HashMap::new(), &mut no_dial);
		assert!(dialed.contains("p:4443"), "static source still holds the dial");
	}

	/// `reconcile_api` drops API dials missing from the desired set, dials the
	/// newly desired ones, and never touches Static dials (even when they're absent
	/// from the API list).
	#[test]
	fn reconcile_api_adds_and_removes_only_api() {
		let mut dialed = DialMap::default();
		seeded(&mut dialed, target("static:4443"), DialSource::Static);
		seeded(&mut dialed, target("api-keep:4443"), DialSource::Api);
		seeded(&mut dialed, target("api-drop:4443"), DialSource::Api);

		// Desired: keep one existing API peer, drop the other, add a new one.
		// The static peer is not in the list but must survive.
		let mut added = Vec::new();
		dialed.reconcile_api(&desired(&["api-keep:4443", "api-new:4443"]), &mut recording(&mut added));

		let added: Vec<_> = added.into_iter().map(|target| target.key).collect();
		assert_eq!(added, ["api-new:4443"]);
		assert!(dialed.contains("api-keep:4443"));
		assert!(!dialed.contains("api-drop:4443"), "dropped API peer must be removed");
		assert!(dialed.contains("static:4443"), "static peer must survive reconcile");
		assert_eq!(dialed.tasks.len(), 3, "the dropped dial is cancelled");
	}

	/// A peer already dialed via another source is not dialed again, so the API
	/// reconcile can't open a duplicate connection.
	#[test]
	fn reconcile_api_dedupes_against_other_sources() {
		let mut dialed = DialMap::default();
		seeded(&mut dialed, target("shared:4443"), DialSource::Static);

		dialed.reconcile_api(&desired(&["shared:4443"]), &mut no_dial);
		assert!(dialed.contains("shared:4443"));
	}

	/// A secondary source can update its target without disturbing the source that
	/// opened the session. If the active source disappears, its latest fallback
	/// configuration is used for the replacement.
	#[test]
	fn inactive_source_update_applies_on_takeover() {
		let mut dialed = DialMap::default();
		let seed = DialTarget::parse("https://peer.example/?cost=1").unwrap();
		let api = DialTarget::parse("https://peer.example/?cost=3").unwrap();
		seeded(&mut dialed, seed.clone(), DialSource::Static);
		let old = dialed.entries[&seed.key].task;

		let desired = [(api.key.clone(), api.clone())].into_iter().collect();
		dialed.reconcile_api(&desired, &mut no_dial);
		assert_eq!(dialed.entries[&seed.key].task, old, "inactive source must not redial");

		let mut spawned = Vec::new();
		dialed.release(&seed.key, DialSource::Static, &mut recording(&mut spawned));

		assert!(dialed.tasks.remove(old).is_none(), "old source must be cancelled");
		assert_eq!(dialed.tasks.len(), 1);
		assert_eq!(spawned.len(), 1);
		assert!(spawned[0] == api);
	}

	/// If the fallback source requests the same target, ownership transfers without
	/// interrupting the healthy session.
	#[test]
	fn identical_fallback_takeover_keeps_task() {
		let mut dialed = DialMap::default();
		let target = DialTarget::parse("https://peer.example/?cost=1").unwrap();
		seeded(&mut dialed, target.clone(), DialSource::Static);
		let task = dialed.entries[&target.key].task;

		let desired = [(target.key.clone(), target.clone())].into_iter().collect();
		dialed.reconcile_api(&desired, &mut no_dial);
		dialed.release(&target.key, DialSource::Static, &mut no_dial);

		let entry = dialed.entries.get(&target.key).expect("current dial");
		assert_eq!(entry.task, task, "healthy dial must be preserved");
		assert_eq!(entry.active, DialSource::Api);
		assert!(entry.sources.get(entry.active) == Some(&target));
		assert_eq!(dialed.tasks.len(), 1);
	}

	/// Replacing `old` with `new` through the API redials exactly once and cancels
	/// the old dial.
	fn assert_api_replaces(old: DialTarget, new: DialTarget) {
		assert_eq!(old.key, new.key);
		let mut dialed = DialMap::default();
		seeded(&mut dialed, old.clone(), DialSource::Api);
		let old_task = dialed.entries[&old.key].task;

		let desired = [(new.key.clone(), new.clone())].into_iter().collect();
		let mut spawned = Vec::new();
		dialed.reconcile_api(&desired, &mut recording(&mut spawned));

		assert!(dialed.tasks.remove(old_task).is_none(), "old dial must be cancelled");
		assert_eq!(dialed.tasks.len(), 1);
		assert_eq!(spawned.len(), 1);
		assert!(spawned[0] == new);
	}

	/// A cost-only API update has the same identity but different SETUP input, so
	/// it must replace the live task instead of being mistaken for an unchanged
	/// peer.
	#[test]
	fn reconcile_api_replaces_cost_only_change() {
		let old = DialTarget::parse("https://peer.example/?cost=1").unwrap();
		let new = DialTarget::parse("https://peer.example/?cost=2").unwrap();
		assert!(old != new);
		assert_api_replaces(old, new);
	}

	/// An identical API render keeps the live task, avoiding connection churn.
	#[test]
	fn reconcile_api_identical_target_is_noop() {
		let mut dialed = DialMap::default();
		let target = DialTarget::parse("https://peer.example/?cost=2&jwt=secret").unwrap();
		seeded(&mut dialed, target.clone(), DialSource::Api);
		let task = dialed.entries[&target.key].task;
		let desired = [(target.key.clone(), target.clone())].into_iter().collect();

		dialed.reconcile_api(&desired, &mut no_dial);
		assert_eq!(dialed.entries[&target.key].task, task, "live dial must be preserved");
		assert_eq!(dialed.tasks.len(), 1);
	}

	/// Inline credentials are dial-affecting even though they are excluded from
	/// peer identity, so rotating one replaces the session too.
	#[test]
	fn reconcile_api_replaces_inline_credential() {
		assert_api_replaces(
			DialTarget::parse("https://peer.example/?jwt=old").unwrap(),
			DialTarget::parse("https://peer.example/?jwt=new").unwrap(),
		);
	}

	/// A malformed replacement is rejected before reconciliation, so it cannot
	/// tear down or reconfigure the last-known-good dial set.
	#[test]
	fn malformed_peer_list_preserves_current_dial() {
		let commands = DialCommands::default();
		Cluster::apply_peer_list(vec![Peer::new("https://peer.example/?cost=invalid")], &None, &commands);
		assert!(commands.is_empty(), "an invalid list must not reach the dial map");

		Cluster::apply_peer_list(vec![Peer::new("https://peer.example/?cost=1")], &None, &commands);
		assert_eq!(commands.len(), 1, "a valid list is queued");
	}

	/// The same identity cannot appear twice with different SETUP inputs because
	/// input ordering must not choose which configuration wins.
	#[test]
	fn peer_list_rejects_conflicting_duplicate() {
		let Err(err) = parse_peer_list(
			vec![
				Peer::new("https://peer.example/?cost=1"),
				Peer::new("https://peer.example/?cost=2"),
			],
			None,
		) else {
			panic!("conflicting duplicate must fail");
		};
		assert!(format!("{err:#}").contains("conflicting configurations"));
	}

	/// The peer-list wire format is a JSON array of bare URL strings and/or
	/// objects, parsed by the same type static config uses.
	#[test]
	fn peer_list_parses_as_string_array() {
		let body = r#"["a.pop.example", "b.pop.example:4443"]"#;
		let list: Vec<Peer> = serde_json::from_str(body).expect("parse peer list");
		assert_eq!(list, vec![Peer::new("a.pop.example"), Peer::new("b.pop.example:4443")]);
	}

	/// A bare URL keeps working, and an equivalent object normalizes to the same
	/// dial target, so the two forms dedupe instead of conflicting.
	#[test]
	fn peer_object_form_matches_bare_url() {
		let from_url = DialTarget::from_peer(&Peer::new("https://peer.example/?cost=2")).unwrap();
		let object: Peer =
			serde_json::from_str(r#"{"url": "https://peer.example/", "cost": 2}"#).expect("parse object peer");
		let from_object = DialTarget::from_peer(&object).unwrap();
		assert_eq!(from_url, from_object);

		let desired = parse_peer_list(vec![Peer::new("https://peer.example/?cost=2"), object], None)
			.expect("equivalent forms dedupe");
		assert_eq!(desired.len(), 1);
		assert_eq!(desired.values().next().expect("one peer"), &from_url);
	}

	/// An object `token` rides the same inline credential path: the dial URL
	/// carries `?jwt=`, the identity drops it, and the shared token does not
	/// override it.
	#[test]
	fn peer_object_token_matches_inline_credential() {
		let inline = DialTarget::from_peer(&Peer::new("https://peer.example/?jwt=secret")).unwrap();
		let object: Peer =
			serde_json::from_str(r#"{"url": "https://peer.example/", "token": "secret"}"#).expect("parse object peer");
		let from_object = DialTarget::from_peer(&object).unwrap();
		assert_eq!(inline, from_object);
		assert_eq!(inline.key, "https://peer.example/");
		assert!(
			inline
				.url
				.query_pairs()
				.any(|(key, value)| key == "jwt" && value == "secret"),
			"the credential must reach the dial URL: {}",
			inline.url
		);
	}

	/// `upstream` is object-only policy: it round-trips, reaches the dial target,
	/// and flipping it is a different configuration, so the API redials.
	#[test]
	fn peer_object_upstream() {
		let object: Peer =
			serde_json::from_str(r#"{"url": "https://core.example/", "upstream": true}"#).expect("parse upstream peer");
		assert_eq!(object, Peer::new("https://core.example/").with_upstream(true));
		assert_eq!(
			serde_json::to_string(&object).unwrap(),
			r#"{"url":"https://core.example/","upstream":true}"#
		);
		let upstream = DialTarget::from_peer(&object).unwrap();
		assert!(upstream.upstream);
		let plain = DialTarget::from_peer(&Peer::new("https://core.example/")).unwrap();
		assert_eq!(upstream.key, plain.key);
		assert_ne!(upstream, plain);
		assert_eq!(
			serde_json::to_string(&Peer::new("https://core.example/")).unwrap(),
			r#""https://core.example/""#
		);
	}

	/// `egress` defaults to `cost`, so a matching value is accepted and prices
	/// the link exactly like the bare URL form. An unpriced link costs 1, so
	/// `egress = 1` alone is symmetric too.
	#[test]
	fn peer_symmetric_egress_is_supported() {
		let cost_only = DialTarget::from_peer(&Peer::new("https://peer.example/?cost=2")).unwrap();
		let symmetric: Peer = serde_json::from_str(r#"{"url": "https://peer.example/", "cost": 2, "egress": 2}"#)
			.expect("parse symmetric peer");
		assert_eq!(DialTarget::from_peer(&symmetric).unwrap(), cost_only);

		let unpriced = DialTarget::from_peer(&Peer::new("https://peer.example/")).unwrap();
		let default_egress = DialTarget::from_peer(&Peer::new("https://peer.example/").with_egress(1)).unwrap();
		assert_eq!(default_egress, unpriced);
	}

	/// An `egress` that differs from the effective cost is refused, never
	/// silently ignored. That includes an egress with no cost at all.
	#[test]
	fn peer_asymmetric_egress_is_rejected() {
		for peer in [
			Peer::new("https://peer.example/").with_cost(1).with_egress(2),
			Peer::new("https://peer.example/").with_egress(2),
		] {
			let err = DialTarget::from_peer(&peer).expect_err("asymmetric egress must fail");
			assert!(
				format!("{err:#}").contains("not supported"),
				"egress refusal must say so: {err:#}"
			);
		}

		let Err(err) = parse_peer_list(
			vec![Peer::new("https://peer.example/").with_cost(1).with_egress(2)],
			None,
		) else {
			panic!("asymmetric egress must reject the list");
		};
		assert!(format!("{err:#}").contains("not supported"));
	}

	/// Object policy and URL params each get one home. Setting both is rejected
	/// rather than given a precedence a migration could silently get wrong.
	#[test]
	fn peer_mixed_policy_is_rejected() {
		let cost_mix: Peer =
			serde_json::from_str(r#"{"url": "https://peer.example/?cost=1", "cost": 1}"#).expect("parse mixed peer");
		assert!(DialTarget::from_peer(&cost_mix).is_err());

		let token_mix: Peer = serde_json::from_str(r#"{"url": "https://peer.example/?jwt=secret", "token": "secret"}"#)
			.expect("parse mixed peer");
		assert!(DialTarget::from_peer(&token_mix).is_err());
	}

	/// Unknown object fields reject the entry, so a typo keeps the last-good
	/// topology instead of dialing with a silently dropped policy. The error
	/// names the field rather than a generic shape mismatch.
	#[test]
	fn peer_unknown_field_is_rejected() {
		let err = serde_json::from_str::<Vec<Peer>>(r#"[{"url": "https://peer.example/", "cosst": 1}]"#)
			.expect_err("unknown field must fail");
		assert!(err.to_string().contains("cosst"), "error must name the field: {err}");

		#[derive(Debug, serde::Deserialize)]
		struct Doc {
			#[allow(dead_code, reason = "the test only checks that deserializing fails")]
			connect: Vec<Peer>,
		}
		let err = toml::from_str::<Doc>("connect = [{ url = \"https://peer.example/\", cost = \"cheap\" }]")
			.expect_err("wrong type must fail");
		assert!(err.to_string().contains("cost"), "error must name the field: {err}");
	}

	/// Equivalent URL and object forms of one peer dedupe, while differing
	/// policies for one identity conflict, whatever their forms.
	#[test]
	fn peer_list_mixed_forms_conflict_on_policy() {
		let object: Peer =
			serde_json::from_str(r#"{"url": "https://peer.example/", "cost": 3}"#).expect("parse object peer");
		let Err(err) = parse_peer_list(vec![Peer::new("https://peer.example/?cost=1"), object], None) else {
			panic!("differing policies must conflict");
		};
		assert!(format!("{err:#}").contains("conflicting configurations"));
	}

	/// `Config::load` traces the whole resolved config, so `Peer` redacts its
	/// credential from `Debug` instead of printing it into logs.
	#[test]
	fn peer_debug_redacts_token() {
		for peer in [
			Peer::new("https://peer.example/").with_cost(2).with_token("secret"),
			Peer::new("https://user:secret@peer.example/?jwt=secret#secret"),
			Peer::new("https://peer.example:bad/?jwt=secret"),
		] {
			let debug = format!("{peer:?}");
			assert!(!debug.contains("secret"), "Debug must not leak the credential: {debug}");
			if peer_url(peer.url()).is_ok() {
				assert!(
					debug.contains("https://peer.example/"),
					"Debug keeps the address: {debug}"
				);
			}
		}
	}

	/// Static `--cluster-connect` entries get the same all-or-nothing
	/// validation as `--cluster-api-url` lists: a repeated identity with
	/// conflicting policy fails construction instead of silently keeping the
	/// first entry, while equivalent entries dedupe.
	#[tokio::test]
	async fn static_conflicting_peers_fail_at_construction() {
		let conflicting = Config {
			connect: vec![
				Peer::new("https://peer.example/?cost=1"),
				Peer::new("https://peer.example/?cost=2"),
			],
			..Default::default()
		};
		let Err(err) = new_cluster(conflicting) else {
			panic!("conflicting static peers must fail");
		};
		assert!(
			format!("{err:#}").contains("conflicting configurations"),
			"refusal must say so: {err:#}"
		);

		let equivalent = Config {
			connect: vec![
				Peer::new("https://peer.example/?cost=2"),
				serde_json::from_str(r#"{"url": "https://peer.example/", "cost": 2}"#).expect("parse object peer"),
			],
			..Default::default()
		};
		new_cluster(equivalent).expect("equivalent static peers dedupe");
	}

	fn test_mesh() -> (Mesh, DialCommands) {
		let commands = DialCommands::default();
		let mesh = Mesh {
			cluster: new_cluster(Config::default()).expect("cluster"),
			token: String::new(),
			dialed: DialMap::default(),
			sources: kio::Tasks::new(),
			commands: commands.clone(),
		};
		(mesh, commands)
	}

	/// A peer source feeds the dial set through the mesh's queue, on the mesh's own
	/// poll, and a source failing ends the mesh with its error.
	#[tokio::test]
	async fn mesh_dials_what_a_source_queues_and_fails_with_it() {
		let (mut mesh, commands) = test_mesh();
		let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
		mesh.sources.push(Child::new("test source", async move {
			Cluster::apply_peer_list(vec![Peer::new("https://peer.example/")], &None, &commands);
			let _ = stopped.await;
			anyhow::bail!("source stopped")
		}));

		let waiter = kio::Waiter::noop();
		assert!(mesh.poll(&waiter).is_pending());
		assert!(
			mesh.dialed.contains("https://peer.example/"),
			"the queued peer is dialed"
		);
		assert_eq!(mesh.dialed.tasks.len(), 1);

		drop(stop);
		let Poll::Ready(Err(err)) = mesh.poll(&waiter) else {
			panic!("a failed source must end the mesh");
		};
		assert_eq!(err.to_string(), "source stopped");
	}

	/// A dial task's panic ends the mesh with it, rather than unwinding the task
	/// polling the mesh.
	#[tokio::test]
	async fn mesh_reports_a_dial_panic() {
		let (mut mesh, _commands) = test_mesh();
		mesh.dialed
			.tasks
			.insert(Child::new("cluster dial", async { panic!("injected") }));

		let Poll::Ready(Err(err)) = mesh.poll(&kio::Waiter::noop()) else {
			panic!("a dial panic must end the mesh");
		};
		assert_eq!(err.to_string(), "cluster dial panicked: injected");
	}

	/// With every source finished and no dial left, the mesh has nothing to run.
	#[tokio::test]
	async fn mesh_finishes_once_nothing_is_left() {
		let (mut mesh, _commands) = test_mesh();
		mesh.sources.push(Child::new("test source", async { Ok(()) }));
		assert!(matches!(mesh.poll(&kio::Waiter::noop()), Poll::Ready(Ok(()))));
	}

	/// A malformed object entry keeps the last-good dials, exactly like a
	/// malformed URL does.
	#[test]
	fn malformed_object_peer_list_preserves_current_dial() {
		let commands = DialCommands::default();
		let asymmetric = Peer::new("https://peer.example/").with_cost(1).with_egress(2);
		Cluster::apply_peer_list(vec![asymmetric], &None, &commands);
		assert!(commands.is_empty(), "an invalid list must not reach the dial map");
	}

	#[tokio::test]
	async fn cluster_id_sets_origin() {
		for id in [42, u64::MAX] {
			let cluster = new_cluster(Config {
				id: Some(id),
				..Default::default()
			})
			.expect("valid id");
			assert_eq!(cluster.origin.hop().id(), id);
		}
	}

	/// Cache settings land on the one origin serving and stats share. A handle
	/// cloned at construction stays on that origin.
	#[tokio::test]
	async fn constructed_origin_keeps_cache_and_handles() {
		let duration = Duration::from_secs(5);
		let cache = crate::cache::Config {
			duration: Some(duration.into()),
			..Default::default()
		};
		let cache = cache.init().expect("cache");
		let pool = cache.pool.clone();

		let cluster = Cluster::spawn(
			Options::new(Config {
				id: Some(42),
				..Default::default()
			})
			.with_cache(cache),
		)
		.expect("cluster");

		let origin = cluster.origin.clone();
		assert_eq!(origin.hop().id(), 42);
		assert_eq!(origin.config().cache_duration, duration);
		assert_eq!(origin.config().pool.expiry(), Some(duration));

		let (stats, publisher) = crate::stats::Config {
			enabled: true,
			node: Some("test".to_string()),
			..Default::default()
		}
		.build(origin.clone())
		.expect("stats");
		let cluster = cluster.with_stats(stats, publisher);

		assert_eq!(cluster.origin.hop().id(), origin.hop().id());
		assert_eq!(cluster.origin.config().cache_duration, duration);
		assert_eq!(cluster.origin.config().pool.expiry(), Some(duration));

		let broadcast = origin.create_broadcast("cam").expect("create");
		broadcast.announce(Default::default()).expect("announce");
		let mut track = broadcast.create_track("data", timed()).expect("track");
		track.write_frame(moq_net::Timestamp::ZERO, b"hello").expect("write");
		assert!(pool.used() > 0, "writes charge the constructed cache pool");

		let consumer = cluster.origin.consume();
		kio::clock::timeout(Duration::from_secs(2), consumer.request("cam", None))
			.await
			.expect("broadcast resolves")
			.expect("broadcast present");

		let stats_path = moq_net::Path::from_static(".stats/node/test");
		kio::clock::timeout(
			Duration::from_secs(5),
			consumer.follow(&stats_path, None).unwrap().available(),
		)
		.await
		.expect("stats announced")
		.expect("stats present");
	}

	/// The reserved `cluster.id` 0 is rejected, since it cannot be excluded for loop
	/// detection.
	#[test]
	fn cluster_id_zero_errors() {
		let err = new_cluster(Config {
			id: Some(0),
			..Default::default()
		})
		.err()
		.expect("should error");
		assert!(format!("{err}").contains("--cluster-id"), "got: {err}");
	}

	#[tokio::test]
	async fn cluster_hop_limit_reaches_the_shared_origin() {
		assert_eq!(
			new_cluster(Config::default()).unwrap().origin.config().max_hops.get(),
			32
		);
		for cap in [1, 6, 32] {
			let cluster = new_cluster(Config {
				max_hops: Some(cap),
				..Default::default()
			})
			.unwrap();
			assert_eq!(cluster.origin.config().max_hops.get(), cap);
		}
		for cap in [0, 33, usize::MAX] {
			let err = new_cluster(Config {
				max_hops: Some(cap),
				..Default::default()
			})
			.err()
			.unwrap();
			assert!(err.to_string().contains("--cluster-max-hops"), "{err}");
		}
	}

	/// Static config accepts the object form beside bare URLs, through the same
	/// type the connect API parses.
	#[test]
	fn cluster_connect_object_form_round_trip() {
		let toml = "[cluster]\nconnect = [\"https://a.example/?cost=1\", { url = \"https://b.example/\", cost = 2, egress = 2, token = \"secret\", upstream = true }]\n";
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("cluster-connect-object-toml.toml");
		std::fs::write(&path, toml).unwrap();

		let args = vec![std::ffi::OsString::from("moq-relay"), std::ffi::OsString::from(&path)];
		let config = RelayConfig::parse_and_merge(args).expect("config load");
		assert_eq!(
			config.cluster.connect,
			vec![
				Peer::new("https://a.example/?cost=1"),
				Peer::new("https://b.example/")
					.with_cost(2)
					.with_egress(2)
					.with_token("secret")
					.with_upstream(true),
			]
		);
		// The object normalizes exactly like its URL spellings.
		let desired = parse_peer_list(config.cluster.connect, None).expect("static peers parse");
		assert_eq!(desired.len(), 2);
	}

	/// `--cluster-connect` accepts a full URL verbatim (preserving its `?jwt=`)
	/// and falls back to wrapping a bare host / `host:port` in `https://.../`.
	#[test]
	fn peer_url_full_url_and_legacy_host() {
		// Full URL used verbatim, including its jwt query.
		assert_eq!(
			peer_url("https://cdn.example.com/?jwt=abc").unwrap().as_str(),
			"https://cdn.example.com/?jwt=abc"
		);
		// Bare host (legacy) wrapped in https://.../.
		assert_eq!(
			peer_url("cdn.example.com").unwrap().as_str(),
			"https://cdn.example.com/"
		);
		// `host:port` (legacy) is NOT mis-parsed as scheme `host`.
		assert_eq!(peer_url("localhost:4443").unwrap().as_str(), "https://localhost:4443/");

		assert!(is_legacy_peer("cdn.example.com"));
		assert!(is_legacy_peer("localhost:4443"));
		assert!(!is_legacy_peer("https://cdn.example.com/?jwt=abc"));
	}

	/// A bare `--cluster-connect` host still parses so the process can name what
	/// replaced it, but it configures nothing and must stop a run.
	#[test]
	fn released_cluster_spellings_are_reported_not_applied() {
		let config = Config {
			connect: vec![Peer::new("root.example.com:4443")],
			..Default::default()
		};
		let reported = config.deprecated().to_string();
		assert!(
			reported.contains("a full URL like https://host/?jwt=TOKEN"),
			"{reported}"
		);
		assert!(new_cluster(config).is_err());
	}

	/// A malformed URL may contain a credential, so parse errors must not echo the
	/// raw input into logs.
	#[test]
	fn peer_url_error_redacts_inline_credential() {
		let err = peer_url("https://peer.example:bad/?jwt=top-secret").unwrap_err();
		assert!(!format!("{err:#}").contains("top-secret"));
	}

	/// The same relay spelled as a bare `host:port`, a full URL, or a URL with an
	/// inline jwt all canonicalize to one key, so they share a single dial entry.
	#[test]
	fn canonicalize_peer_key_dedupes_spellings() {
		let key = canonicalize_peer_key("host:4443");
		assert_eq!(key, "https://host:4443/");
		assert_eq!(canonicalize_peer_key("https://host:4443/"), key);
		assert_eq!(canonicalize_peer_key("https://host:4443/?jwt=abc"), key);

		// A URL form and the legacy host:port form dedupe against each other.
		let mut dialed = DialMap::default();
		seeded(
			&mut dialed,
			DialTarget::parse("https://host:4443/?jwt=abc").unwrap(),
			DialSource::Static,
		);
		assert!(dialed.contains(&canonicalize_peer_key("host:4443")));

		// Different ports stay distinct.
		assert_ne!(canonicalize_peer_key("host:4443"), canonicalize_peer_key("host:5555"));
	}

	/// The nested `[cluster.lan]` table survives the TOML-to-CLI merge.
	#[cfg(feature = "cluster-lan")]
	#[test]
	fn cluster_lan_survives_toml_merge() {
		// Usage reads the environment while parsing, so a variable in the developer's
		// shell must not decide the merge.
		if !test_env::Env::without(&["MOQ_CLUSTER_LAN", "MOQ_CLUSTER_LAN_SECRET", "MOQ_CLUSTER_LAN_APP"]).isolate() {
			return;
		}

		let toml = "[cluster]\nnode = \"https://relay.example.com\"\n\n[cluster.lan]\nenabled = true\nsecret = \"cluster.key\"\napp = \"custom\"\n";
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("cluster-lan-toml.toml");
		std::fs::write(&path, toml).unwrap();

		let args = vec![std::ffi::OsString::from("moq-relay"), std::ffi::OsString::from(&path)];
		let config = RelayConfig::parse_and_merge(args).expect("config load");
		assert!(config.cluster.lan.enabled);
		assert_eq!(
			config.cluster.lan.secret.as_deref(),
			Some(std::path::Path::new("cluster.key"))
		);
		assert_eq!(
			config.cluster.lan.app.as_ref().map(ToString::to_string).as_deref(),
			Some("custom")
		);
		assert_eq!(config.cluster.node.as_deref(), Some("https://relay.example.com"));
	}

	/// A CLI flag overrides the same key from TOML, rather than the nesting
	/// hiding it from `update_from`.
	#[cfg(feature = "cluster-lan")]
	#[test]
	fn cli_overrides_toml_cluster_lan() {
		if !test_env::Env::without(&["MOQ_CLUSTER_LAN", "MOQ_CLUSTER_LAN_SECRET", "MOQ_CLUSTER_LAN_APP"]).isolate() {
			return;
		}

		let toml = "[cluster.lan]\nenabled = true\nsecret = \"from-toml.key\"\napp = \"from-toml\"\n";
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("cluster-lan-override.toml");
		std::fs::write(&path, toml).unwrap();

		let args = vec![
			std::ffi::OsString::from("moq-relay"),
			std::ffi::OsString::from(&path),
			std::ffi::OsString::from("--cluster-lan-secret"),
			std::ffi::OsString::from("from-cli.key"),
			std::ffi::OsString::from("--cluster-lan-app"),
			std::ffi::OsString::from("from-cli"),
		];
		let config = RelayConfig::parse_and_merge(args).expect("config load");
		assert_eq!(
			config.cluster.lan.secret.as_deref(),
			Some(std::path::Path::new("from-cli.key"))
		);
		assert_eq!(
			config.cluster.lan.app.as_ref().map(ToString::to_string).as_deref(),
			Some("from-cli")
		);
		assert!(config.cluster.lan.enabled, "the untouched key survives");
	}

	/// A path-capable client version that the listener does not offer cannot
	/// negotiate, so start-up must refuse it rather than retry forever.
	#[cfg(feature = "cluster-lan")]
	#[test]
	fn lan_versions_must_overlap_on_a_path_capable_version() {
		let lite04: moq_net::Version = "moq-lite-04".parse().unwrap();
		let lite05: moq_net::Version = "moq-lite-05".parse().unwrap();

		let mut client = moq_tokio::connect::Config::default();
		client.version = vec![lite04, lite05];
		let mut server = moq_tokio::listen::Config::default();
		server.version = vec![lite04];
		let err = Cluster::validate_lan_versions(&client, &server)
			.expect_err("lite-05 client vs lite-04 listener")
			.to_string();
		assert!(
			err.contains("--connect-version") || err.contains("--listen-version"),
			"{err}"
		);

		server.version = vec![lite05];
		Cluster::validate_lan_versions(&client, &server).expect("shared lite-05");
	}

	/// A LAN mesh without a node URL still starts when the listener has a
	/// generated certificate to advertise.
	#[cfg(feature = "cluster-lan")]
	#[tokio::test]
	async fn lan_without_node_needs_a_fingerprint() {
		let config = Config {
			lan: LanConfig {
				enabled: true,
				..Default::default()
			},
			..Default::default()
		};
		let err = new_cluster(config.clone())
			.unwrap()
			.start()
			.await
			.expect_err("--cluster-lan without advertise must fail");
		let msg = format!("{err}");
		assert!(msg.contains("with_advertise") || msg.contains("--cluster-lan"), "{msg}");

		let err = new_cluster(config)
			.unwrap()
			.with_advertise(LanAdvertise::new(4443))
			.with_connect(Default::default(), Default::default())
			.start()
			.await
			.expect_err("neither node nor fingerprint");
		let msg = format!("{err}");
		assert!(msg.contains("--cluster-node") || msg.contains("generated"), "{msg}");
	}

	/// The secret is optional: without one the mesh is open, and start-up does
	/// not refuse it. Binding mDNS is skipped here by not attaching a client;
	/// the missing-fingerprint/node check still runs first.
	#[cfg(feature = "cluster-lan")]
	#[tokio::test]
	async fn lan_without_a_secret_is_open() {
		let config = Config {
			node: Some("https://us-west.example.com".to_string()),
			lan: LanConfig {
				enabled: true,
				secret: None,
				..Default::default()
			},
			..Default::default()
		};
		let err = new_cluster(config)
			.unwrap()
			.with_advertise(LanAdvertise::new(4443))
			.start()
			.await
			.expect_err("open LAN still needs with_connect");
		let msg = format!("{err}");
		assert!(msg.contains("with_connect"), "{msg}");
		assert!(
			!msg.contains("--cluster-lan-secret"),
			"an open mesh must not demand a secret: {msg}"
		);
	}

	/// A secret or app without the mesh is an error, not a silently ignored flag.
	#[cfg(feature = "cluster-lan")]
	#[test]
	fn lan_secret_and_app_require_the_mesh() {
		assert!(
			LanConfig {
				enabled: false,
				secret: Some("cluster.key".into()),
				..Default::default()
			}
			.validate()
			.unwrap_err()
			.to_string()
			.contains("--cluster-lan=true")
		);
		assert!(
			LanConfig {
				enabled: false,
				app: Some("custom".parse().expect("valid app")),
				..Default::default()
			}
			.validate()
			.unwrap_err()
			.to_string()
			.contains("--cluster-lan-app")
		);
	}

	/// A LAN dial presents the peer's credential on the mesh path and drops
	/// `?jwt=` even when the advertised node URL carried one.
	#[cfg(feature = "cluster-lan")]
	#[test]
	fn lan_dial_urls_carry_the_credential_not_the_token() {
		let mut url = Url::parse("https://relay.example.com/anon?jwt=secret&cost=2").expect("url");
		let cost = take_cost(&mut url).expect("cost");
		strip_jwt(&mut url);
		url.set_path(&format!("{CLUSTER_PATH}/theirs"));
		assert_eq!(cost, Some(2));
		assert_eq!(url.path(), "/.cluster/theirs");
		assert!(!url.query().unwrap_or("").contains("jwt"));
	}

	#[test]
	fn lan_credential_is_the_path_segment_after_the_marker() {
		assert_eq!(Cluster::lan_credential("/.cluster/abc"), Some("abc"));
		assert_eq!(Cluster::lan_credential("/.cluster"), None);
		assert_eq!(Cluster::lan_credential("/.cluster/"), None);
		assert_eq!(Cluster::lan_credential("/.clusterish/abc"), None);
		assert_eq!(Cluster::lan_credential("/room"), None);
		assert!(Cluster::is_lan_path("/.cluster"));
		assert!(Cluster::is_lan_path("/.cluster/abc"));
		assert!(!Cluster::is_lan_path("/.clusterish/abc"));
		assert!(!Cluster::is_lan_path("/room"));
	}

	/// mDNS is just another wanter: a peer also reached by a static seed keeps
	/// its dial when the advertisement goes away, and only the last release
	/// tears it down.
	#[cfg(feature = "cluster-lan")]
	#[test]
	fn mdns_release_respects_the_other_sources() {
		let mut dialed = DialMap::default();
		let peer = target("peer:4443");
		seeded(&mut dialed, peer.clone(), DialSource::Mdns);
		dialed.upsert(peer.clone(), DialSource::Static, &mut no_dial);

		// The static seed wants the same target, so the takeover reuses the dial.
		dialed.release(&peer.key, DialSource::Mdns, &mut no_dial);
		assert!(dialed.contains(&peer.key), "the static seed still wants it");

		dialed.release(&peer.key, DialSource::Static, &mut no_dial);
		assert!(!dialed.contains(&peer.key), "the last release abandons the dial");
		assert!(dialed.tasks.is_empty(), "the abandoned dial is cancelled");

		// Releasing an unknown peer is a no-op, not a panic.
		dialed.release("never-dialed", DialSource::Mdns, &mut no_dial);
	}

	#[test]
	fn cluster_connect_api_survives_toml_merge() {
		let toml = "[cluster.api]\nurl = \"https://api.example.com/cluster/connect\"\ntls.root = [\"api.pem\"]\n";
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("cluster-connect-api-toml.toml");
		std::fs::write(&path, toml).unwrap();

		let args = vec![std::ffi::OsString::from("moq-relay"), std::ffi::OsString::from(&path)];
		let config = RelayConfig::parse_and_merge(args).expect("config load");
		assert_eq!(
			config.cluster.api.url.as_deref(),
			Some("https://api.example.com/cluster/connect")
		);
		assert_eq!(config.cluster.api.tls.root, vec![PathBuf::from("api.pem")]);
	}

	#[tokio::test]
	async fn remote_plaintext_api_refuses_startup() {
		let _ = moq_tokio::crypto::install_default();
		for url in ["http://api.example.com/", "http://192.0.2.1/", "http://[2001:db8::1]/"] {
			let mut config = Config::default();
			config.api.url = Some(url.into());
			let result = new_cluster(config)
				.unwrap()
				.with_connect(Default::default(), Default::default())
				.start()
				.await;
			let Err(err) = result else { panic!("accepted {url}") };
			assert!(err.to_string().contains("loopback"), "{err}");
		}
	}

	#[test]
	fn cluster_api_supports_loopback_http_and_local_files() {
		for source in [
			"http://localhost/",
			"http://127.0.0.2/",
			"http://[::1]/",
			"https://api.example.com/",
		] {
			assert!(connect_api_is_http(source).unwrap(), "{source}");
		}
		for source in ["peers.json", "/etc/moq/peers.json", "file:///etc/moq/peers.json"] {
			assert!(!connect_api_is_http(source).unwrap(), "{source}");
		}
		assert!(connect_api_is_http("http://localhost.example.com/").is_err());
	}

	#[tokio::test]
	async fn cluster_api_rejects_redirect_bodies() {
		use tokio::io::{AsyncReadExt, AsyncWriteExt};
		let _ = moq_tokio::crypto::install_default();
		let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
		let url = format!("http://{}/", listener.local_addr().unwrap()).parse().unwrap();
		let server = async move {
			let (mut stream, _) = listener.accept().await.unwrap();
			let mut data = [0; 4096];
			assert!(stream.read(&mut data).await.unwrap() > 0);
			// No Location header: refusing to follow alone does not reject this body.
			stream
				.write_all(b"HTTP/1.1 302 Found\r\nContent-Length: 2\r\nConnection: close\r\n\r\n[]")
				.await
				.unwrap();
		};
		let tls = moq_tokio::tls::Connect::default().build().unwrap();
		let client = crate::http_client::build(&tls, &[]).unwrap();
		let (peers, ()) = tokio::join!(Cluster::fetch_peer_list(&client, url), server);
		assert!(peers.unwrap_err().to_string().contains("302"));
	}

	/// Two in-process origins share broadcasts both ways over one
	/// fingerprint-pinned `/.cluster/<credential>` session. Wires accept to dial
	/// directly, so the test needs no multicast and stays CI-safe. It does not
	/// exercise `run_mdns`, `Peer::urls()` order, or `discovery.should_dial`;
	/// the names "node" and "fingerprint" are the two origins, not two
	/// advertising modes.
	#[cfg(feature = "cluster-lan")]
	#[tokio::test]
	async fn lan_cluster_path_carries_broadcasts_both_ways() {
		const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
		let _ = moq_tokio::crypto::install_default();

		let node = new_cluster(Config::default()).expect("node cluster");
		let fingerprint = new_cluster(Config::default()).expect("fingerprint cluster");

		let _from_node = node.origin.create_broadcast("from-node").expect("create");
		_from_node.announce(Default::default()).expect("announce");

		let mut listen = moq_tokio::listen::Config::default();
		listen.bind = Some("127.0.0.1:0".parse().unwrap());
		listen.tls.generate = vec!["moq-cluster-lan".to_string()];
		let server = listen.init(Default::default()).expect("bind");
		let port = server.local_addr().expect("local addr").port();
		let fp = server
			.certificates()
			.fingerprints()
			.into_iter()
			.next()
			.expect("generated fingerprint");
		let listener = server.listen().await.expect("listen");

		node.set_lan_credential("listener-proof");
		let accept = node.clone();
		tokio::spawn(async move {
			let mut listener = listener;
			while let Ok(request) = listener.accept().await {
				let conn = crate::Connection::new(request, accept.clone(), crate::auth::Auth::refuse("test"));
				tokio::spawn(async move {
					conn.run().await;
				});
			}
		});

		let connect = moq_tokio::connect::Config::default();
		let fingerprint = fingerprint
			.with_connect(connect, Default::default())
			.with_advertise(LanAdvertise::new(port).with_fingerprint(fp.clone()));
		let url: Url = format!("moqt://127.0.0.1:{port}{CLUSTER_PATH}/listener-proof")
			.parse()
			.expect("url");
		let target = DialTarget {
			key: format!("moqt://127.0.0.1:{port}/"),
			url: url.clone(),
			urls: vec![url],
			cost: None,
			fingerprint: Some(fp),
			lan: true,
			upstream: false,
		};
		let _dial = fingerprint.dial_lan_target(&target).expect("dial");

		let mut announced = fingerprint.origin.consume().announced();
		let (update, _) = kio::clock::timeout(TIMEOUT, next_update(&mut announced))
			.await
			.expect("timed out waiting for from-node")
			.expect("origin closed");
		assert_eq!(update.prefix.as_str(), "from-node");
		// The dialer marks what the peer forwards: it entered at `node`, not here.
		assert_eq!(update.route.source(), origin::Source::Peer(node.origin.hop()));

		let _from_fp = fingerprint.origin.create_broadcast("from-fingerprint").expect("create");
		_from_fp.announce(Default::default()).expect("announce");
		let mut announced = node.origin.consume().announced();
		loop {
			let (update, _) = kio::clock::timeout(TIMEOUT, next_update(&mut announced))
				.await
				.expect("timed out waiting for from-fingerprint")
				.expect("origin closed");
			if update.prefix.as_str() == "from-fingerprint" {
				// The acceptor marks a LAN peer the same way.
				assert_eq!(update.route.source(), origin::Source::Peer(fingerprint.origin.hop()));
				break;
			}
		}

		// Each relay's local view holds only what it ingested itself.
		let mut local = node.origin.consume().local().announced();
		let update = try_next_announced(&mut local).expect("from-node is local");
		assert_eq!(update.prefix.as_str(), "from-node");
		assert_eq!(update.route.source(), origin::Source::Local);
		assert!(
			try_next_announced(&mut local).is_none(),
			"a peer's broadcast is not local"
		);
	}

	/// A start that fails binds no dial socket, so a corrected start on the same
	/// fixed `[connect]` port succeeds. One that never runs registers nothing, so
	/// dropping it releases its client rather than leaving it to `close`.
	#[tokio::test]
	async fn a_failed_start_leaves_the_dial_bind_free() {
		let _ = moq_tokio::crypto::install_default();
		let dir = tempfile::tempdir().expect("tempdir");
		let token = dir.path().join("token");
		let config = Config {
			connect: vec![Peer::new("moql://127.0.0.1:9/")],
			token: Some(token.clone()),
			..Default::default()
		};
		let mut connect = moq_tokio::connect::Config::default();
		// Borrow a free port from the OS, then hand it to the dial template.
		connect.bind = Some(
			std::net::UdpSocket::bind("127.0.0.1:0")
				.expect("probe socket")
				.local_addr()
				.expect("probe addr"),
		);
		let cluster = new_cluster(config)
			.expect("cluster")
			.with_connect(connect, Default::default());

		let err = cluster.clone().start().await.expect_err("the token file is missing");
		assert!(format!("{err:#}").contains("cluster token"), "{err:#}");
		std::fs::write(&token, "secret").expect("write token");
		let _started = cluster.clone().start().await.expect("a corrected start binds the port");
		let registered = cluster.dialers.lock().open.as_ref().expect("open").clients.len();
		assert_eq!(registered, 0, "a start registers its client only once it runs");
	}

	/// A QUIC endpoint that accepts the ALPNs of `connect` and speaks no MoQ, so a
	/// test sees the transport close of the dial itself. `idle` outlasts any wait.
	#[cfg(feature = "quic")]
	fn bare_quic_server(
		connect: &moq_tokio::connect::Config,
		idle: std::time::Duration,
	) -> moq_tokio::quic::driver::endpoint::Endpoint {
		use moq_tokio::quic::driver;
		let key = rcgen::KeyPair::generate().expect("keypair");
		let cert = rcgen::CertificateParams::new(vec!["localhost".to_string()])
			.expect("cert params")
			.self_signed(&key)
			.expect("self-signed cert");
		let mut tls = rustls::ServerConfig::builder()
			.with_no_client_auth()
			.with_single_cert(
				vec![cert.der().clone()],
				rustls::pki_types::PrivateKeyDer::Pkcs8(key.serialize_der().into()),
			)
			.expect("server tls");
		tls.alpn_protocols = connect
			.versions()
			.alpns()
			.iter()
			.map(|alpn| alpn.as_bytes().to_vec())
			.collect();
		let mut transport = driver::transport::Config::default();
		transport.idle_timeout = Some(idle);
		let mut server_config = driver::server::Config::new(Arc::new(tls));
		server_config.transport = transport;
		driver::endpoint::Endpoint::new(
			driver::endpoint::Config::default().with_server(server_config),
			std::net::UdpSocket::bind("127.0.0.1:0").unwrap(),
		)
		.expect("server")
	}

	/// Cancelling `run` aborts the dials a peer list started, which run outside
	/// the tasks of `run` itself, so none keeps redialing with nothing left to close it.
	#[cfg(feature = "quic")]
	#[tokio::test]
	async fn cancelling_run_stops_its_dynamic_dials() {
		const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
		const IDLE: std::time::Duration = std::time::Duration::from_secs(300);
		let _ = moq_tokio::crypto::install_default();

		let mut connect = moq_tokio::connect::Config::default();
		connect.tls.insecure = Some(true);
		let server = bare_quic_server(&connect, IDLE);
		let port = server.local_addr().port();
		let dir = tempfile::tempdir().expect("tempdir");
		let peers = dir.path().join("peers.json");
		std::fs::write(&peers, format!(r#"["moql://127.0.0.1:{port}/"]"#)).expect("write peers");

		let config = Config {
			api: api::Config {
				url: Some(peers.display().to_string()),
				..Default::default()
			},
			..Default::default()
		};
		let mut quic = moq_tokio::quic::Config::default();
		quic.idle_timeout = Some((IDLE).into());
		let cluster = new_cluster(config).expect("cluster").with_connect(connect, quic);
		let started = cluster.clone().start().await.expect("start");
		let running = tokio::spawn(started.run());

		let _conn = kio::clock::timeout(TIMEOUT, server.accept())
			.await
			.expect("dial timeout")
			.expect("incoming")
			.accept()
			.expect("accept")
			.await
			.expect("handshake");
		running.abort();
		let _ = running.await;
		// Each dial holds a stop receiver until its task ends.
		kio::clock::timeout(TIMEOUT, cluster.dialers.stop.closed())
			.await
			.expect("a peer-list dial outlived its cancelled run");
	}

	/// A critical failure ends `run`, which closes the dials on the way out: the
	/// caller consumed the cluster, so nothing else could. The dialing runtime stops
	/// right after, as a process exit would, and the peer's idle timeout is far past
	/// the wait, so only the close explains the link ending.
	#[cfg(feature = "quic")]
	#[tokio::test]
	async fn a_critical_failure_closes_the_dials() {
		use moq_tokio::quic::driver;
		const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
		const IDLE: std::time::Duration = std::time::Duration::from_secs(300);
		let _ = moq_tokio::crypto::install_default();

		let mut connect = moq_tokio::connect::Config::default();
		connect.tls.insecure = Some(true);
		let server = bare_quic_server(&connect, IDLE);
		let port = server.local_addr().port();

		let config = Config {
			connect: vec![Peer::new(format!("moql://127.0.0.1:{port}/"))],
			..Default::default()
		};
		let mut quic = moq_tokio::quic::Config::default();
		quic.idle_timeout = Some((IDLE).into());
		let runtime = tokio::runtime::Builder::new_multi_thread()
			.worker_threads(1)
			.enable_all()
			.build()
			.expect("dialer runtime");
		let (cluster, running) = runtime
			.spawn(async move {
				let cluster = new_cluster(config).expect("cluster").with_connect(connect, quic);
				let started = cluster.clone().start().await.expect("start");
				(cluster, tokio::spawn(started.run()))
			})
			.await
			.expect("dialer task");

		let conn = kio::clock::timeout(TIMEOUT, server.accept())
			.await
			.expect("dial timeout")
			.expect("incoming")
			.accept()
			.expect("accept")
			.await
			.expect("handshake");
		cluster
			.critical
			.spawn("injected", async { panic!("injected critical failure") });
		let result = kio::clock::timeout(TIMEOUT, running)
			.await
			.expect("run returns on a critical failure")
			.expect("run task");
		assert!(result.is_err(), "a critical failure fails run");
		runtime.shutdown_background();

		let err = kio::clock::timeout(TIMEOUT, conn.closed())
			.await
			.expect("the peer heard no close before its idle timeout");
		match err {
			driver::connection::Error::ApplicationClosed(close) => assert_eq!(close.reason, "client shutdown"),
			other => panic!("the link ended with {other:?}, not the shutdown close"),
		}
	}

	/// A second close awaits the shutdown the first started, and still finishes it
	/// when the first caller gives up, so no caller returns while links are open.
	#[tokio::test]
	async fn concurrent_closes_share_one_shutdown() {
		let dialers = Dialers::new();
		// Stands in for a dropped guard's close, which holds the shutdown open.
		let (background, closed) = tokio::sync::oneshot::channel::<()>();
		dialers
			.lock()
			.open
			.as_mut()
			.expect("open")
			.closing
			.push(Child::new("held close", async move {
				let _ = closed.await;
			}));
		let first = tokio::spawn({
			let dialers = dialers.clone();
			async move { dialers.close().await }
		});
		while dialers.lock().open.is_some() {
			tokio::task::yield_now().await;
		}

		let mut second = std::pin::pin!(dialers.close());
		assert!(
			futures::poll!(second.as_mut()).is_pending(),
			"a second close returned before the shutdown finished"
		);
		first.abort();
		let _ = first.await;
		drop(background);
		kio::clock::timeout(std::time::Duration::from_secs(5), second)
			.await
			.expect("the second close finishes the shutdown");
	}

	/// A LAN dial owns its client, so closing the cluster closes that too: the peer
	/// drops the dialer's routes at once, though the dialing runtime stops right
	/// after, as a process exit would. Both idle timeouts are far past the wait.
	#[cfg(feature = "cluster-lan")]
	#[tokio::test]
	async fn closing_a_cluster_closes_its_lan_dials() {
		const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
		const IDLE: std::time::Duration = std::time::Duration::from_secs(300);
		let _ = moq_tokio::crypto::install_default();
		let mut quic = moq_tokio::quic::Config::default();
		quic.idle_timeout = Some((IDLE).into());

		let node = new_cluster(Config::default()).expect("node cluster");
		let mut listen = moq_tokio::listen::Config::default();
		listen.bind = Some("127.0.0.1:0".parse().unwrap());
		listen.tls.generate = vec!["moq-cluster-lan".to_string()];
		let server = listen.init(quic.clone()).expect("bind");
		let port = server.local_addr().expect("local addr").port();
		let fp = server
			.certificates()
			.fingerprints()
			.into_iter()
			.next()
			.expect("generated fingerprint");
		let listener = server.listen().await.expect("listen");
		node.set_lan_credential("listener-proof");
		let accept = node.clone();
		tokio::spawn(async move {
			let mut listener = listener;
			while let Ok(request) = listener.accept().await {
				let conn = crate::Connection::new(request, accept.clone(), crate::auth::Auth::refuse("test"));
				tokio::spawn(conn.run());
			}
		});

		let url: Url = format!("moqt://127.0.0.1:{port}{CLUSTER_PATH}/listener-proof")
			.parse()
			.expect("url");
		let target = DialTarget {
			key: format!("moqt://127.0.0.1:{port}/"),
			url: url.clone(),
			urls: vec![url],
			cost: None,
			fingerprint: Some(fp),
			lan: true,
			upstream: false,
		};
		let runtime = tokio::runtime::Builder::new_multi_thread()
			.worker_threads(1)
			.enable_all()
			.build()
			.expect("dialer runtime");
		let (dialer, broadcast) = runtime
			.spawn(async move {
				let dialer = new_cluster(Config::default())
					.expect("dialer cluster")
					.with_connect(Default::default(), quic);
				let broadcast = dialer.origin.create_broadcast("from-dialer").expect("create");
				broadcast.announce(Default::default()).expect("announce");
				tokio::spawn(dialer.clone().supervise_remote(target, String::new()));
				(dialer, broadcast)
			})
			.await
			.expect("dialer task");

		let mut announced = node.origin.consume().announced();
		loop {
			let (update, active) = kio::clock::timeout(TIMEOUT, next_update(&mut announced))
				.await
				.expect("timed out waiting for from-dialer")
				.expect("origin closed");
			if update.prefix.as_str() == "from-dialer" && active {
				break;
			}
		}

		runtime.spawn(async move { dialer.close().await }).await.expect("close");
		runtime.shutdown_background();
		// Held throughout: dropping it would unannounce over a live link instead.
		let _broadcast = broadcast;

		loop {
			let (update, active) = kio::clock::timeout(TIMEOUT, next_update(&mut announced))
				.await
				.expect("the peer kept the dialer's route past the close")
				.expect("origin closed");
			if update.prefix.as_str() == "from-dialer" && !active {
				break;
			}
		}
	}

	/// A grant naming a cluster peer marks the session's routes as a peer's, so the
	/// relay's local view leaves them out; any other grant ingests here.
	#[tokio::test]
	async fn peer_grant_marks_the_session_publisher() {
		let cluster = new_cluster(Config::default()).expect("cluster");
		let all: moq_net::Patterns = [moq_net::Pattern::all()].into_iter().collect();

		let client = auth::Token::new("/", &moq_auth::Grant::new(all.clone(), all.clone())).unwrap();
		let mut grant = moq_auth::Grant::new(all.clone(), all);
		grant.peer = true;
		let peer = auth::Token::new("/", &grant).unwrap();

		let _ingest = cluster
			.publisher(&client)
			.expect("client")
			.publish("ingest", Default::default());
		let _forwarded = cluster
			.publisher(&peer)
			.expect("peer")
			.publish("forwarded", Default::default());

		let mut announced = cluster.origin.consume().announced();
		let forwarded = try_next_announced(&mut announced).expect("forwarded");
		assert_eq!(forwarded.prefix.as_str(), "forwarded");
		assert!(matches!(forwarded.route.source(), origin::Source::Peer(_)));
		let ingest = try_next_announced(&mut announced).expect("ingest");
		assert_eq!(ingest.route.source(), origin::Source::Local);

		let mut local = cluster.origin.consume().local().announced();
		assert_eq!(
			try_next_announced(&mut local).expect("ingest").prefix.as_str(),
			"ingest"
		);
		assert!(try_next_announced(&mut local).is_none());
	}

	/// An upstream grant's session is a peer that never reads another upstream's
	/// routes: two accepted cores never transit through this relay.
	#[tokio::test]
	async fn upstream_grant_hides_other_upstreams() {
		let cluster = new_cluster(Config::default()).expect("cluster");
		let all: moq_net::Patterns = [moq_net::Pattern::all()].into_iter().collect();

		let client = auth::Token::new("/", &moq_auth::Grant::new(all.clone(), all.clone())).unwrap();
		let mut grant = moq_auth::Grant::new(all.clone(), all);
		grant.peer = true;
		let peer = auth::Token::new("/", &grant).unwrap();
		grant.upstream = true;
		let core = auth::Token::new("/", &grant).unwrap();

		let _ingest = cluster
			.publisher(&client)
			.expect("client")
			.publish("ingest", Default::default());
		let _mesh = cluster
			.publisher(&peer)
			.expect("peer")
			.publish("mesh", Default::default());
		let _core = cluster
			.publisher(&core)
			.expect("core")
			.publish("core", Default::default());

		let mut everyone = cluster.subscriber(&client).expect("client").consume().announced();
		let mut prefixes = std::iter::from_fn(|| try_next_announced(&mut everyone))
			.map(|announce| announce.prefix.to_string())
			.collect::<Vec<_>>();
		prefixes.sort();
		assert_eq!(prefixes, ["core", "ingest", "mesh"]);

		let mut toward_core = cluster.subscriber(&core).expect("core").consume().announced();
		let mut prefixes = std::iter::from_fn(|| try_next_announced(&mut toward_core))
			.map(|announce| announce.prefix.to_string())
			.collect::<Vec<_>>();
		prefixes.sort();
		assert_eq!(prefixes, ["ingest", "mesh"]);
	}

	/// A `/.cluster` request on a cluster without LAN discovery is refused.
	#[cfg(feature = "cluster-lan")]
	#[tokio::test]
	async fn lan_path_without_discovery_is_refused() {
		assert_eq!(
			new_cluster(Config::default())
				.expect("cluster")
				.verify_lan_credential("anything"),
			None
		);
	}
}
