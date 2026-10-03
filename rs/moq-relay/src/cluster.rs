use std::{
	collections::HashMap,
	path::PathBuf,
	sync::{
		Arc, Mutex,
		atomic::{AtomicU64, Ordering},
	},
	time::Duration,
};

use anyhow::Context;
use moq_net::origin;
use moq_net::{Hop, stats::Tier};
use reqwest_middleware::ClientWithMiddleware;
use tokio::task::AbortHandle;
use tracing::Instrument as _;
use url::Url;

use crate::auth;

/// The request path prefix a LAN mesh dial presents, marking it as a peer rather
/// than an ordinary publisher or viewer on the same listener.
pub(crate) const CLUSTER_PATH: &str = "/.cluster";

/// How often the relay re-checks an http(s) `--cluster-connect-api` endpoint. The
/// HTTP cache middleware suppresses the actual network round-trip while the cached
/// list is still fresh (per the response's `Cache-Control`), so this is the floor
/// on responsiveness, not on origin load: a tighter `max-age` means more of these
/// ticks turn into real conditional GETs.
const CONNECT_API_POLL_INTERVAL: Duration = Duration::from_secs(30);

/// One cluster peer to dial, as listed in [`Config::connect`] or returned
/// by a `connect_api` endpoint.
///
/// Accepts a URL string or an object with `url` plus
/// policy: `cost` prices the link, `egress` declares this relay's own price to
/// the peer, and `token` carries the peer's credential. `egress` defaults to
/// `cost`; a different effective value is rejected until asymmetric routing
/// lands. An object `token` behaves exactly like an inline `?jwt=` and is
/// redacted the same way. Unknown object fields are rejected, as is an object
/// that sets policy while its `url` still carries `?cost=` or `?jwt=`.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Peer {
	url: String,
	cost: Option<u64>,
	egress: Option<u64>,
	token: Option<String>,
}

impl std::fmt::Debug for Peer {
	/// Redact `token`: [`crate::Config::load`] traces the whole resolved config,
	/// so a derived `Debug` would print cluster credentials into logs.
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Peer")
			.field("url", &self.url)
			.field("cost", &self.cost)
			.field("egress", &self.egress)
			.field("token", &self.token.as_ref().map(|_| "..."))
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
				f.write_str("a peer URL string or an object with url, cost, egress, token")
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
		if self.cost.is_none() && self.egress.is_none() && self.token.is_none() {
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
/// than one at once (e.g. found on the LAN *and* listed by `--cluster-connect-api`), so
/// [`DialEntry`] tracks a set of these and only tears the dial down when the last
/// one releases it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DialSource {
	/// Seeded from `--cluster-connect`. Never released, so the dial retries forever
	/// (operator intent says "always dial").
	Static,
	/// Supplied by `--cluster-connect-api`. Released when a fetched peer list no
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
	handle: AbortHandle,
	sources: DialSources,
	active: DialSource,
}

/// Map of in-flight cluster dials, keyed by canonical peer identity. Cloneable: the inner
/// map is shared via `Arc<Mutex<_>>` so the discovery tasks and the static-seed
/// phase write to the same set of entries.
#[derive(Clone, Default)]
struct DialMap {
	inner: Arc<Mutex<HashMap<String, DialEntry>>>,
}

impl DialMap {
	/// True if `peer` is already being dialed.
	fn contains(&self, peer: &str) -> bool {
		self.inner.lock().expect("dial map poisoned").contains_key(peer)
	}

	/// Record an already-spawned dial under `source`. If the source is redundant,
	/// abort its task instead of leaking a second session.
	fn insert(&self, target: DialTarget, handle: AbortHandle, source: DialSource) {
		let mut handle = Some(handle);
		self.upsert(target, source, &mut |_| handle.take().expect("dial handle used once"));
		if let Some(handle) = handle {
			handle.abort();
		}
	}

	/// Add or update one source's target, replacing the live dial only when that
	/// source already owns it. Other sources retain their latest target as a
	/// fallback without changing the first source's active configuration.
	fn upsert<F>(&self, target: DialTarget, source: DialSource, spawn: &mut F)
	where
		F: FnMut(DialTarget) -> AbortHandle,
	{
		let mut map = self.inner.lock().expect("dial map poisoned");
		if let Some(entry) = map.get_mut(&target.key) {
			let replace = entry.active == source && entry.sources.get(source) != Some(&target);
			entry.sources.set(source, target.clone());
			if replace {
				entry.handle.abort();
				entry.handle = spawn(target);
			}
			return;
		}

		let key = target.key.clone();
		let handle = spawn(target.clone());
		let mut sources = DialSources::default();
		sources.set(source, target);
		map.insert(
			key,
			DialEntry {
				handle,
				sources,
				active: source,
			},
		);
	}

	/// Release one source. If it owned the live dial, switch to a remaining
	/// source's latest target or abandon the peer when no source remains.
	fn release<F>(&self, peer: &str, source: DialSource, spawn: &mut F)
	where
		F: FnMut(DialTarget) -> AbortHandle,
	{
		let mut map = self.inner.lock().expect("dial map poisoned");
		let Some(entry) = map.get_mut(peer) else { return };
		if entry.sources.get(source).is_none() {
			return;
		}
		let active_target = entry.sources.get(source).cloned();
		entry.sources.clear(source);
		if entry.active != source {
			return;
		}

		if let Some((next_source, next_target)) = entry.sources.fallback() {
			let next_target = next_target.clone();
			entry.active = next_source;
			if active_target.as_ref() != Some(&next_target) {
				entry.handle.abort();
				entry.handle = spawn(next_target);
			}
			return;
		}

		let entry = map.remove(peer).expect("entry exists");
		entry.handle.abort();
	}

	/// Reconcile the API source against `desired`, including changes to a peer's
	/// dial-affecting URL or link cost while its canonical identity stays fixed.
	fn reconcile_api<F>(&self, desired: &HashMap<String, DialTarget>, mut spawn: F)
	where
		F: FnMut(DialTarget) -> AbortHandle,
	{
		for target in desired.values() {
			self.upsert(target.clone(), DialSource::Api, &mut spawn);
		}

		let removed: Vec<String> = self
			.inner
			.lock()
			.expect("dial map poisoned")
			.iter()
			.filter(|(peer, entry)| entry.sources.api.is_some() && !desired.contains_key(*peer))
			.map(|(peer, _)| peer.clone())
			.collect();
		for peer in removed {
			self.release(&peer, DialSource::Api, &mut spawn);
		}
	}
}

/// Configuration for relay clustering.
///
/// [`Self::connect`] / [`Self::connect_api`] list peers to dial, and
/// [`Self::node`] is this relay's own URL (identity). A relay only dials peers
/// configured here or found on the LAN; it never dials a URL learned from an
/// announcement.
///
/// Hop-based routing on broadcasts prevents announcement loops regardless of topology.
#[serde_with::serde_as]
#[derive(usage::Args, Clone, Debug, serde::Serialize, serde::Deserialize, Default)]
#[usage(unknown_flags = "error", args_override_self = false)]
#[serde_with::skip_serializing_none]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct Config {
	/// Fixed origin (hop) id for this relay, identifying it in the hop chains
	/// carried on each broadcast for loop detection and shortest-path routing.
	///
	/// Unset (the default) picks a fresh random id on every start. Set it to give
	/// a node a stable identity across restarts. Must be non-zero and below 2^62
	/// (the wire varint limit); an out-of-range value errors at startup. Keep it
	/// below 2^53 for compatibility with older `@moq/lite` JS clients, which
	/// decode hop ids as a `u53` and reject anything larger.
	#[usage(
		name = "cluster-id",
		long = "cluster-id",
		env = "MOQ_CLUSTER_ID",
		setting = "cluster.id"
	)]
	pub id: Option<u64>,

	/// Connect to one or more other cluster nodes. Each entry is a full URL, e.g.
	/// `https://host/?jwt=TOKEN`, or an object with `url`, `cost`, `egress`,
	/// and `token`; see [`Peer`]. A bare host or `host:port` is refused; pass the
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

	/// Fetch the list of peers to dial from an HTTP(S) URL or a local file,
	/// reloading at runtime without a restart. The source returns a JSON array
	/// of peers: bare URL strings and/or objects with `url`, `cost`, `egress`,
	/// and `token`, exactly as [`Self::connect`] accepts, e.g.
	/// `["https://a.pop.example/?cost=1", {"url": "https://b.pop.example/", "cost": 2}]`.
	/// An http(s) URL is re-checked on a fixed cadence, with caching, conditional revalidation
	/// (`ETag` / `Last-Modified`), and stale-if-error handled by the shared HTTP
	/// cache client, so the response's `Cache-Control` controls how often a real
	/// fetch hits the endpoint; a local path is watched via OS filesystem
	/// notifications (with a periodic re-check fallback). This relay's own
	/// [`Self::node`] value, when set, is sent as a `?node=` query param so the
	/// server can return this node's peers. The relay keeps the last good list if
	/// a fetch fails. Composes with [`Self::connect`].
	#[usage(
		name = "cluster-connect-api",
		long = "cluster-connect-api",
		env = "MOQ_CLUSTER_CONNECT_API",
		setting = "cluster.connect_api"
	)]
	pub connect_api: Option<String>,

	/// This relay's own externally-reachable URL (identity). Sent to
	/// [`Self::connect_api`] as a `?node=` query param so the endpoint can return
	/// this node's peers, and advertised over mDNS when LAN discovery is on. On
	/// its own it neither opens nor accepts a connection.
	#[usage(
		name = "cluster-node",
		long = "cluster-node",
		env = "MOQ_CLUSTER_NODE",
		setting = "cluster.node"
	)]
	pub node: Option<String>,

	/// Released spelling of the removed gossip discovery, kept so
	/// [`Self::deprecated`] can refuse it. Any value, boolean or the older URL
	/// form, is refused.
	#[doc(hidden)]
	#[usage(
		name = "cluster-mesh",
		long = "cluster-mesh",
		env = "MOQ_CLUSTER_MESH",
		setting = "cluster.mesh",
		default_missing = "true",
		num_args = 0..=1,
		require_equals = true,
		hide = true,
	)]
	#[serde(default, deserialize_with = "deserialize_bool_or_string")]
	pub mesh: Option<String>,

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
	/// stats under. Defaults to the unprefixed tier.
	#[usage(
		name = "cluster-tier",
		long = "cluster-tier",
		env = "MOQ_CLUSTER_TIER",
		setting = "cluster.tier"
	)]
	pub tier: Option<String>,
	/// Released spelling, kept so [`Self::deprecated`] can name that linger is gone.
	#[doc(hidden)]
	#[usage(skip)]
	#[serde(with = "crate::duration::serde_option")]
	pub linger: Option<std::time::Duration>,

	#[usage(
		name = "cluster-linger",
		long = "cluster-linger",
		env = "MOQ_CLUSTER_LINGER",
		setting = "cluster.linger",
		hide = true
	)]
	#[serde(default, rename = "__cli_linger", skip_serializing_if = "Option::is_none")]
	linger_arg: Option<crate::duration::Duration>,
}

impl Config {
	/// Released spellings this config was parsed from, each paired with what replaced it.
	pub fn deprecated(&self) -> moq_tokio::cli::Deprecated {
		let mut found = moq_tokio::cli::Deprecated::default();
		if self.linger.is_some() || self.linger_arg.is_some() {
			found.changed(
				"--cluster-linger",
				Some("MOQ_CLUSTER_LINGER"),
				"(removed)",
				"a broadcast closes as soon as its last publisher is lost",
			);
		}
		if self.mesh.is_some() {
			found.changed(
				"--cluster-mesh",
				Some("MOQ_CLUSTER_MESH"),
				"--cluster-connect or --cluster-connect-api",
				"gossip discovery is removed; list every peer this relay dials",
			);
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
#[derive(usage::Args, Clone, Debug, serde::Serialize, serde::Deserialize, Default)]
#[usage(unknown_flags = "error", args_override_self = false)]
#[serde_with::skip_serializing_none]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
#[cfg(feature = "cluster-lan")]
pub struct LanConfig {
	/// Enable mDNS discovery. Boolean flag: pass `--cluster-lan` (or `=true` /
	/// `=false`). Advertises the listener fingerprint when the certificate was
	/// generated or supplied in-memory, the [`Config::node`] URL when one
	/// is configured, and needs at least one of them.
	#[usage(
		name = "cluster-lan",
		long = "cluster-lan",
		env = "MOQ_CLUSTER_LAN",
		setting = "cluster.lan.enabled",
		bool_value
	)]
	pub enabled: bool,

	/// The shared key admitting a peer to the LAN mesh, as 64 hexadecimal
	/// characters or a path to a file containing them.
	///
	/// Optional. Without it, anyone who can reach the listener joins, so leave
	/// it unset only on networks you trust. With it, only peers that prove they
	/// hold the same key are discovered or accepted.
	#[usage(
		name = "cluster-lan-secret",
		long = "cluster-lan-secret",
		env = "MOQ_CLUSTER_LAN_SECRET",
		value_name = "HEX_OR_PATH",
		setting = "cluster.lan.secret"
	)]
	pub secret: Option<String>,

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
/// taken after construction. Independent services ([`Cluster::with_client`],
/// [`Cluster::with_stats`]) attach afterwards without rebuilding it.
#[derive(Default)]
#[non_exhaustive]
pub struct Options {
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
	/// `None` when nothing is configured, so [`run`](Self::run) returns immediately.
	work: Option<Work>,
}

impl Started {
	/// True when nothing is configured, so [`run`](Self::run) returns immediately.
	pub fn standalone(&self) -> bool {
		self.work.is_none()
	}

	/// Run the cluster until it stops.
	///
	/// Returns immediately when nothing is configured (standalone).
	pub async fn run(self) -> anyhow::Result<()> {
		match self.work {
			Some(work) => self.cluster.run_work(work).await,
			None => Ok(()),
		}
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
	/// The live mDNS advertisement, bound by [`Cluster::start`].
	#[cfg(feature = "cluster-lan")]
	discovery: Option<moq_tokio::mdns::Discovery>,
}

/// A relay cluster built around a single [`origin::Producer`].
///
/// Local sessions and remote cluster connections all publish into the same
/// origin. Loop prevention and route preference come from the hop list carried
/// on each announced route (see [`moq_net::origin::Route`]).
///
/// Construct with [`Cluster::new`], then attach a QUIC client and (optionally)
/// a [`stats::Registry`](moq_net::stats::Registry) with the `with_*` builder
/// methods. Those builders do not rebuild the origin. A cluster without a
/// client can serve local sessions but cannot dial remote peers.
#[derive(Clone)]
pub struct Cluster {
	config: Config,
	client: Option<moq_tokio::Client>,
	/// Dial template for LAN peers that pin a fingerprint; static and API
	/// dials keep [`Self::client`].
	connect: Option<moq_tokio::connect::Config>,
	quic: Option<moq_tokio::quic::Config>,
	/// Bound listen port and optional generated-certificate fingerprint.
	advertise: Option<LanAdvertise>,
	/// Set by [`Self::start`] when LAN discovery is on, so inbound
	/// `/.cluster/<credential>` can be verified on every clone.
	#[cfg(feature = "cluster-lan")]
	lan_auth: Arc<std::sync::OnceLock<LanAuth>>,
	pub(crate) nodes: crate::nodes::Nodes,

	/// Hands out the `conn` id every session logs under, inbound and outbound
	/// alike, so one id space covers the whole process and an id in the `/nodes`
	/// view always points at the same session in the logs.
	connection_ids: Arc<AtomicU64>,

	/// Client TLS config used to build the `--cluster-connect-api` HTTP client, so
	/// peer-list fetches present the same cluster cert the QUIC dials do. `Arc` so
	/// cloning a `Cluster` per connection stays cheap.
	client_tls: Option<Arc<rustls::ClientConfig>>,

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
pub struct Admitted {
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
	/// The origin ID used by this relay on the wire.
	pub fn id(&self) -> u64 {
		self.origin.hop().id()
	}

	/// Creates a cluster with one origin, using [`Options`] for identity
	/// and cache.
	///
	/// Use [`with_client`](Self::with_client) to enable dialing remote peers
	/// (required when `config.connect` is non-empty), and
	/// [`with_stats`](Self::with_stats) to enable metrics publishing. Those
	/// builders do not rebuild the origin.
	///
	/// Must be called within a tokio runtime: the origin's lifecycle driver is
	/// spawned here, so the origin serves sessions whether or not
	/// [`start`](Self::start) (the mesh half) is ever called.
	///
	/// Errors if `config.id` is set but invalid: it must be non-zero and below
	/// 2^62 (the wire varint limit). An unset id picks a fresh random origin.
	pub fn new(options: Options) -> anyhow::Result<Self> {
		let Options { config, cache } = options;
		let id = match config.id {
			Some(0) => anyhow::bail!("--cluster-id must be non-zero"),
			Some(id) if id >= 1 << 62 => {
				anyhow::bail!("--cluster-id must be below 2^62 (wire varint limit), got {id}")
			}
			Some(id) => Hop::new(id).expect("cluster id already validated"),
			None => Hop::random(),
		};
		let deprecated = config.deprecated();
		anyhow::ensure!(deprecated.is_empty(), "{deprecated}");
		// Reject a repeated static identity with conflicting policy up front,
		// matching the `--cluster-connect-api` validation, instead of silently
		// keeping the first entry when dials spawn.
		parse_peer_list(config.connect.clone(), None).context("invalid --cluster-connect peer list")?;
		let mut origin_config = origin::Config::new(id);
		if let Some(cache) = cache {
			origin_config.pool = cache.pool;
			origin_config.cache_duration = cache.duration;
		}
		let origin = moq_tokio::origin::spawn_config(origin_config);
		let nodes = crate::nodes::Nodes::default();
		tracing::info!(hop_id = %origin.hop(), configured = config.id.is_some(), "cluster initialized");
		Ok(Cluster {
			config,
			client: None,
			connect: None,
			quic: None,
			advertise: None,
			#[cfg(feature = "cluster-lan")]
			lan_auth: Arc::new(std::sync::OnceLock::new()),
			nodes,
			connection_ids: Arc::default(),
			client_tls: None,
			origin,
			stats: moq_net::stats::Registry::disabled(),
			_stats_publisher: None,
		})
	}

	/// Admit a gateway session and scope both origin directions from its grant.
	pub async fn admit(&self, auth: &auth::Auth, request: moq_auth::Request) -> Result<Admitted, auth::Error> {
		if !matches!(request.event, moq_auth::Event::Connect) {
			return Err(auth::Error::Request("admission requires a connect event".into()));
		}
		let lease = auth.admit(request.clone()).await?;
		self.scope(lease, &request)
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

		let stats = self.stats.tier(token.tier.clone()).session(&token.root);
		tracing::info!(transport = %request.transport, ?request.role, tier = %token.tier, root = %token.root,
			publish = ?publisher.as_ref().map(origin::Producer::allowed),
			subscribe = ?subscriber.as_ref().map(origin::Producer::allowed),
			"session accepted");
		let publisher = match request.role {
			Some(moq_auth::Role::Subscriber) => None,
			_ => publisher.map(|origin| origin.with_stats(stats.clone())),
		};
		// An authenticated cluster peer (a verified client certificate or the LAN
		// credential) discovers hidden routes whether or not it asks. A peer that
		// predates the hidden opt-in (below moq-lite-07-wip, or moq-transport without
		// MoQ Hidden) would otherwise lose every dot path during a rolling upgrade.
		// TODO: drop the exemption once deployed peers all opt in.
		let cluster_peer = request.tls.is_some() || Self::is_lan_path(&request.path);
		let subscriber = match request.role {
			Some(moq_auth::Role::Publisher) => None,
			_ => subscriber.map(|origin| origin.consume().with_hidden(cluster_peer).with_stats(stats.clone())),
		};
		Ok(Admitted {
			lease: lease.with_stats(stats.clone()),
			publisher,
			subscriber,
			stats,
		})
	}

	/// Attach a QUIC client used to dial cluster peers.
	///
	/// Required when `config.connect` is non-empty; [`start`](Self::start) returns
	/// an error otherwise.
	pub fn with_client(mut self, client: moq_tokio::Client) -> Self {
		self.client = Some(client);
		self
	}

	/// Attach the dial template used to build a per-peer client for LAN mesh
	/// dials (fingerprint pinning, request-path versions, ephemeral bind).
	///
	/// Required when `--cluster-lan` is on; [`start`](Self::start) returns an
	/// error otherwise.
	pub fn with_connect(mut self, connect: moq_tokio::connect::Config, quic: moq_tokio::quic::Config) -> Self {
		self.connect = Some(connect);
		self.quic = Some(quic);
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

	/// Attach the client TLS config used for `--cluster-connect-api` peer-list
	/// fetches. Required when `config.connect_api` is set; pass the same config
	/// used to build the QUIC [`with_client`](Self::with_client) so the endpoint
	/// sees this relay's cluster certificate.
	pub fn with_client_tls(mut self, tls: rustls::ClientConfig) -> Self {
		self.client_tls = Some(Arc::new(tls));
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
	/// registry and taking over keeping its publish task alive.
	///
	/// Build one with [`stats::Config::build`](crate::stats::Config::build), passing
	/// [`Self::origin`] so it publishes through the same origin cluster peers read
	/// from. The cluster holds the producer from here on, so the caller may drop
	/// its own handle: publishing lasts exactly as long as the cluster does.
	pub fn with_stats(mut self, stats: moq_stats::Producer) -> Self {
		self.stats = stats.registry().clone();
		self._stats_publisher = Some(stats);
		self
	}

	/// Billing tier cluster-peer traffic records under (`--cluster-tier`).
	/// An absent or empty label selects the default unprefixed tier.
	fn cluster_tier(&self) -> Tier {
		crate::configured_tier(self.config.tier.clone())
	}

	/// Returns an [`origin::Producer`] scoped to this session's subscribe permissions.
	///
	/// Passed by reference to [`moq_net::Server::with_publisher`] (or the
	/// equivalent per-request setter), which derives the read handle.
	pub fn subscriber(&self, token: &auth::Token) -> Option<origin::Producer> {
		self.mounted(token)?.scope(&token.root, &token.subscribe).ok()
	}

	/// Returns an [`origin::Producer`] scoped to this session's publish permissions,
	/// marked [`origin::Producer::peer`] when the grant names a cluster peer.
	/// Nothing is published beneath the grant's mounts.
	pub fn publisher(&self, token: &auth::Token) -> Option<origin::Producer> {
		let publisher = self.mounted(token)?.scope(&token.root, &token.publish).ok()?;
		Some(match token.peer {
			true => publisher.peer(),
			false => publisher,
		})
	}

	/// The origin with the grant's mounts applied, before it is scoped to the
	/// session. A mount the origin refuses (overlapping another) admits nothing.
	fn mounted(&self, token: &auth::Token) -> Option<origin::Producer> {
		let mut origin = self.origin.clone();
		for (at, target) in &token.mounts {
			origin = match origin.mount(token.root.join(at), target) {
				Ok(origin) => origin,
				Err(err) => {
					tracing::warn!(root = %token.root, %at, %target, %err, "grant mount refused");
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
		let mut grant = moq_auth::Grant::new(
			[moq_auth::Pattern::all()].into_iter().collect(),
			[moq_auth::Pattern::all()].into_iter().collect(),
		);
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

	/// Validate the cluster config and bind anything that can fail, returning a
	/// [`Started`] to run.
	///
	/// Split from running because that future is first polled well after the
	/// process reports itself ready. Anything fallible left inside it (a malformed
	/// node URL, an unreadable token file, a bad LAN key, an mDNS bind failure)
	/// would release systemd's dependent units on a relay that is about to exit.
	/// Call this before signalling readiness, and hand the result to `run`.
	///
	/// Bails when peers are configured to dial but no client was attached via
	/// [`with_client`](Self::with_client).
	pub async fn start(self) -> anyhow::Result<Started> {
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

		let can_dial = !self.config.connect.is_empty() || self.config.connect_api.is_some() || lan;
		if !can_dial {
			tracing::info!("no cluster peers configured; running standalone");
			return Ok(Started {
				work: None,
				cluster: self,
			});
		}

		anyhow::ensure!(
			self.client.is_some(),
			"cluster peers configured but no QUIC client attached (call Cluster::with_client)"
		);

		// Only http(s) sources need the TLS client; a local file doesn't.
		if let Some(source) = &self.config.connect_api {
			anyhow::ensure!(
				!connect_api_is_http(source) || self.client_tls.is_some(),
				"cluster.connect_api with an http(s) URL needs client TLS (call Cluster::with_client_tls)"
			);
		}

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
				let discovery = lan_discovery(advertise, node.as_deref(), secret, app).await?;
				let _ = self.lan_auth.set(LanAuth {
					credential: discovery.credential().to_string(),
				});
				Some(discovery)
			}
			false => None,
		};

		Ok(Started {
			work: Some(Work {
				node,
				token,
				#[cfg(feature = "cluster-lan")]
				discovery,
			}),
			cluster: self,
		})
	}

	/// Runs the cluster event loop against the work [`start`](Self::start)
	/// resolved: dials the static peers, then keeps the `connect_api` list and
	/// LAN discovery reconciled into the same dial set.
	async fn run_work(self, work: Work) -> anyhow::Result<()> {
		let Work {
			node,
			token,
			#[cfg(feature = "cluster-lan")]
			discovery,
		} = work;

		// Every source shares one dial map so a peer reached via several only
		// opens a single dial.
		let dialed = DialMap::default();
		let mut tasks = tokio::task::JoinSet::new();
		// Tasks whose ending is a failure rather than an ordinary lifecycle event,
		// so their result has to be looked at instead of drained.
		#[allow(unused_mut, reason = "only the cluster-lan feature spawns into it")]
		let mut supervised: tokio::task::JoinSet<anyhow::Result<()>> = tokio::task::JoinSet::new();

		for peer in &self.config.connect {
			let target = DialTarget::from_peer(peer).context("invalid --cluster-connect peer URL")?;
			if dialed.contains(&target.key) {
				continue;
			}
			let this = self.clone();
			let token = token.clone();
			let peer_for_task = target.clone();
			let handle = tasks.spawn(this.supervise_remote(peer_for_task, token));
			dialed.insert(target, handle, DialSource::Static);
		}

		if let Some(source) = self.config.connect_api.clone() {
			let this = self.clone();
			let token = token.clone();
			let dialed = dialed.clone();
			let node = node.clone();
			tasks.spawn(async move {
				this.run_connect_api(source, node, token, dialed).await;
			});
		}

		#[cfg(feature = "cluster-lan")]
		if let Some(discovery) = discovery {
			let this = self.clone();
			let dialed = dialed.clone();
			// Supervised rather than fire-and-forget: a dial task ending is ordinary
			// (that peer went away), but discovery ending is the relay going blind.
			supervised.spawn(async move { this.run_mdns(dialed, discovery).await });
		}

		loop {
			tokio::select! {
				// A supervised task ending at all is a failure, so its result decides
				// the cluster's. A panic surfaces too rather than being swallowed.
				Some(res) = supervised.join_next() => res.context("cluster task panicked")??,
				// A dial task ending is ordinary: that peer went away.
				Some(_) = tasks.join_next() => {}
				else => break,
			}
		}
		Ok(())
	}

	/// Dial every process that advertises itself on the LAN.
	///
	/// Each candidate is [`Peer::urls`](moq_tokio::mdns::Peer::urls) in order
	/// (node first) on `/.cluster/<credential>`, with the advertised fingerprint
	/// pinned. The lower discovery id dials; the other waits inbound. A LAN dial
	/// never carries `?jwt=`: [`Config::token`] is for static and API peers
	/// only.
	#[cfg(feature = "cluster-lan")]
	async fn run_mdns(self, dialed: DialMap, mut discovery: moq_tokio::mdns::Discovery) -> anyhow::Result<()> {
		use moq_tokio::mdns::Event;

		// Logged by key, never by the target's URL, so an inline query stays out of
		// the logs. Empty token: LAN authenticates with the mDNS credential.
		let mut spawn = |target: DialTarget| {
			tracing::info!(peer = %target.key, "dialing LAN cluster peer");
			tokio::spawn(self.clone().supervise_remote(target, String::new())).abort_handle()
		};

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
					dialed.upsert(target, DialSource::Mdns, &mut spawn);
				}
				Event::Lost(id) => dialed.release(&canonicalize_peer_key(&id), DialSource::Mdns, &mut spawn),
				_ => continue,
			}
		}

		// Discovery only ends if the daemon or its channel died. The relay would
		// otherwise keep serving, still reporting ready, while permanently blind to
		// LAN peers. Startup refuses to come up without mDNS, so losing it later is
		// the same failure arriving late.
		anyhow::bail!("LAN discovery stopped unexpectedly");
	}

	/// Drive `--cluster-connect-api`: an http(s) URL is polled, a local path (or
	/// `file://` URL) is watched for changes. Either way the source yields a JSON
	/// array of peers that's reconciled into the shared dial map.
	async fn run_connect_api(self, source: String, node: Option<String>, token: String, dialed: DialMap) {
		match Url::parse(&source) {
			Ok(url) if matches!(url.scheme(), "http" | "https") => {
				// Validated in `run`: an http(s) source has client TLS attached.
				let tls = self
					.client_tls
					.as_ref()
					.expect("http(s) connect_api source requires client TLS");
				let http = match crate::http_client::build(tls) {
					Ok(http) => http,
					Err(err) => {
						tracing::error!(%err, "cluster.connect_api: failed to build HTTP client");
						return;
					}
				};
				self.run_connect_api_http(url, node, token, dialed, http).await;
			}
			Ok(url) if url.scheme() == "file" => match url.to_file_path() {
				Ok(path) => self.run_connect_api_file(path, node, token, dialed).await,
				Err(()) => tracing::error!(%source, "cluster.connect_api file URL is not a valid local path"),
			},
			// Anything that isn't a URL we recognize is treated as a filesystem path.
			_ => {
				self.run_connect_api_file(PathBuf::from(&source), node, token, dialed)
					.await
			}
		}
	}

	/// Poll an http(s) endpoint for the peer list on a fixed cadence
	/// ([`CONNECT_API_POLL_INTERVAL`]). Freshness is the HTTP cache middleware's job:
	/// while the cached list is still fresh, `send` is served from cache with no
	/// network round-trip; once it's stale the middleware issues a conditional GET
	/// (`ETag` / `Last-Modified`) and serves the cached body if revalidation fails.
	/// Fails static: a failed fetch logs and keeps the current dials rather than
	/// tearing the cluster down.
	async fn run_connect_api_http(
		&self,
		url: Url,
		node: Option<String>,
		token: String,
		dialed: DialMap,
		http: ClientWithMiddleware,
	) {
		let mut tick = tokio::time::interval(CONNECT_API_POLL_INTERVAL);
		// A slow fetch must not bank missed ticks into a catch-up burst; just resume
		// the cadence from the next whole interval.
		tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
		loop {
			tick.tick().await;

			let mut req_url = url.clone();
			if let Some(node) = &node {
				req_url.query_pairs_mut().append_pair("node", node);
			}

			match Self::fetch_peer_list(&http, req_url).await {
				Ok(list) => self.apply_peer_list(list, &node, &token, &dialed),
				Err(err) => tracing::warn!(%err, "cluster.connect_api fetch failed; keeping current peers"),
			}
		}
	}

	/// Watch a local peer-list file, reconciling whenever it changes. Backed by
	/// [`moq_tokio::watch::Files`] (OS notifications with a polling fallback).
	/// Fails static: a missing or malformed file keeps the current dials, and the
	/// next change triggers a fresh attempt.
	async fn run_connect_api_file(&self, path: PathBuf, node: Option<String>, token: String, dialed: DialMap) {
		self.reload_connect_api_file(&path, &node, &token, &dialed);

		let mut watcher = match moq_tokio::watch::Files::new(std::slice::from_ref(&path)) {
			Ok(watcher) => watcher,
			Err(err) => {
				tracing::error!(%err, ?path, "failed to watch cluster.connect_api file; updates disabled");
				return;
			}
		};

		loop {
			watcher.changed().await;
			self.reload_connect_api_file(&path, &node, &token, &dialed);
		}
	}

	/// Re-read the peer-list file and reconcile. Any read/parse error keeps the
	/// current dials; the [`Files`](moq_tokio::watch::Files) only
	/// re-invokes this on a real change, so a malformed file isn't re-warned on a
	/// loop.
	fn reload_connect_api_file(&self, path: &std::path::Path, node: &Option<String>, token: &str, dialed: &DialMap) {
		match std::fs::read_to_string(path) {
			Ok(body) => match serde_json::from_str::<Vec<Peer>>(&body) {
				Ok(list) => self.apply_peer_list(list, node, token, dialed),
				Err(err) => {
					tracing::warn!(%err, ?path, "cluster.connect_api file is not a JSON array of peers; keeping current peers")
				}
			},
			Err(err) => tracing::warn!(%err, ?path, "failed to read cluster.connect_api file; keeping current peers"),
		}
	}

	/// Fetch and parse the peer list. Caching, conditional revalidation, and
	/// stale-if-error are handled by the HTTP cache middleware on `http`, so this
	/// just issues the request and parses the (possibly cache-served) body.
	async fn fetch_peer_list(http: &ClientWithMiddleware, url: Url) -> anyhow::Result<Vec<Peer>> {
		let body = http
			.get(url)
			.send()
			.await
			.context("cluster.connect_api request failed")?
			.error_for_status()
			.context("cluster.connect_api returned an error status")?
			.text()
			.await
			.context("failed to read cluster.connect_api body")?;

		serde_json::from_str(&body).context("cluster.connect_api response is not a JSON array of peers")
	}

	/// Reconcile a freshly fetched peer list into the dial map: dial peers that
	/// are new and drop API peers that disappeared. The relay's own [`node`] URL
	/// is filtered out so it never dials itself.
	fn apply_peer_list(&self, list: Vec<Peer>, node: &Option<String>, token: &str, dialed: &DialMap) {
		// Dedupe against the shared dial map (and filter out self) on stable identity,
		// while retaining the full dial configuration so a cost or credential update
		// replaces the existing session.
		let desired = match parse_peer_list(list, node.as_deref()) {
			Ok(desired) => desired,
			Err(err) => {
				tracing::warn!(%err, "invalid cluster.connect_api peer list; keeping current peers");
				return;
			}
		};

		dialed.reconcile_api(&desired, |target| {
			tracing::info!(peer = %target.key, "cluster.connect_api peer; dialing");
			let handle = tokio::spawn(self.clone().supervise_remote(target, token.to_string()));
			handle.abort_handle()
		});
	}

	async fn supervise_remote(self, target: DialTarget, token: String) {
		let log_peer = target.key.clone();
		if let Err(err) = self.run_remote(&target, token).await {
			tracing::warn!(%err, peer = %log_peer, "cluster peer connection ended");
		}
	}

	#[tracing::instrument("remote", skip_all, err, fields(remote = %target.key))]
	async fn run_remote(self, target: &DialTarget, token: String) -> anyhow::Result<()> {
		let mut urls = target.addrs();
		let cost = target.cost;
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

		let base_backoff = tokio::time::Duration::from_secs(1);
		let max_backoff = tokio::time::Duration::from_secs(300);
		// Sessions shorter than this are treated as churn: we keep backing off
		// instead of resetting, otherwise a peer that rejects us instantly would
		// turn into a tight reconnect loop.
		let stable_threshold = tokio::time::Duration::from_secs(10);

		let mut backoff = base_backoff;

		loop {
			let started = tokio::time::Instant::now();
			let result = self
				.run_remote_once(&addrs, cost, target.lan, target.fingerprint.as_deref())
				.await;
			let elapsed = started.elapsed();

			match result {
				Ok(()) if elapsed >= stable_threshold => backoff = base_backoff,
				Ok(()) => {
					tracing::warn!(?elapsed, "cluster peer session closed cleanly but quickly; backing off");
					backoff = (backoff * 2).min(max_backoff);
				}
				Err(err) => {
					tracing::warn!(%err, "cluster peer error; will retry");
					backoff = (backoff * 2).min(max_backoff);
				}
			}

			tokio::time::sleep(backoff).await;
		}
	}

	async fn run_remote_once(
		&self,
		addrs: &moq_tokio::Addrs,
		cost: Option<u64>,
		lan: bool,
		fingerprint: Option<&str>,
	) -> anyhow::Result<()> {
		// Each attempt is its own session, so it gets its own id. Matches the span an
		// accepted connection runs under, so both directions log the same way.
		let id = self.next_connection_id();
		self.run_remote_session(id, addrs, cost, lan, fingerprint)
			.instrument(tracing::info_span!("conn", id))
			.await
	}

	async fn run_remote_session(
		&self,
		id: u64,
		addrs: &moq_tokio::Addrs,
		cost: Option<u64>,
		lan: bool,
		fingerprint: Option<&str>,
	) -> anyhow::Result<()> {
		// The peer URL may carry the cluster JWT in its query, so neither the log
		// line nor the node label below may show the raw URL.
		let first = addrs.as_slice().first().expect("Addrs is non-empty").url();
		let redacted = moq_tokio::RedactedUrl::new(first);
		tracing::info!(url = %redacted, "dialing cluster peer");

		let client = if lan {
			self.lan_client(fingerprint)?
		} else {
			self.client
				.clone()
				.context("internal: cluster peer dial without an attached QUIC client")?
		};

		// Cluster dials use their configured stats tier. Cluster peers carry no auth
		// root, so presence is keyed under the empty root within the cluster tier.
		// The peer's routes entered the cluster elsewhere. A peer that predates the
		// hidden opt-in still discovers our hidden routes; see `Cluster::scope`.
		let origin = self.origin.clone().peer();
		let mut client = client
			.with_publisher(origin.consume().with_hidden(true))
			.with_subscriber(origin)
			.with_stats(self.stats.tier(self.cluster_tier()).session(""));
		if let Some(cost) = cost {
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
		// Forced on regardless of `--client-reconnect`: that flag is about the
		// relay's own upstream dial, and a cluster peer link that stopped following
		// GOAWAY would break rolling handoff, redialing the drained URL after the
		// old session finally closed instead of migrating to the replacement.
		let mut reconnect = client.with_reconnect(true).connect(addrs.clone());
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

	/// A client for one LAN dial: request-path versions, ephemeral bind, and
	/// the advertised fingerprint pinned on a clean TLS config so the relay's
	/// CA roots cannot combine with it.
	fn lan_client(&self, fingerprint: Option<&str>) -> anyhow::Result<moq_tokio::Client> {
		let mut connect = self
			.connect
			.clone()
			.context("internal: LAN dial without Cluster::with_connect")?;
		connect.backoff.timeout = std::time::Duration::ZERO;
		connect.once = Some(false);
		let mut bind = connect.resolve().bind;
		bind.set_port(0);
		connect.bind = Some(bind);
		connect.version = connect
			.versions()
			.iter()
			.filter(|version| Self::carries_request_path(version))
			.copied()
			.collect();
		if let Some(fingerprint) = fingerprint {
			connect.tls = moq_tokio::tls::Connect::default();
			connect.tls.fingerprint = vec![fingerprint.to_string()];
		}
		let quic = self.quic.clone().unwrap_or_default();
		Ok(connect.init(quic)?)
	}

	/// Start a reconnecting LAN session from discovered details. Tests wire this
	/// without multicast.
	#[cfg(all(test, feature = "cluster-lan"))]
	fn dial_lan_target(&self, target: &DialTarget) -> anyhow::Result<moq_tokio::Connection> {
		let addrs = moq_tokio::Addrs::collect(target.addrs()).context("peer advertised no reachable address")?;
		let mut client = self
			.lan_client(target.fingerprint.as_deref())?
			.with_origin(self.origin.clone().peer());
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
	secret: Option<&str>,
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
		let secret = moq_tokio::mdns::Secret::load(secret).context("invalid --cluster-lan-secret")?;
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

/// Whether a `--cluster-connect-api` source is an http(s) URL (otherwise it's
/// treated as a local file path, which needs no TLS client).
fn connect_api_is_http(source: &str) -> bool {
	Url::parse(source).is_ok_and(|url| matches!(url.scheme(), "http" | "https"))
}

/// Resolve a cluster peer to the URL we dial.
///
/// The modern form is a full URL, e.g. `https://host/?jwt=TOKEN`, which is used
/// verbatim. A bare host or `host:port` is wrapped in `https://.../` for
/// `--cluster-connect-api` lists; user-supplied `--cluster-connect` entries
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

/// Deserialize a field that accepts either a TOML boolean or string into an
/// `Option<String>` (booleans become `"true"` / `"false"`). Lets the removed
/// `cluster.mesh` parse in both its released forms, `mesh = true` and
/// `mesh = "<url>"`, so it is refused by name rather than as an unknown type.
fn deserialize_bool_or_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
	D: serde::Deserializer<'de>,
{
	use serde::Deserialize as _;

	#[derive(serde::Deserialize)]
	#[serde(untagged)]
	enum BoolOrString {
		Bool(bool),
		Str(String),
	}

	Ok(
		Option::<BoolOrString>::deserialize(deserializer)?.map(|value| match value {
			BoolOrString::Bool(value) => value.to_string(),
			BoolOrString::Str(value) => value,
		}),
	)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::Config as RelayConfig;

	/// The next route and whether it is active, skipping the caught-up marker.
	async fn next_update(announced: &mut moq_net::announce::Consumer) -> Option<(moq_net::announce::Announce, bool)> {
		loop {
			return match announced.next().await? {
				moq_net::announce::Event::Start(route) | moq_net::announce::Event::Update(route) => Some((route, true)),
				moq_net::announce::Event::End(route) => Some((route, false)),
				moq_net::announce::Event::Live => continue,
			};
		}
	}

	/// The next announcement without blocking, skipping the caught-up marker.
	fn try_next_announced(announced: &mut moq_net::announce::Consumer) -> Option<moq_net::announce::Announce> {
		loop {
			return match announced.try_next()? {
				moq_net::announce::Event::Start(route) => Some(route),
				moq_net::announce::Event::Live => continue,
				other => panic!("expected an announcement: got {other:?}"),
			};
		}
	}

	fn new_cluster(config: Config) -> anyhow::Result<Cluster> {
		Cluster::new(Options::new(config))
	}

	/// A grant's mount reaches the target for subscribe and refuses publish, both
	/// named from the session's root.
	#[tokio::test]
	async fn session_handles_apply_grant_mounts() {
		let cluster = new_cluster(Config::default()).expect("cluster");
		let everything = || moq_net::Patterns::from(moq_net::Pattern::all());
		let mut grant = moq_auth::Grant::new(everything(), everything());
		grant.mounts.insert(".svc".into(), ".svc/pid".into());
		let token = auth::Token::new("/pid", &grant);

		let _worker = cluster
			.origin
			.publish(".svc/pid/foo", origin::Route::default())
			.expect("publish at the fleet path");
		let subscriber = cluster.subscriber(&token).expect("subscribe grant").consume();
		let broadcast = subscriber
			.request_broadcast(".svc/foo")
			.await
			.expect("resolves through the mount");
		assert_eq!(broadcast.info().path.as_str(), ".svc/foo");

		let publisher = cluster.publisher(&token).expect("publish grant");
		assert!(publisher.create_broadcast(".svc/foo").is_err());
		publisher.create_broadcast("cam").expect("publish outside the mount");

		// A mount the origin refuses admits nothing rather than half a grant.
		grant.mounts.insert(".svc/x".into(), ".other".into());
		let token = auth::Token::new("/pid", &grant);
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
			let token = auth::Token::new("/pid", &grant);
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
	#[tokio::test]
	async fn stats_publishing_outlives_the_producer_handle() {
		let config = crate::stats::Config {
			enabled: true,
			node: Some("test".to_string()),
			..Default::default()
		};

		let cluster = new_cluster(Config::default()).expect("cluster");
		let stats = config.build(cluster.origin.clone());
		let cluster = cluster.with_stats(stats);

		let path = moq_net::Path::new(".stats").join("node").join("test");
		let consumer = cluster.origin.consume();
		tokio::time::timeout(std::time::Duration::from_secs(5), consumer.routed(&path))
			.await
			.expect("stats broadcast announced within 5s")
			.expect("stats broadcast present");
		let broadcast = consumer
			.request_broadcast(&path)
			.await
			.expect("stats broadcast resolves");

		// Several publish intervals (1s each by default) after the handle went out
		// of scope, the task is still running and its broadcast still open. The
		// re-check is bounded: without the keepalive the broadcast is gone and
		// `routed` waits forever, which must read as a failure rather
		// than a hung test.
		tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
		let still_live = tokio::time::timeout(std::time::Duration::from_secs(2), consumer.routed(&path)).await;
		assert!(
			matches!(still_live, Ok(Some(_))),
			"stats broadcast unannounced after the producer handle was dropped: \
			 the publish task stopped while the cluster kept serving"
		);
		drop(broadcast);
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
		assert_eq!(cluster.cluster_tier(), Tier::new("region/sjc"));
	}

	/// Stand-in dial task: never makes progress, exposes an AbortHandle.
	fn placeholder_handle() -> AbortHandle {
		tokio::spawn(std::future::pending::<()>()).abort_handle()
	}

	fn target(key: &str) -> DialTarget {
		DialTarget {
			key: key.to_string(),
			url: Url::parse(&format!("https://{key}/")).expect("test URL"),
			urls: Vec::new(),
			cost: None,
			fingerprint: None,
			lan: false,
		}
	}

	fn desired(keys: &[&str]) -> HashMap<String, DialTarget> {
		keys.iter().map(|key| ((*key).to_string(), target(key))).collect()
	}

	/// A peer wanted by both a static seed and the API survives losing either
	/// source: the dial is only torn down once the last source releases it.
	#[tokio::test]
	async fn multi_source_peer_survives_until_last_release() {
		let dialed = DialMap::default();
		// Seeded first, then also appears in the API list.
		dialed.insert(target("both:4443"), placeholder_handle(), DialSource::Static);
		dialed.reconcile_api(&desired(&["both:4443"]), |_| panic!("already dialed"));

		// Dropped from the API list -> still wanted by the seed.
		dialed.reconcile_api(&HashMap::new(), |_| panic!("static dial stays active"));
		assert!(dialed.contains("both:4443"), "the seed still wants it");

		dialed.release("both:4443", DialSource::Static, &mut |_| panic!("must not redial"));
		assert!(!dialed.contains("both:4443"));
	}

	/// `insert` for an already-dialed peer merges the source onto the existing
	/// entry (and aborts the redundant handle) rather than opening a second dial.
	#[tokio::test]
	async fn insert_merges_redundant_dial() {
		let dialed = DialMap::default();
		dialed.insert(target("p:4443"), placeholder_handle(), DialSource::Static);
		dialed.insert(target("p:4443"), placeholder_handle(), DialSource::Api);

		// Dropping the API source leaves the static source holding the dial.
		dialed.reconcile_api(&HashMap::new(), |_| panic!("static dial stays active"));
		assert!(dialed.contains("p:4443"), "static source still holds the dial");
	}

	/// `reconcile_api` drops API dials missing from the desired set, reports the
	/// newly desired ones for the caller to spawn, and never touches Static dials
	/// (even when they're absent from the API list).
	#[tokio::test]
	async fn reconcile_api_adds_and_removes_only_api() {
		let dialed = DialMap::default();
		dialed.insert(target("static:4443"), placeholder_handle(), DialSource::Static);
		dialed.insert(target("api-keep:4443"), placeholder_handle(), DialSource::Api);
		dialed.insert(target("api-drop:4443"), placeholder_handle(), DialSource::Api);

		// Desired: keep one existing API peer, drop the other, add a new one.
		// The static peer is not in the list but must survive.
		let mut to_add = Vec::new();
		dialed.reconcile_api(&desired(&["api-keep:4443", "api-new:4443"]), |target| {
			to_add.push(target.key);
			placeholder_handle()
		});
		to_add.sort();

		assert_eq!(to_add, vec!["api-new:4443".to_string()]);
		assert!(dialed.contains("api-keep:4443"));
		assert!(!dialed.contains("api-drop:4443"), "dropped API peer must be removed");
		assert!(dialed.contains("static:4443"), "static peer must survive reconcile");
	}

	/// A peer already dialed via another source is not re-reported for dialing,
	/// so the API reconcile can't open a duplicate connection.
	#[tokio::test]
	async fn reconcile_api_dedupes_against_other_sources() {
		let dialed = DialMap::default();
		dialed.insert(target("shared:4443"), placeholder_handle(), DialSource::Static);

		dialed.reconcile_api(&desired(&["shared:4443"]), |_| panic!("already dialed"));
		assert!(dialed.contains("shared:4443"));
	}

	/// A secondary source can update its target without disturbing the source that
	/// opened the session. If the active source disappears, its latest fallback
	/// configuration is used for the replacement.
	#[tokio::test]
	async fn inactive_source_update_applies_on_takeover() {
		let dialed = DialMap::default();
		let seeded = DialTarget::parse("https://peer.example/?cost=1").unwrap();
		let api = DialTarget::parse("https://peer.example/?cost=3").unwrap();
		let old_task = tokio::spawn(std::future::pending::<()>());
		dialed.insert(seeded.clone(), old_task.abort_handle(), DialSource::Static);

		let desired = [(api.key.clone(), api.clone())].into_iter().collect();
		dialed.reconcile_api(&desired, |_| panic!("inactive source must not redial"));
		assert!(!old_task.is_finished());

		let mut spawned = Vec::new();
		dialed.release(&seeded.key, DialSource::Static, &mut |target| {
			spawned.push(target);
			placeholder_handle()
		});

		tokio::task::yield_now().await;
		assert!(old_task.is_finished(), "old source must be aborted");
		assert_eq!(spawned.len(), 1);
		assert!(spawned[0] == api);
	}

	/// If the fallback source requests the same target, ownership transfers without
	/// interrupting the healthy session.
	#[tokio::test]
	async fn identical_fallback_takeover_keeps_task() {
		let dialed = DialMap::default();
		let target = DialTarget::parse("https://peer.example/?cost=1").unwrap();
		let task = tokio::spawn(std::future::pending::<()>());
		dialed.insert(target.clone(), task.abort_handle(), DialSource::Static);

		let desired = [(target.key.clone(), target.clone())].into_iter().collect();
		dialed.reconcile_api(&desired, |_| panic!("inactive source must not redial"));

		dialed.release(&target.key, DialSource::Static, &mut |_| {
			panic!("identical fallback must not redial")
		});

		assert!(!task.is_finished(), "healthy dial must be preserved");
		let map = dialed.inner.lock().expect("dial map");
		let entry = map.get(&target.key).expect("current dial");
		assert_eq!(entry.active, DialSource::Api);
		assert!(entry.sources.get(entry.active) == Some(&target));
		drop(map);
		task.abort();
	}

	/// A cost-only API update has the same identity but different SETUP input, so
	/// it must replace the live task instead of being mistaken for an unchanged
	/// peer.
	#[tokio::test]
	async fn reconcile_api_replaces_cost_only_change() {
		let dialed = DialMap::default();
		let old = DialTarget::parse("https://peer.example/?cost=1").unwrap();
		let new = DialTarget::parse("https://peer.example/?cost=2").unwrap();
		assert_eq!(old.key, new.key);
		assert!(old != new);

		let old_task = tokio::spawn(std::future::pending::<()>());
		dialed.insert(old, old_task.abort_handle(), DialSource::Api);
		let desired = [(new.key.clone(), new.clone())].into_iter().collect();
		let mut spawned = Vec::new();
		dialed.reconcile_api(&desired, |target| {
			spawned.push(target);
			placeholder_handle()
		});

		tokio::task::yield_now().await;
		assert!(old_task.is_finished(), "old dial must be aborted");
		assert_eq!(spawned.len(), 1);
		assert!(spawned[0] == new);
	}

	/// An identical API render keeps the live task, avoiding connection churn.
	#[tokio::test]
	async fn reconcile_api_identical_target_is_noop() {
		let dialed = DialMap::default();
		let target = DialTarget::parse("https://peer.example/?cost=2&jwt=secret").unwrap();
		let task = tokio::spawn(std::future::pending::<()>());
		dialed.insert(target.clone(), task.abort_handle(), DialSource::Api);
		let desired = [(target.key.clone(), target)].into_iter().collect();

		dialed.reconcile_api(&desired, |_| panic!("identical target must not redial"));
		assert!(!task.is_finished(), "live dial must be preserved");
		task.abort();
	}

	/// Inline credentials are dial-affecting even though they are excluded from
	/// peer identity, so rotating one replaces the session too.
	#[tokio::test]
	async fn reconcile_api_replaces_inline_credential() {
		let dialed = DialMap::default();
		let old = DialTarget::parse("https://peer.example/?jwt=old").unwrap();
		let new = DialTarget::parse("https://peer.example/?jwt=new").unwrap();
		assert_eq!(old.key, new.key);

		let old_task = tokio::spawn(std::future::pending::<()>());
		dialed.insert(old, old_task.abort_handle(), DialSource::Api);
		let desired = [(new.key.clone(), new.clone())].into_iter().collect();
		let mut spawned = Vec::new();
		dialed.reconcile_api(&desired, |target| {
			spawned.push(target);
			placeholder_handle()
		});

		tokio::task::yield_now().await;
		assert!(old_task.is_finished(), "old dial must be aborted");
		assert_eq!(spawned.len(), 1);
		assert!(spawned[0] == new);
	}

	/// A malformed replacement is rejected before reconciliation, so it cannot
	/// tear down or reconfigure the last-known-good dial set.
	#[tokio::test]
	async fn malformed_peer_list_preserves_current_dial() {
		let cluster = new_cluster(Config::default()).expect("cluster");
		let dialed = DialMap::default();
		let current = DialTarget::parse("https://peer.example/?cost=1").unwrap();
		let task = tokio::spawn(std::future::pending::<()>());
		dialed.insert(current.clone(), task.abort_handle(), DialSource::Api);

		cluster.apply_peer_list(
			vec![Peer::new("https://peer.example/?cost=invalid")],
			&None,
			"",
			&dialed,
		);

		assert!(!task.is_finished(), "last-known-good dial must stay active");
		let map = dialed.inner.lock().expect("dial map");
		let entry = map.get(&current.key).expect("current dial");
		assert!(entry.sources.get(entry.active) == Some(&current));
		task.abort();
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
			#[allow(dead_code)]
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
		let peer = Peer::new("https://peer.example/").with_cost(2).with_token("secret");
		let debug = format!("{peer:?}");
		assert!(!debug.contains("secret"), "Debug must not leak the credential: {debug}");
		assert!(
			debug.contains("https://peer.example/"),
			"Debug keeps the address: {debug}"
		);
	}

	/// Static `--cluster-connect` entries get the same all-or-nothing
	/// validation as `--cluster-connect-api` lists: a repeated identity with
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

	/// A malformed object entry keeps the last-good dials, exactly like a
	/// malformed URL does.
	#[tokio::test]
	async fn malformed_object_peer_list_preserves_current_dial() {
		let cluster = new_cluster(Config::default()).expect("cluster");
		let dialed = DialMap::default();
		let current = DialTarget::from_peer(&Peer::new("https://peer.example/?cost=1")).unwrap();
		let task = tokio::spawn(std::future::pending::<()>());
		dialed.insert(current.clone(), task.abort_handle(), DialSource::Api);

		let asymmetric = Peer::new("https://peer.example/").with_cost(1).with_egress(2);
		cluster.apply_peer_list(vec![asymmetric], &None, "", &dialed);

		assert!(!task.is_finished(), "last-known-good dial must stay active");
		let map = dialed.inner.lock().expect("dial map");
		let entry = map.get(&current.key).expect("current dial");
		assert!(entry.sources.get(entry.active) == Some(&current));
		task.abort();
	}

	/// Gossip discovery is removed, so every released `--cluster-mesh` form, the
	/// boolean and the older self-URL, stops construction and names the
	/// replacement instead of leaving a relay without the peers it expected.
	#[tokio::test]
	async fn removed_mesh_is_refused() {
		for value in ["true", "false", "rendezvous.example.com:4443"] {
			let err = new_cluster(Config {
				mesh: Some(value.to_string()),
				..Default::default()
			})
			.err()
			.expect("mesh must be refused");
			let msg = format!("{err}");
			assert!(msg.contains("--cluster-mesh / MOQ_CLUSTER_MESH"), "{value}: {msg}");
			assert!(msg.contains("--cluster-connect"), "{value}: {msg}");
		}
	}

	/// A valid `cluster.id` is used verbatim as the relay's Hop ID, giving the
	/// node a stable identity across restarts.
	#[tokio::test]
	async fn cluster_id_sets_origin() {
		let cluster = new_cluster(Config {
			id: Some(42),
			..Default::default()
		})
		.expect("valid id");
		assert_eq!(cluster.origin.hop().id(), 42);
	}

	/// Cache settings land on the one origin serving and stats share. A handle
	/// cloned at construction stays on that origin.
	#[tokio::test]
	async fn constructed_origin_keeps_cache_and_handles() {
		let duration = Duration::from_secs(5);
		let mut cache = crate::cache::Config::default();
		cache.duration = Some(duration);
		let cache = cache.init().expect("cache");
		let pool = cache.pool.clone();

		let cluster = Cluster::new(
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

		let stats = crate::stats::Config {
			enabled: true,
			node: Some("test".to_string()),
			..Default::default()
		}
		.build(origin.clone());
		let cluster = cluster.with_stats(stats);

		assert_eq!(cluster.origin.hop().id(), origin.hop().id());
		assert_eq!(cluster.origin.config().cache_duration, duration);
		assert_eq!(cluster.origin.config().pool.expiry(), Some(duration));

		let broadcast = origin.create_broadcast("cam").expect("create");
		broadcast.announce(Default::default()).expect("announce");
		let mut track = broadcast.create_track("data", None).expect("track");
		track.write_frame(moq_net::Timestamp::ZERO, b"hello").expect("write");
		assert!(pool.used() > 0, "writes charge the constructed cache pool");

		let consumer = cluster.origin.consume();
		tokio::time::timeout(Duration::from_secs(2), consumer.request_broadcast("cam"))
			.await
			.expect("broadcast resolves")
			.expect("broadcast present");

		let stats_path = moq_net::Path::new(".stats").join("node").join("test");
		tokio::time::timeout(Duration::from_secs(5), consumer.routed(&stats_path))
			.await
			.expect("stats announced")
			.expect("stats present");
	}

	/// A reserved (0) or out-of-range (>= 2^62) `cluster.id` is rejected rather
	/// than producing an unencodable hop id.
	#[test]
	fn cluster_id_out_of_range_errors() {
		for bad in [0, 1u64 << 62] {
			let err = new_cluster(Config {
				id: Some(bad),
				..Default::default()
			})
			.err()
			.expect("should error");
			assert!(format!("{err}").contains("--cluster-id"), "got: {err}");
		}
	}

	/// `cluster.node` (identity) round-trips through TOML and survives the CLI
	/// re-parse when no flags override it.
	#[test]
	fn cluster_node_round_trip() {
		// Usage reads the environment while parsing, so serialize with the tests
		// that mutate it.
		let _env = crate::test_env::EnvGuard::lock();

		let toml = "[cluster]\nnode = \"us-east.example.com:4443\"\nconnect = [\"https://root.example.com:4443/\"]\n";
		let dir = std::env::temp_dir().join("moq-relay-cluster-test");
		std::fs::create_dir_all(&dir).unwrap();
		let path = dir.join("cluster-node-toml.toml");
		std::fs::write(&path, toml).unwrap();

		let args = vec![std::ffi::OsString::from("moq-relay"), std::ffi::OsString::from(&path)];
		let config = RelayConfig::parse_and_merge(args).expect("config load");
		assert_eq!(config.cluster.node.as_deref(), Some("us-east.example.com:4443"));
		assert_eq!(
			config.cluster.connect,
			vec![Peer::new("https://root.example.com:4443/")]
		);
	}

	/// A TOML `mesh` in either released type is refused by name at load, so a
	/// config file that relied on gossip stops instead of starting without peers.
	#[test]
	fn toml_mesh_is_refused() {
		// Usage reads the environment while parsing, so serialize with the tests
		// that mutate it.
		let _env = crate::test_env::EnvGuard::lock();

		let dir = std::env::temp_dir().join("moq-relay-cluster-test");
		std::fs::create_dir_all(&dir).unwrap();
		for (name, value) in [("bool", "true"), ("url", "\"us-east.example.com:4443\"")] {
			let path = dir.join(format!("cluster-mesh-{name}-toml.toml"));
			std::fs::write(&path, format!("[cluster]\nmesh = {value}\n")).unwrap();

			let args = vec![std::ffi::OsString::from("moq-relay"), std::ffi::OsString::from(&path)];
			let err = RelayConfig::parse_and_merge(args).expect_err("mesh must be refused");
			assert!(err.to_string().contains("--cluster-mesh"), "{name}: {err}");
		}
	}

	/// Static config accepts the object form beside bare URLs, through the same
	/// type the connect API parses.
	#[test]
	fn cluster_connect_object_form_round_trip() {
		// Usage reads the environment while parsing, so serialize with the tests
		// that mutate it.
		let _env = crate::test_env::EnvGuard::lock();

		let toml = "[cluster]\nconnect = [\"https://a.example/?cost=1\", { url = \"https://b.example/\", cost = 2, egress = 2, token = \"secret\" }]\n";
		let dir = std::env::temp_dir().join("moq-relay-cluster-test");
		std::fs::create_dir_all(&dir).unwrap();
		let path = dir.join("cluster-connect-object-toml.toml");
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
					.with_token("secret"),
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

	/// Linger and a bare `--cluster-connect` host still parse so the process can
	/// name what replaced them, but they configure nothing and must stop a run.
	#[test]
	fn released_cluster_spellings_are_reported_not_applied() {
		let config = Config {
			linger: Some(std::time::Duration::from_secs(5)),
			connect: vec![Peer::new("root.example.com:4443")],
			..Default::default()
		};
		let reported = config.deprecated().to_string();
		assert!(reported.contains("--cluster-linger / MOQ_CLUSTER_LINGER"), "{reported}");
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
	#[tokio::test]
	async fn canonicalize_peer_key_dedupes_spellings() {
		let key = canonicalize_peer_key("host:4443");
		assert_eq!(key, "https://host:4443/");
		assert_eq!(canonicalize_peer_key("https://host:4443/"), key);
		assert_eq!(canonicalize_peer_key("https://host:4443/?jwt=abc"), key);

		// A URL form and the legacy host:port form dedupe against each other.
		let dialed = DialMap::default();
		dialed.insert(
			DialTarget::parse("https://host:4443/?jwt=abc").unwrap(),
			placeholder_handle(),
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
		// Usage reads the environment while parsing, so serialize with the tests
		// that mutate it.
		let _env =
			crate::test_env::EnvGuard::clear(&["MOQ_CLUSTER_LAN", "MOQ_CLUSTER_LAN_SECRET", "MOQ_CLUSTER_LAN_APP"]);

		let toml = "[cluster]\nnode = \"https://relay.example.com\"\n\n[cluster.lan]\nenabled = true\nsecret = \"cluster.key\"\napp = \"custom\"\n";
		let dir = std::env::temp_dir().join("moq-relay-cluster-test");
		std::fs::create_dir_all(&dir).unwrap();
		let path = dir.join("cluster-lan-toml.toml");
		std::fs::write(&path, toml).unwrap();

		let args = vec![std::ffi::OsString::from("moq-relay"), std::ffi::OsString::from(&path)];
		let config = RelayConfig::parse_and_merge(args).expect("config load");
		assert!(config.cluster.lan.enabled);
		assert_eq!(config.cluster.lan.secret.as_deref(), Some("cluster.key"));
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
		let _env =
			crate::test_env::EnvGuard::clear(&["MOQ_CLUSTER_LAN", "MOQ_CLUSTER_LAN_SECRET", "MOQ_CLUSTER_LAN_APP"]);

		let toml = "[cluster.lan]\nenabled = true\nsecret = \"from-toml.key\"\napp = \"from-toml\"\n";
		let dir = std::env::temp_dir().join("moq-relay-cluster-test");
		std::fs::create_dir_all(&dir).unwrap();
		let path = dir.join("cluster-lan-override.toml");
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
		assert_eq!(config.cluster.lan.secret.as_deref(), Some("from-cli.key"));
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
	#[tokio::test]
	async fn mdns_release_respects_the_other_sources() {
		let dialed = DialMap::default();
		let peer = target("peer:4443");
		dialed.insert(peer.clone(), placeholder_handle(), DialSource::Mdns);
		dialed.insert(peer.clone(), placeholder_handle(), DialSource::Static);

		// The static seed wants the same target, so the takeover reuses the dial.
		dialed.release(&peer.key, DialSource::Mdns, &mut |_| panic!("must not redial"));
		assert!(dialed.contains(&peer.key), "the static seed still wants it");

		dialed.release(&peer.key, DialSource::Static, &mut |_| panic!("must not redial"));
		assert!(!dialed.contains(&peer.key), "the last release abandons the dial");

		// Releasing an unknown peer is a no-op, not a panic.
		dialed.release("never-dialed", DialSource::Mdns, &mut |_| panic!("must not redial"));
	}

	#[test]
	fn cluster_connect_api_survives_toml_merge() {
		// Usage reads the environment while parsing, so serialize with the tests
		// that mutate it.
		let _env = crate::test_env::EnvGuard::lock();

		let toml = "[cluster]\nconnect_api = \"https://api.example.com/cluster/connect\"\n";
		let dir = std::env::temp_dir().join("moq-relay-cluster-test");
		std::fs::create_dir_all(&dir).unwrap();
		let path = dir.join("cluster-connect-api-toml.toml");
		std::fs::write(&path, toml).unwrap();

		let args = vec![std::ffi::OsString::from("moq-relay"), std::ffi::OsString::from(&path)];
		let config = RelayConfig::parse_and_merge(args).expect("config load");
		assert_eq!(
			config.cluster.connect_api.as_deref(),
			Some("https://api.example.com/cluster/connect")
		);
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
			while let Some(request) = listener.accept().await {
				let conn = crate::Connection::new(request, accept.clone(), crate::auth::Auth::refuse("test"));
				tokio::spawn(async move {
					let _ = conn.run().await;
				});
			}
		});

		let mut connect = moq_tokio::connect::Config::default();
		connect.once = Some(true);
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
		};
		let _dial = fingerprint.dial_lan_target(&target).expect("dial");

		let mut announced = fingerprint.origin.consume().announced();
		let (update, _) = tokio::time::timeout(TIMEOUT, next_update(&mut announced))
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
			let (update, _) = tokio::time::timeout(TIMEOUT, next_update(&mut announced))
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

	/// A grant naming a cluster peer marks the session's routes as a peer's, so the
	/// relay's local view leaves them out; any other grant ingests here.
	#[tokio::test]
	async fn peer_grant_marks_the_session_publisher() {
		let cluster = new_cluster(Config::default()).expect("cluster");
		let all: moq_net::Patterns = [moq_net::Pattern::all()].into_iter().collect();

		let client = auth::Token::new("/", &moq_auth::Grant::new(all.clone(), all.clone()));
		let mut grant = moq_auth::Grant::new(all.clone(), all);
		grant.peer = true;
		let peer = auth::Token::new("/", &grant);

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
