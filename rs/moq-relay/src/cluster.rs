use std::{
	collections::HashMap,
	path::PathBuf,
	sync::{
		Arc, Mutex,
		atomic::{AtomicU64, Ordering},
	},
	time::{Duration, Instant},
};

use anyhow::Context;
use moq_net::origin;
use moq_net::{Origin, Path, stats::Tier};
use reqwest_middleware::ClientWithMiddleware;
use tokio::task::AbortHandle;
use tracing::Instrument as _;
use url::Url;

use crate::{AuthToken, nodes::MESH_PREFIX};

/// How often the discovery loop scans for stale entries.
const SWEEP_INTERVAL: Duration = Duration::from_secs(30);

/// How long a peer must stay unannounced before we abort the dial. Must clear the
/// "prefer shorter hop" re-announce flap (which arrives as
/// unannounce-then-announce within sub-milliseconds) plus reasonable churn from
/// a peer restart.
const STALE_AFTER: Duration = Duration::from_secs(60);

/// How often the relay re-checks an http(s) `--cluster-connect-api` endpoint. The
/// HTTP cache middleware suppresses the actual network round-trip while the cached
/// list is still fresh (per the response's `Cache-Control`), so this is the floor
/// on responsiveness, not on origin load: a tighter `max-age` means more of these
/// ticks turn into real conditional GETs.
const CONNECT_API_POLL_INTERVAL: Duration = Duration::from_secs(30);

/// Mesh tiebreaker for gossip-discovered peers. In a full mesh both peers
/// discover each other and would each open a dial, leaving two redundant
/// sessions. The session is bidirectional (we publish *and* consume on it), so
/// one suffices. Break the symmetry on URL order: only dial peers that sort
/// after us, making the lexicographically-smaller node the client and the
/// larger the server. The skipped side still gets the connection inbound.
fn should_dial(self_url: &str, peer: &str) -> bool {
	peer > self_url
}

/// One peer's stable identity and the dial-affecting configuration currently
/// supplied by a discovery source.
#[derive(Clone, PartialEq, Eq)]
struct DialTarget {
	key: String,
	url: Url,
	cost: Option<u64>,
}

impl DialTarget {
	fn parse(peer: &str) -> anyhow::Result<Self> {
		let mut url = peer_url(peer)?;
		let key = {
			let mut identity = url.clone();
			identity.set_query(None);
			identity.into()
		};
		let cost = take_cost(&mut url)?;
		Ok(Self { key, url, cost })
	}
}

/// Every currently-live gossip path for a canonical peer, in announcement order.
/// Query changes create a new path before the old path necessarily unannounces,
/// so the latest live path owns the dial configuration.
#[derive(Default)]
struct GossipTargets {
	by_key: HashMap<String, Vec<(String, DialTarget)>>,
}

enum GossipUpdate {
	/// The current advertisement changed to this target.
	Current(DialTarget),
	/// A non-current or unknown advertisement disappeared.
	Unchanged,
	/// The peer has no live advertisements left.
	Gone,
}

impl GossipTargets {
	fn announce(&mut self, advertisement: String, target: DialTarget) -> DialTarget {
		let targets = self.by_key.entry(target.key.clone()).or_default();
		targets.retain(|(existing, _)| existing != &advertisement);
		targets.push((advertisement, target.clone()));
		target
	}

	fn unannounce(&mut self, advertisement: &str, key: &str) -> GossipUpdate {
		let Some(targets) = self.by_key.get_mut(key) else {
			return GossipUpdate::Unchanged;
		};
		let was_current = targets.last().is_some_and(|(current, _)| current == advertisement);
		let previous_len = targets.len();
		targets.retain(|(existing, _)| existing != advertisement);
		if targets.len() == previous_len {
			return GossipUpdate::Unchanged;
		}
		if targets.is_empty() {
			self.by_key.remove(key);
			return GossipUpdate::Gone;
		}
		if was_current {
			return GossipUpdate::Current(targets.last().expect("live target exists").1.clone());
		}
		GossipUpdate::Unchanged
	}
}

/// Parse a complete dynamic peer list before mutating the live dial set. A bad
/// entry or conflicting duplicate rejects the whole update, preserving the
/// last-known-good topology.
fn parse_peer_list(list: Vec<String>, node: Option<&str>) -> anyhow::Result<HashMap<String, DialTarget>> {
	let self_key = node.map(canonicalize_peer_key);
	let mut desired = HashMap::new();
	for (index, peer) in list.into_iter().enumerate() {
		let target = DialTarget::parse(&peer).with_context(|| format!("invalid peer at index {index}"))?;
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
/// than one at once (e.g. gossiped *and* listed by `--cluster-connect-api`), so
/// [`DialEntry`] tracks a set of these and only tears the dial down when the last
/// one releases it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DialSource {
	/// Seeded from `--cluster-connect`. Never released, so the dial retries forever
	/// (operator intent says "always dial").
	Static,
	/// Discovered via gossip on `.internal/origins/*`. Released by the periodic
	/// stale-sweep once its unannounce has stuck for [`STALE_AFTER`].
	Gossip,
	/// Supplied by `--cluster-connect-api`. Released when a fetched peer list no
	/// longer contains the peer.
	Api,
	/// Discovered on the LAN via mDNS (`--cluster-lan`). Released as soon as the
	/// advertisement goes away, which (unlike gossip) is a definite signal rather
	/// than one that flaps.
	#[cfg(feature = "cluster-lan")]
	Mdns,
}

/// The set of [`DialSource`]s currently keeping a dial alive.
#[derive(Clone, Default)]
struct DialSources {
	seeded: Option<DialTarget>,
	gossip: Option<DialTarget>,
	api: Option<DialTarget>,
	#[cfg(feature = "cluster-lan")]
	mdns: Option<DialTarget>,
}

impl DialSources {
	fn get(&self, source: DialSource) -> Option<&DialTarget> {
		match source {
			DialSource::Static => self.seeded.as_ref(),
			DialSource::Gossip => self.gossip.as_ref(),
			DialSource::Api => self.api.as_ref(),
			#[cfg(feature = "cluster-lan")]
			DialSource::Mdns => self.mdns.as_ref(),
		}
	}

	fn set(&mut self, source: DialSource, target: DialTarget) {
		match source {
			DialSource::Static => self.seeded = Some(target),
			DialSource::Gossip => self.gossip = Some(target),
			DialSource::Api => self.api = Some(target),
			#[cfg(feature = "cluster-lan")]
			DialSource::Mdns => self.mdns = Some(target),
		}
	}

	fn clear(&mut self, source: DialSource) {
		match source {
			DialSource::Static => self.seeded = None,
			DialSource::Gossip => self.gossip = None,
			DialSource::Api => self.api = None,
			#[cfg(feature = "cluster-lan")]
			DialSource::Mdns => self.mdns = None,
		}
	}

	/// The source to fall back to when the active one is released, most durable
	/// first: operator intent, then the API list, then the two discovery
	/// mechanisms whose targets come and go on their own.
	fn fallback(&self) -> Option<(DialSource, &DialTarget)> {
		let fallback = self
			.seeded
			.as_ref()
			.map(|target| (DialSource::Static, target))
			.or_else(|| self.api.as_ref().map(|target| (DialSource::Api, target)))
			.or_else(|| self.gossip.as_ref().map(|target| (DialSource::Gossip, target)));
		#[cfg(feature = "cluster-lan")]
		let fallback = fallback.or_else(|| self.mdns.as_ref().map(|target| (DialSource::Mdns, target)));
		fallback
	}
}

/// One entry in [`DialMap`]. `unannounced_at` carries the gossip stale timer; the
/// sweep uses it to decide when the gossip source has truly gone vs. is just
/// flapping between paths. It's only meaningful while `sources.gossip` is set.
struct DialEntry {
	handle: AbortHandle,
	sources: DialSources,
	active: DialSource,
	unannounced_at: Option<Instant>,
}

/// Map of in-flight cluster dials, keyed by canonical peer identity. Cloneable: the inner
/// map is shared via `Arc<Mutex<_>>` so the discovery task and the static-seed
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
	fn upsert<F>(&self, target: DialTarget, source: DialSource, spawn: &mut F) -> bool
	where
		F: FnMut(DialTarget) -> AbortHandle,
	{
		let mut map = self.inner.lock().expect("dial map poisoned");
		if let Some(entry) = map.get_mut(&target.key) {
			let replace = entry.active == source && entry.sources.get(source) != Some(&target);
			entry.sources.set(source, target.clone());
			let reannounced = source == DialSource::Gossip && entry.unannounced_at.take().is_some();
			if replace {
				entry.handle.abort();
				entry.handle = spawn(target);
			}
			return reannounced;
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
				unannounced_at: None,
			},
		);
		false
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

	/// Start the gossip stale timer on `peer` if it isn't already pending. No-op
	/// unless the peer is currently wanted by gossip. Idempotent: a repeat
	/// unannounce while a timestamp is pending doesn't reset the clock.
	fn mark_unannounced(&self, peer: &str, now: Instant) {
		let mut map = self.inner.lock().expect("dial map poisoned");
		if let Some(entry) = map.get_mut(peer)
			&& entry.sources.gossip.is_some()
		{
			entry.unannounced_at.get_or_insert(now);
		}
	}

	/// Release the gossip source from entries whose unannounce has stuck for at
	/// least `threshold`, aborting the dial only if no other source still wants it.
	fn sweep_stale<F>(&self, now: Instant, threshold: Duration, spawn: &mut F)
	where
		F: FnMut(DialTarget) -> AbortHandle,
	{
		let mut map = self.inner.lock().expect("dial map poisoned");
		let expired: Vec<String> = map
			.iter_mut()
			.filter_map(|(peer, entry)| {
				let at = entry.unannounced_at?;
				(now.duration_since(at) >= threshold).then(|| {
					entry.unannounced_at = None;
					peer.clone()
				})
			})
			.collect();
		drop(map);

		for peer in expired {
			self.release(&peer, DialSource::Gossip, spawn);
		}
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
/// [`Self::connect`] / [`Self::connect_api`] list peers to dial. [`Self::node`] is
/// this relay's own URL (identity); [`Self::mesh`] enables gossip, advertising that
/// URL so other peers discover and dial it. Set `node` + `mesh` with no `connect`
/// to act as a passive rendezvous.
///
/// Hop-based routing on broadcasts prevents announcement loops regardless of topology.
#[serde_with::serde_as]
#[derive(clap::Args, Clone, Debug, serde::Serialize, serde::Deserialize, Default)]
#[serde_with::skip_serializing_none]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
#[group(id = "cluster-config")]
pub struct ClusterConfig {
	/// Fixed origin (hop) id for this relay, identifying it in the hop chains
	/// carried on each broadcast for loop detection and shortest-path routing.
	///
	/// Unset (the default) picks a fresh random id on every start. Set it to give
	/// a node a stable identity across restarts. Must be non-zero and below 2^62
	/// (the wire varint limit); an out-of-range value errors at startup. Keep it
	/// below 2^53 for compatibility with older `@moq/lite` JS clients, which
	/// decode hop ids as a `u53` and reject anything larger.
	#[arg(id = "cluster-id", long = "cluster-id", env = "MOQ_CLUSTER_ID")]
	pub id: Option<u64>,

	/// Connect to one or more other cluster nodes. Each peer is a full URL, e.g.
	/// `https://host/?jwt=TOKEN`; a bare host or `host:port` is deprecated but
	/// still accepted (wrapped in `https://.../`). Accepts a comma-separated list
	/// on the CLI or repeat the flag; in config files use a TOML array.
	///
	/// A `?cost=N` query param prices the link (moq-lite-06+): every
	/// announcement crossing it adds `N` to its route cost, so routing prefers
	/// cheap paths over short ones. Use `0` for a same-datacenter sibling and
	/// something large for a metered backbone; an unpriced link costs 1, which
	/// reproduces plain hop counting. The param is consumed by this relay, not
	/// sent to the peer.
	#[serde(alias = "connect")]
	#[arg(
		id = "cluster-connect",
		long = "cluster-connect",
		env = "MOQ_CLUSTER_CONNECT",
		value_delimiter = ','
	)]
	#[serde_as(as = "serde_with::OneOrMany<_>")]
	pub connect: Vec<String>,

	/// Fetch the list of peers to dial from an HTTP(S) URL or a local file,
	/// reloading at runtime without a restart. The source returns a JSON array
	/// of peer URLs: `["https://a.pop.example/?cost=1", "b.pop.example"]`. An http(s) URL is
	/// re-checked on a fixed cadence, with caching, conditional revalidation
	/// (`ETag` / `Last-Modified`), and stale-if-error handled by the shared HTTP
	/// cache client, so the response's `Cache-Control` controls how often a real
	/// fetch hits the endpoint; a local path is watched via OS filesystem
	/// notifications (with a periodic re-check fallback). This relay's own
	/// [`Self::node`] value, when set, is sent as a `?node=` query param so the
	/// server can return this node's peers. The relay keeps the last good list if
	/// a fetch fails. Composes with [`Self::connect`] and [`Self::mesh`].
	#[arg(
		id = "cluster-connect-api",
		long = "cluster-connect-api",
		env = "MOQ_CLUSTER_CONNECT_API"
	)]
	pub connect_api: Option<String>,

	/// This relay's own externally-reachable URL (identity). Sent to
	/// [`Self::connect_api`] as a `?node=` query param so the endpoint can return
	/// this node's peers, and advertised to other relays when [`Self::mesh`] gossip
	/// is enabled. On its own it neither opens nor accepts a connection.
	#[arg(id = "cluster-node", long = "cluster-node", env = "MOQ_CLUSTER_NODE")]
	pub node: Option<String>,

	/// Enable gossip discovery: advertise this relay's [`Self::node`] URL on the
	/// cluster origin so peers can find and dial it (and so this relay discovers
	/// peers the same way). Requires [`Self::node`]. Boolean flag: pass
	/// `--cluster-mesh` (or `=true` / `=false`).
	///
	/// Kept as a string for backwards compatibility: `--cluster-mesh` used to take
	/// this relay's URL. A non-boolean value is treated as a legacy [`Self::node`]
	/// (with a deprecation warning), or an error if it conflicts with an explicit
	/// `--cluster-node`. Accepts a TOML boolean or string.
	#[arg(
		id = "cluster-mesh",
		long = "cluster-mesh",
		env = "MOQ_CLUSTER_MESH",
		default_missing_value = "true",
		num_args = 0..=1,
		require_equals = true,
	)]
	#[serde(default, deserialize_with = "deserialize_bool_or_string")]
	pub mesh: Option<String>,

	/// LAN discovery over mDNS (`[cluster.lan]`).
	#[cfg(feature = "cluster-lan")]
	#[command(flatten)]
	#[serde(default)]
	pub lan: LanConfig,

	/// JWT presented on outbound cluster dials, read from this file. Applied to
	/// any peer whose URL doesn't already carry a `?jwt=` (so it authenticates
	/// any peer whose URL has no inline token). An inline `?jwt=` can provide a
	/// per-peer credential for static or `connect_api` peers. Gossip should use
	/// this shared token or mTLS because the advertised node URL is public.
	#[arg(id = "cluster-token", long = "cluster-token", env = "MOQ_CLUSTER_TOKEN")]
	pub token: Option<PathBuf>,

	/// Billing tier label that cluster-peer (relay-to-relay) traffic records
	/// stats under. Defaults to the unprefixed tier.
	#[arg(id = "cluster-tier", long = "cluster-tier", env = "MOQ_CLUSTER_TIER")]
	pub tier: Option<String>,
	// Accepted so existing configs keep parsing (`deny_unknown_fields`), but
	// ignored: a broadcast now closes as soon as its last publisher is lost.
	#[doc(hidden)]
	#[deprecated(note = "ignored; a broadcast closes as soon as its last publisher is lost")]
	#[arg(
		id = "cluster-linger",
		long = "cluster-linger",
		env = "MOQ_CLUSTER_LINGER",
		value_parser = humantime::parse_duration,
		hide = true,
	)]
	#[serde(default, with = "humantime_serde")]
	pub linger: Option<Duration>,
}

/// LAN discovery configuration (`[cluster.lan]`).
///
/// Advertises [`ClusterConfig::node`] over mDNS and dials the node URLs other
/// relays advertise, so a rack or a home lab meshes with no seed list and no
/// shared rendezvous. mDNS only replaces *how* peers are found: they are dialed
/// and authenticated exactly like any other cluster peer.
#[derive(clap::Args, Clone, Debug, serde::Serialize, serde::Deserialize, Default)]
#[serde_with::skip_serializing_none]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
#[group(id = "cluster-lan-config")]
#[cfg(feature = "cluster-lan")]
pub struct LanConfig {
	/// Enable mDNS discovery. Requires [`ClusterConfig::node`], so there is an
	/// address to advertise, and [`Self::secret`]. Boolean flag: pass
	/// `--cluster-lan` (or `=true` / `=false`).
	#[arg(
		id = "cluster-lan",
		long = "cluster-lan",
		env = "MOQ_CLUSTER_LAN",
		default_missing_value = "true",
		num_args = 0..=1,
		require_equals = true,
	)]
	pub enabled: Option<bool>,

	/// The shared key admitting a peer to the LAN mesh, as 64 hexadecimal
	/// characters or a path to a file containing them. Required by
	/// [`Self::enabled`].
	///
	/// mDNS is an open channel: anyone can advertise, including an attacker
	/// naming a URL it controls. Without a proof that the advertiser holds this
	/// key, this relay would dial that URL and hand it
	/// [`ClusterConfig::token`]. So the key is mandatory rather than optional,
	/// and only peers that prove they hold it are ever dialed.
	#[arg(
		id = "cluster-lan-secret",
		long = "cluster-lan-secret",
		env = "MOQ_CLUSTER_LAN_SECRET",
		value_name = "HEX_OR_PATH"
	)]
	pub secret: Option<String>,
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
	gossip: bool,
	node: Option<String>,
	can_dial: bool,
	token: String,
	/// The live mDNS advertisement, bound by [`Cluster::start`].
	#[cfg(feature = "cluster-lan")]
	discovery: Option<moq_tokio::mdns::Discovery>,
}

/// A relay cluster built around a single [`origin::Producer`].
///
/// Local sessions and remote cluster connections all publish into the same
/// origin. Loop prevention and route preference come from the hop list carried
/// on each broadcast's route (see [`moq_net::broadcast::Route`]).
///
/// Construct with [`Cluster::new`], then attach a QUIC client and (optionally)
/// a [`stats::Registry`](moq_net::stats::Registry) with the `with_*` builder
/// methods. A cluster without a client can serve local sessions but cannot
/// dial remote peers.
#[derive(Clone)]
pub struct Cluster {
	config: ClusterConfig,
	client: Option<moq_tokio::Client>,
	pub(crate) nodes: crate::nodes::Nodes,

	/// Hands out the `conn` id every session logs under, inbound and outbound
	/// alike, so one id space covers the whole process and an id in the `/nodes`
	/// view always points at the same session in the logs.
	connection_ids: Arc<AtomicU64>,

	/// The origin's construction config (identity, cache pool). Kept so the
	/// `with_*` builders can rebuild the origin without losing each other's
	/// settings.
	info: origin::Info,

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

impl Cluster {
	/// Creates a new cluster with a fresh origin and no peers, client, or stats.
	///
	/// Use [`with_client`](Self::with_client) to enable dialing remote peers
	/// (required when `config.connect` is non-empty), and
	/// [`with_stats`](Self::with_stats) to enable metrics publishing.
	///
	/// Must be called within a tokio runtime: the origin's lifecycle driver is
	/// spawned here, so the origin serves sessions whether or not
	/// [`start`](Self::start) (the mesh half) is ever called.
	///
	/// Errors if `config.id` is set but invalid: it must be non-zero and below
	/// 2^62 (the wire varint limit). An unset id picks a fresh random origin.
	pub fn new(config: ClusterConfig) -> anyhow::Result<Self> {
		let id = match config.id {
			Some(0) => anyhow::bail!("--cluster-id must be non-zero"),
			Some(id) if id >= 1 << 62 => {
				anyhow::bail!("--cluster-id must be below 2^62 (wire varint limit), got {id}")
			}
			Some(id) => Origin::new(id).expect("cluster id already validated"),
			None => Origin::random(),
		};
		#[allow(deprecated)]
		if config.linger.is_some() {
			tracing::warn!(
				"cluster linger is deprecated and ignored; a broadcast closes as soon as its last publisher is lost"
			);
		}
		let info = origin::Info::new(id);
		let origin = moq_tokio::origin::spawn(info.clone());
		let nodes = crate::nodes::Nodes::new(origin.clone());
		tracing::info!(origin_id = %origin.id(), configured = config.id.is_some(), "cluster initialized");
		Ok(Cluster {
			config,
			client: None,
			nodes,
			connection_ids: Arc::default(),
			client_tls: None,
			info,
			origin,
			stats: moq_net::stats::Registry::disabled(),
			_stats_publisher: None,
		})
	}

	/// Attach the resolved [`Cache`](crate::Cache) (the shared group pool and the
	/// per-track retention ceiling) so every session's broadcasts cache into one
	/// memory budget bounded by both bytes and age. Call before deriving any origin
	/// handles (e.g. [`with_stats`](Self::with_stats)) so they inherit the settings.
	///
	/// Rebuilds the origin with the cache: safe because the cluster's origin is
	/// still pristine here (no broadcasts published, no scopes derived).
	pub fn with_cache(mut self, cache: crate::Cache) -> Self {
		self.info = self
			.info
			.clone()
			.with_pool(cache.pool)
			.with_cache_duration(cache.duration);
		self.origin = moq_tokio::origin::spawn(self.info.clone());
		self.nodes = self.nodes.with_origin(self.origin.clone());
		self
	}

	/// Attach a QUIC client used to dial cluster peers.
	///
	/// Required when `config.connect` is non-empty; [`start`](Self::start) returns
	/// an error otherwise.
	pub fn with_client(mut self, client: moq_tokio::Client) -> Self {
		self.client = Some(client);
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
	/// Build one with [`StatsConfig::build`](crate::StatsConfig::build), passing
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
	pub fn subscriber(&self, token: &AuthToken) -> Option<origin::Producer> {
		self.origin.with_root(&token.root)?.scope(&token.subscribe)
	}

	/// Returns an [`origin::Producer`] scoped to this session's publish permissions.
	pub fn publisher(&self, token: &AuthToken) -> Option<origin::Producer> {
		self.origin.with_root(&token.root)?.scope(&token.publish)
	}

	/// Resolve whether gossip is on and which URL this relay advertises, from
	/// `cluster.node` and the (string-typed) `cluster.mesh` toggle.
	///
	/// `mesh` is `"true"` / `"false"` normally. For backwards compatibility a
	/// non-boolean value is the legacy "advertise this URL" form: it turns gossip
	/// on and supplies the node URL (with a deprecation warning), unless it
	/// conflicts with an explicit `cluster.node`, which is an error.
	fn resolve_mesh(&self) -> anyhow::Result<(bool, Option<String>)> {
		let node = self.config.node.clone();
		match self.config.mesh.as_deref() {
			None => Ok((false, node)),
			Some("true") => Ok((true, node)),
			Some("false") => Ok((false, node)),
			Some(legacy) => {
				tracing::warn!(
					value = %legacy,
					"`--cluster-mesh` is now a boolean; treating the value as `--cluster-node` for backwards \
					 compatibility. Set `--cluster-node <url>` and `--cluster-mesh` instead."
				);
				match &node {
					Some(node) if node != legacy => anyhow::bail!(
						"`--cluster-mesh` was given URL {legacy:?}, which conflicts with `--cluster-node` {node:?}. \
						 `--cluster-mesh` is now a boolean; set the address only via `--cluster-node`."
					),
					_ => Ok((true, Some(legacy.to_owned()))),
				}
			}
		}
	}

	/// Whether `--cluster-lan` asked this relay to discover peers over mDNS.
	fn lan(&self) -> bool {
		#[cfg(feature = "cluster-lan")]
		return self.config.lan.enabled.unwrap_or(false);
		#[cfg(not(feature = "cluster-lan"))]
		false
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
	/// Bails when `mesh` gossip is on without `node`, or when peers are configured
	/// to dial but no client was attached via [`with_client`](Self::with_client).
	pub async fn start(self) -> anyhow::Result<Started> {
		let (gossip, node) = self.resolve_mesh()?;
		anyhow::ensure!(
			!gossip || node.is_some(),
			"`--cluster-mesh` (gossip) requires `--cluster-node <self-url>` so there's an address to advertise. \
			 See https://doc.moq.dev/bin/relay/cluster."
		);

		let lan = self.lan();
		anyhow::ensure!(
			!lan || node.is_some(),
			"`--cluster-lan` requires `--cluster-node <self-url>` so there's an address to advertise. \
			 See https://doc.moq.dev/bin/relay/cluster."
		);
		// Without this, an attacker advertising a URL it controls would be dialed
		// like any other peer and handed `--cluster-token`.
		#[cfg(feature = "cluster-lan")]
		anyhow::ensure!(
			!lan || self.config.lan.secret.is_some(),
			"`--cluster-lan` requires `--cluster-lan-secret <hex-or-path>`, shared by every peer. \
			 mDNS is unauthenticated, so without it any advertiser on the network would be dialed. \
			 See https://doc.moq.dev/bin/relay/cluster."
		);

		let has_outbound = !self.config.connect.is_empty() || self.config.connect_api.is_some();
		// Every mechanism that opens a dial, so gossip discovery runs whenever
		// there is something to dial with, not only when a peer was listed.
		let can_dial = has_outbound || lan;
		let has_work = can_dial || gossip;
		if !has_work {
			tracing::info!("no cluster peers configured; running standalone");
			return Ok(Started {
				work: None,
				cluster: self,
			});
		}

		if can_dial {
			anyhow::ensure!(
				self.client.is_some(),
				"cluster peers configured but no QUIC client attached (call Cluster::with_client)"
			);
		}

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
			// Checked above: the LAN mesh advertises `node` and requires a key.
			true => {
				let node = node.as_deref().expect("--cluster-lan requires --cluster-node");
				let secret = self
					.config
					.lan
					.secret
					.as_deref()
					.expect("--cluster-lan requires --cluster-lan-secret");
				Some(lan_discovery(node, secret).await?)
			}
			false => None,
		};

		Ok(Started {
			work: Some(Work {
				gossip,
				node,
				can_dial,
				token,
				#[cfg(feature = "cluster-lan")]
				discovery,
			}),
			cluster: self,
		})
	}

	/// Runs the cluster event loop against the work [`start`](Self::start)
	/// resolved.
	///
	/// Modes are derived from config: passive rendezvous (`node` + `mesh` gossip,
	/// no peers to dial) parks after publishing self-registration and does not
	/// require a QUIC client; active (`connect` / `connect_api` set) dials peers
	/// and, when `mesh` gossip is on, also advertises `node` and runs discovery.
	async fn run_work(self, work: Work) -> anyhow::Result<()> {
		let Work {
			gossip,
			node,
			can_dial,
			token,
			#[cfg(feature = "cluster-lan")]
			discovery,
		} = work;

		// Static `--cluster-connect` peers and gossip-discovered peers share one
		// dial map so a peer reached via both paths only opens a single dial.
		// Gossip-driven unannounces don't abort immediately. The discovery loop
		// runs a periodic sweep that only aborts entries whose unannounce has
		// stuck for [`STALE_AFTER`]. That filters out the prefer-shorter-hop flap
		// (sub-millisecond unannounce-then-announce) while still cleaning up
		// peers that truly left.
		let dialed = DialMap::default();
		let mut tasks = tokio::task::JoinSet::new();
		// Tasks whose ending is a failure rather than an ordinary lifecycle event,
		// so their result has to be looked at instead of drained.
		#[allow(unused_mut, reason = "only the cluster-lan feature spawns into it")]
		let mut supervised: tokio::task::JoinSet<anyhow::Result<()>> = tokio::task::JoinSet::new();

		for peer in &self.config.connect {
			let target = DialTarget::parse(peer).context("invalid --cluster-connect peer URL")?;
			if dialed.contains(&target.key) {
				continue;
			}
			if is_legacy_peer(peer) {
				tracing::warn!(
					%peer,
					"DEPRECATED: pass --cluster-connect as a full URL like \"https://<host>/?jwt=TOKEN\"; \
					 a bare host or \"host:port\" is deprecated and will be removed in a future release"
				);
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
			let token = token.clone();
			let dialed = dialed.clone();
			// The identity discovery actually advertises, not the configured URL it
			// came from. `with_node` strips userinfo before publishing, so comparing
			// the raw `--cluster-node` here would tiebreak against a spelling no peer
			// ever sees: both sides could decide the other should dial, and neither
			// would.
			let self_url = canonicalize_peer_key(discovery.id());
			// Supervised rather than fire-and-forget: a dial task ending is ordinary
			// (that peer went away), but discovery ending is the relay going blind.
			supervised.spawn(async move { this.run_mdns(self_url, token, dialed, discovery).await });
		}

		// Held in scope so the registration stays announced until `run` exits.
		// Discovery is paired with it: a gossip-only relay (passive rendezvous) has
		// nothing to discover, so we only run it when we also have an outbound peer.
		let self_registration: Option<moq_net::broadcast::Producer> = if gossip {
			// Checked above: gossip requires `node`.
			let node = node.as_deref().expect("gossip requires --cluster-node");
			let path = Path::new(MESH_PREFIX).join(node);
			let broadcast = self
				.origin
				.create_broadcast(&path, moq_net::broadcast::Route::new().with_announce(true))
				.expect(".internal/origins is within the relay origin's root");
			tracing::info!(%node, %path, "advertising cluster node URL");

			if can_dial {
				let this = self.clone();
				let token = token.clone();
				let dialed = dialed.clone();
				// Canonical, so the tiebreaker compares the same spelling both sides do.
				let self_url = canonicalize_peer_key(node);
				tasks.spawn(async move {
					this.run_discovery(self_url, token, dialed).await;
				});
			}

			Some(broadcast)
		} else {
			None
		};

		if tasks.is_empty() && supervised.is_empty() {
			// Passive rendezvous: park to keep `self_registration` alive. The
			// process still exits via the other arms of `tokio::select!` in main.
			std::future::pending::<()>().await
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

		// Deliberate shutdown: finish the registration rather than dropping it, so
		// there is no dropped-without-finish warning.
		if let Some(mut registration) = self_registration {
			registration.finish();
		}
		Ok(())
	}

	/// Watch `.internal/origins/*` for peer registrations and dial each newly-
	/// announced URL that sorts after our own (see [`should_dial`]); the peer on
	/// the other side of that comparison dials us, so each pair opens one session
	/// instead of two. Unannounces don't abort immediately. They just mark the
	/// entry as "pending cleanup" with a timestamp. A periodic sweep evicts
	/// entries whose unannounce has stuck for [`STALE_AFTER`]. The "prefer
	/// shorter hop" path in origin::Producer delivers re-announces as
	/// unannounce-then-announce within sub-milliseconds, which clears the
	/// pending-cleanup timestamp long before the sweep fires.
	async fn run_discovery(self, self_url: String, token: String, dialed: DialMap) {
		let Some(consumer) = self.origin.consume().with_root(MESH_PREFIX) else {
			tracing::warn!("could not scope cluster origin to {MESH_PREFIX}; discovery disabled");
			return;
		};
		let mut announced = consumer.announced();
		let mut live = GossipTargets::default();

		let mut sweep = tokio::time::interval(SWEEP_INTERVAL);
		sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
		// Skip the first immediate tick; nothing has had a chance to go stale yet.
		sweep.tick().await;

		loop {
			tokio::select! {
				ann = announced.next() => {
					let Some(moq_net::announce::Update { path: relative, broadcast }) = ann else { return; };
					// The address to dial, which keeps its query: `run_remote` reads
					// `?cost=` and `?jwt=` off it. The key is only its identity.
					let peer = advertised_node_url(relative.as_str());
					let target = match DialTarget::parse(&peer) {
						Ok(target) => target,
						Err(err) => {
							tracing::warn!(%err, "invalid gossiped cluster peer URL; ignoring update");
							continue;
						}
					};
					// Skip self and any peer we lose the tiebreaker to; that side
					// dials us instead, so each pair forms a single session.
					if !should_dial(&self_url, &target.key) {
						continue;
					}
					let advertisement = relative.as_str().to_owned();
					match broadcast {
						Some(_) => {
							let target = live.announce(advertisement, target);
							let mut spawn = |target: DialTarget| {
								tracing::info!(peer = %target.key, "discovered cluster peer; dialing");
								tokio::spawn(self.clone().supervise_remote(target, token.clone())).abort_handle()
							};
							let key = target.key.clone();
							if dialed.upsert(target, DialSource::Gossip, &mut spawn) {
								tracing::debug!(peer = %key, "reannounce within sweep window; keeping dial");
							}
						}
						None => match live.unannounce(&advertisement, &target.key) {
							GossipUpdate::Current(target) => {
								let mut spawn = |target: DialTarget| {
									tracing::info!(peer = %target.key, "cluster peer advertisement changed; redialing");
									tokio::spawn(self.clone().supervise_remote(target, token.clone())).abort_handle()
								};
								dialed.upsert(target, DialSource::Gossip, &mut spawn);
							}
							GossipUpdate::Unchanged => {}
							GossipUpdate::Gone => dialed.mark_unannounced(&target.key, Instant::now()),
						},
					}
				}
				_ = sweep.tick() => {
					let mut spawn = |target: DialTarget| {
						tracing::info!(peer = %target.key, "cluster peer source changed; redialing");
						tokio::spawn(self.clone().supervise_remote(target, token.clone())).abort_handle()
					};
					dialed.sweep_stale(Instant::now(), STALE_AFTER, &mut spawn);
				}
			}
		}
	}

	/// Dial every relay that advertises itself on the LAN, the same way gossip
	/// dials every relay that advertises itself on the cluster origin.
	///
	/// mDNS only supplies the peer URL; everything downstream (the tiebreaker, the
	/// shared dial map, `?jwt=`, `?cost=`, stats, the node view) is the ordinary
	/// cluster path. A peer that advertises no node URL is skipped: this relay
	/// dials by name, not by discovered socket.
	///
	/// Discovery only reports a peer whose advertisement proved possession of the
	/// shared key, which is what makes the URL safe to dial with `--cluster-token`
	/// attached. An unauthenticated record never reaches this loop.
	#[cfg(feature = "cluster-lan")]
	async fn run_mdns(
		self,
		self_url: String,
		token: String,
		dialed: DialMap,
		mut discovery: moq_tokio::mdns::Discovery,
	) -> anyhow::Result<()> {
		use moq_tokio::mdns::Event;

		// Logged by key, never by the target's URL, so an inline query stays out of
		// the logs.
		let mut spawn = |target: DialTarget| {
			tracing::info!(peer = %target.key, "dialing LAN cluster peer");
			tokio::spawn(self.clone().supervise_remote(target, token.clone())).abort_handle()
		};

		while let Some(event) = discovery.recv().await {
			match event {
				Event::Found(peer) => {
					let Some(node) = peer.node.as_ref() else {
						tracing::debug!(peer = %peer.id, "LAN peer advertised no --cluster-node URL; skipping");
						continue;
					};
					// The address to dial keeps its query, since `run_remote` reads
					// `?cost=` off it. The key is only its identity, exactly as on the
					// gossip path.
					let target = match DialTarget::parse(node.as_ref()) {
						Ok(target) => target,
						Err(err) => {
							tracing::warn!(%err, peer = %peer.id, "LAN peer advertised an invalid --cluster-node URL; skipping");
							continue;
						}
					};
					// Skip any peer we lose the tiebreaker to; that side dials us.
					if !should_dial(&self_url, &target.key) {
						continue;
					}
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
	/// array of peer URLs that's reconciled into the shared dial map.
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
	/// [`moq_tokio::watch::FileWatcher`] (OS notifications with a polling fallback).
	/// Fails static: a missing or malformed file keeps the current dials, and the
	/// next change triggers a fresh attempt.
	async fn run_connect_api_file(&self, path: PathBuf, node: Option<String>, token: String, dialed: DialMap) {
		self.reload_connect_api_file(&path, &node, &token, &dialed);

		let mut watcher = match moq_tokio::watch::FileWatcher::new(std::slice::from_ref(&path)) {
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
	/// current dials; the [`FileWatcher`](moq_tokio::watch::FileWatcher) only
	/// re-invokes this on a real change, so a malformed file isn't re-warned on a
	/// loop.
	fn reload_connect_api_file(&self, path: &std::path::Path, node: &Option<String>, token: &str, dialed: &DialMap) {
		match std::fs::read_to_string(path) {
			Ok(body) => match serde_json::from_str::<Vec<String>>(&body) {
				Ok(list) => self.apply_peer_list(list, node, token, dialed),
				Err(err) => {
					tracing::warn!(%err, ?path, "cluster.connect_api file is not a JSON array; keeping current peers")
				}
			},
			Err(err) => tracing::warn!(%err, ?path, "failed to read cluster.connect_api file; keeping current peers"),
		}
	}

	/// Fetch and parse the peer list. Caching, conditional revalidation, and
	/// stale-if-error are handled by the HTTP cache middleware on `http`, so this
	/// just issues the request and parses the (possibly cache-served) body.
	async fn fetch_peer_list(http: &ClientWithMiddleware, url: Url) -> anyhow::Result<Vec<String>> {
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

		serde_json::from_str(&body).context("cluster.connect_api response is not a JSON array of peer URLs")
	}

	/// Reconcile a freshly fetched peer list into the dial map: dial peers that
	/// are new and drop API peers that disappeared. The relay's own [`node`] URL
	/// is filtered out so it never dials itself.
	fn apply_peer_list(&self, list: Vec<String>, node: &Option<String>, token: &str, dialed: &DialMap) {
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
		let mut url = target.url.clone();
		let cost = target.cost;
		// Apply the shared cluster token unless the URL already carries its own
		// non-empty `?jwt=` (a per-peer inline token wins; the shared token still
		// covers peers that have none). An empty
		// `?jwt=` counts as absent, matching `AuthParams::from_url`.
		if !token.is_empty() && !url.query_pairs().any(|(key, value)| key == "jwt" && !value.is_empty()) {
			url.query_pairs_mut().append_pair("jwt", &token);
		}

		let base_backoff = tokio::time::Duration::from_secs(1);
		let max_backoff = tokio::time::Duration::from_secs(300);
		// Sessions shorter than this are treated as churn: we keep backing off
		// instead of resetting, otherwise a peer that rejects us instantly would
		// turn into a tight reconnect loop.
		let stable_threshold = tokio::time::Duration::from_secs(10);

		let mut backoff = base_backoff;

		loop {
			let started = tokio::time::Instant::now();
			let result = self.run_remote_once(&url, cost).await;
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

	async fn run_remote_once(&self, url: &Url, cost: Option<u64>) -> anyhow::Result<()> {
		// Each attempt is its own session, so it gets its own id. Matches the span an
		// accepted connection runs under, so both directions log the same way.
		let id = self.next_connection_id();
		self.run_remote_session(id, url, cost)
			.instrument(tracing::info_span!("conn", id))
			.await
	}

	async fn run_remote_session(&self, id: u64, url: &Url, cost: Option<u64>) -> anyhow::Result<()> {
		let mut log_url = url.clone();
		log_url.set_query(None);
		tracing::info!(url = %log_url, "dialing cluster peer");

		// Checked at the start of `run`; per-peer tasks inherit that guarantee.
		let client = self
			.client
			.clone()
			.context("internal: cluster peer dial without an attached QUIC client")?;

		// Cluster dials use their configured stats tier. Cluster peers carry no auth
		// root, so presence is keyed under the empty root within the cluster tier.
		let mut client = client
			.with_publisher(&self.origin)
			.with_subscriber(self.origin.clone())
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
		let mut reconnect = client.with_reconnect(true).connect(url.clone());
		let mut connection = None;
		loop {
			match reconnect.status().await? {
				moq_tokio::Status::Connected if connection.is_none() => {
					connection = Some(self.nodes.connect_outbound(id, log_url.to_string()));
				}
				moq_tokio::Status::Disconnected => connection = None,
				moq_tokio::Status::Migrating => {}
				_ => {}
			}
		}
	}
}

/// Advertise this relay's node URL on the LAN, gated on the shared key.
///
/// The DNS-SD record needs a port, so it carries the node URL's; peers dial the
/// URL itself, which is what keeps the relay's normal name-and-certificate path
/// intact. The key is what makes the advertised URL trustworthy enough to dial.
#[cfg(feature = "cluster-lan")]
async fn lan_discovery(node: &str, secret: &str) -> anyhow::Result<moq_tokio::mdns::Discovery> {
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
	let port = url.port_or_known_default().unwrap_or(443);
	let secret = moq_tokio::mdns::Secret::load(secret).context("invalid --cluster-lan-secret")?;
	Ok(moq_tokio::mdns::Config::new(port)
		.with_node(url)
		.with_secret(secret)
		.advertise()
		.await?)
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

/// Whether a `--cluster-connect-api` source is an http(s) URL (otherwise it's
/// treated as a local file path, which needs no TLS client).
fn connect_api_is_http(source: &str) -> bool {
	Url::parse(source).is_ok_and(|url| matches!(url.scheme(), "http" | "https"))
}

/// Resolve a cluster peer to the URL we dial.
///
/// The modern form is a full URL, e.g. `https://host/?jwt=TOKEN`, which is used
/// verbatim. A bare host or `host:port` is still accepted for backwards
/// compatibility and wrapped in `https://.../` (callers warn about this legacy
/// form for user-supplied `--cluster-connect` entries).
fn peer_url(peer: &str) -> anyhow::Result<Url> {
	// A full URL has a scheme separator; a bare host or `host:port` does not
	// (and `Url::parse` would otherwise mis-read `host:port` as scheme `host`).
	if peer.contains("://") {
		return Url::parse(peer).context("invalid cluster peer URL");
	}

	Url::parse(&format!("https://{peer}/")).context("invalid cluster peer host")
}

/// The address to dial for a node advertised under `MESH_PREFIX`.
///
/// A relay advertises its URL as a broadcast path, and [`Path`] collapses slash
/// runs, so `https://relay-b.example/` arrives as `https:/relay-b.example`. The
/// scheme is put back here rather than read as a hostname.
///
/// The trailing colon on the leading segment is what marks a scheme: a bare
/// `host:port` carries its colon in the middle of the segment, never at the end,
/// so the two spellings can't be mistaken for each other and no list of known
/// schemes is needed. A scheme-less advertisement means `https`, exactly as
/// [`peer_url`] reads a bare host.
///
/// The query survives, because [`Cluster::run_remote`] reads `?cost=` and `?jwt=`
/// off the address it dials. Pass the result through [`canonicalize_peer_key`]
/// for an identity key; don't dial the key, which drops the query.
///
/// A trailing slash on a non-empty URL path does not survive, since a path has no
/// way to spell one. `https://a.example/edge/` advertises the same as
/// `https://a.example/edge`.
pub(crate) fn advertised_node_url(advertised: &str) -> String {
	match advertised.split_once('/') {
		// The segment already ends in `:`, so this restores the `//` the path ate.
		Some((scheme, rest)) if scheme.ends_with(':') => format!("{scheme}//{rest}"),
		_ => advertised.to_owned(),
	}
}

/// Whether a peer string uses the deprecated bare-host / `host:port` form rather
/// than a full URL. Used to warn on legacy `--cluster-connect` entries.
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
			// Gossip round-trips the URL through `Path`, which trims slashes and
			// collapses runs of them, so the key normalizes the path the same way.
			// A non-special scheme like `moqt` keeps its trailing slash otherwise,
			// and `moqt://a.example:4443/` is an ordinary way to spell a node.
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
/// `Option<String>` (booleans become `"true"` / `"false"`). Lets `cluster.mesh`
/// take the modern `mesh = true` form or the legacy `mesh = "<url>"` form.
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
	use crate::Config;

	/// The publish task holds only a `Weak` to its producer, so it stops when the
	/// last `moq_stats::Producer` clone drops. Attaching one must therefore hand
	/// its lifetime to the cluster: an embedder driving its own loop takes the
	/// pieces it needs off a `Relay` and drops the rest, and a relay that keeps
	/// serving while silently publishing nothing is exactly the class of failure
	/// this API exists to rule out. Moving the ONLY handle in must keep it alive.
	///
	/// Asserts the broadcast is still live AFTER a few publish intervals, not
	/// merely that one appeared: the task publishes once before it can notice its
	/// producer is gone, so a first announcement races through either way and
	/// proves nothing.
	#[tokio::test]
	async fn stats_publishing_outlives_the_producer_handle() {
		let config = crate::StatsConfig {
			enabled: Some(true),
			node: Some("test".to_string()),
			..Default::default()
		};

		let cluster = Cluster::new(ClusterConfig::default()).expect("cluster");
		let stats = config.build(cluster.origin.clone());
		let cluster = cluster.with_stats(stats);

		let path = moq_net::Path::new(".stats").join("node").join("test");
		let broadcast = tokio::time::timeout(
			std::time::Duration::from_secs(5),
			cluster.origin.consume().announced_broadcast(&path),
		)
		.await
		.expect("stats broadcast announced within 5s")
		.expect("stats broadcast present");

		// Several publish intervals (1s each by default) after the handle went out
		// of scope, the task is still running and its broadcast still open. The
		// re-check is bounded: without the keepalive the broadcast is gone and
		// `announced_broadcast` waits forever, which must read as a failure rather
		// than a hung test.
		tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
		let still_live = tokio::time::timeout(
			std::time::Duration::from_secs(2),
			cluster.origin.consume().announced_broadcast(&path),
		)
		.await;
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
		let cluster = Cluster::new(ClusterConfig::default()).expect("cluster");
		assert_eq!(cluster.cluster_tier(), Tier::default());

		let cluster = Cluster::new(ClusterConfig {
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
			cost: None,
		}
	}

	fn desired(keys: &[&str]) -> HashMap<String, DialTarget> {
		keys.iter().map(|key| ((*key).to_string(), target(key))).collect()
	}

	/// A query change creates a new gossip path before the old path necessarily
	/// unannounces. Removing that old path must not stale the replacement.
	#[test]
	fn gossip_old_unannounce_keeps_new_target() {
		let mut live = GossipTargets::default();
		let old = DialTarget::parse("https://peer.example/?cost=1").unwrap();
		let new = DialTarget::parse("https://peer.example/?cost=2").unwrap();
		live.announce("peer.example/?cost=1".to_string(), old.clone());
		live.announce("peer.example/?cost=2".to_string(), new.clone());

		assert!(matches!(
			live.unannounce("peer.example/?cost=1", &old.key),
			GossipUpdate::Unchanged
		));
		assert!(matches!(
			live.unannounce("peer.example/?cost=2", &new.key),
			GossipUpdate::Gone
		));
	}

	/// If the current gossip path disappears while an older one is still live,
	/// the remaining target becomes current again.
	#[test]
	fn gossip_current_unannounce_restores_live_fallback() {
		let mut live = GossipTargets::default();
		let old = DialTarget::parse("https://peer.example/?cost=1").unwrap();
		let new = DialTarget::parse("https://peer.example/?cost=2").unwrap();
		live.announce("peer.example/?cost=1".to_string(), old.clone());
		live.announce("peer.example/?cost=2".to_string(), new.clone());

		let GossipUpdate::Current(current) = live.unannounce("peer.example/?cost=2", &new.key) else {
			panic!("older live advertisement must become current");
		};
		assert!(current == old);
	}

	/// `mark_unannounced` is a no-op for static peers (operator intent says
	/// "always dial"), so the sweep never has a stale timestamp to act on.
	#[tokio::test]
	async fn sweep_preserves_static_peer() {
		let dialed = DialMap::default();
		dialed.insert(target("static-peer:4443"), placeholder_handle(), DialSource::Static);

		let long_ago = Instant::now() - Duration::from_secs(3600);
		dialed.mark_unannounced("static-peer:4443", long_ago);
		dialed.sweep_stale(Instant::now(), STALE_AFTER, &mut |_| panic!("must not redial"));

		assert!(dialed.contains("static-peer:4443"));
	}

	/// A gossip-discovered peer whose unannounce has stuck longer than the
	/// threshold gets aborted and removed.
	#[tokio::test]
	async fn sweep_evicts_stale_gossip_peer() {
		let dialed = DialMap::default();
		let now = Instant::now();
		dialed.insert(target("gone:4443"), placeholder_handle(), DialSource::Gossip);
		dialed.mark_unannounced("gone:4443", now - STALE_AFTER - Duration::from_secs(1));

		dialed.sweep_stale(now, STALE_AFTER, &mut |_| panic!("must not redial"));

		assert!(!dialed.contains("gone:4443"));
	}

	/// A gossip-discovered peer whose unannounce is recent stays in the map; the
	/// sweep is only allowed to evict entries past the full threshold so that the
	/// prefer-shorter-hop flap (sub-millisecond unannounce-then-announce) doesn't
	/// trip it.
	#[tokio::test]
	async fn sweep_keeps_recently_unannounced_peer() {
		let dialed = DialMap::default();
		let now = Instant::now();
		dialed.insert(target("flapping:4443"), placeholder_handle(), DialSource::Gossip);
		dialed.mark_unannounced("flapping:4443", now - Duration::from_millis(50));

		dialed.sweep_stale(now, STALE_AFTER, &mut |_| panic!("must not redial"));

		assert!(dialed.contains("flapping:4443"));
	}

	/// A peer that's currently announced (no pending unannounce) is never swept.
	#[tokio::test]
	async fn sweep_keeps_currently_announced_peer() {
		let dialed = DialMap::default();
		dialed.insert(target("healthy:4443"), placeholder_handle(), DialSource::Gossip);
		// No mark_unannounced -> stays announced.

		dialed.sweep_stale(Instant::now(), STALE_AFTER, &mut |_| panic!("must not redial"));

		assert!(dialed.contains("healthy:4443"));
	}

	/// A re-announce after an unannounce clears the pending-sweep timestamp, so
	/// the entry survives even if the original unannounce was old enough to
	/// otherwise trigger eviction.
	#[tokio::test]
	async fn reannounce_cancels_pending_sweep() {
		let dialed = DialMap::default();
		let now = Instant::now();
		let target = target("flap:4443");
		dialed.insert(target.clone(), placeholder_handle(), DialSource::Gossip);
		dialed.mark_unannounced("flap:4443", now - STALE_AFTER - Duration::from_secs(1));

		// Re-adding the gossip source (a reannounce) clears the pending sweep.
		assert!(
			dialed.upsert(target.clone(), DialSource::Gossip, &mut |_| panic!("must not redial")),
			"should report a cleared pending-sweep"
		);
		dialed.sweep_stale(now, STALE_AFTER, &mut |_| panic!("must not redial"));

		assert!(dialed.contains("flap:4443"));
		// A second reannounce has nothing to clear.
		assert!(!dialed.upsert(target, DialSource::Gossip, &mut |_| panic!("must not redial")));
	}

	/// A peer wanted by both gossip and the API survives losing either source: the
	/// dial is only torn down once the last source releases it.
	#[tokio::test]
	async fn multi_source_peer_survives_until_last_release() {
		let dialed = DialMap::default();
		let now = Instant::now();
		// Gossiped first, then also appears in the API list.
		dialed.insert(target("both:4443"), placeholder_handle(), DialSource::Gossip);
		dialed.reconcile_api(&desired(&["both:4443"]), |_| panic!("already dialed"));

		// Dropped from the API list -> still wanted by gossip.
		dialed.reconcile_api(&HashMap::new(), |_| panic!("gossip dial stays active"));
		assert!(dialed.contains("both:4443"), "gossip still wants it");

		// Now gossip goes stale too -> the dial is finally released.
		dialed.mark_unannounced("both:4443", now - STALE_AFTER - Duration::from_secs(1));
		dialed.sweep_stale(now, STALE_AFTER, &mut |_| panic!("must not redial"));
		assert!(!dialed.contains("both:4443"));
	}

	/// `insert` for an already-dialed peer merges the source onto the existing
	/// entry (and aborts the redundant handle) rather than opening a second dial.
	#[tokio::test]
	async fn insert_merges_redundant_dial() {
		let dialed = DialMap::default();
		dialed.insert(target("p:4443"), placeholder_handle(), DialSource::Gossip);
		dialed.insert(target("p:4443"), placeholder_handle(), DialSource::Api);

		// Dropping the API source leaves the gossip source holding the dial.
		dialed.reconcile_api(&HashMap::new(), |_| panic!("gossip dial stays active"));
		assert!(dialed.contains("p:4443"), "gossip source still holds the dial");
	}

	/// `reconcile_api` drops API dials missing from the desired set, reports the
	/// newly desired ones for the caller to spawn, and never touches Static or
	/// Gossip dials (even when they're absent from the API list).
	#[tokio::test]
	async fn reconcile_api_adds_and_removes_only_api() {
		let dialed = DialMap::default();
		dialed.insert(target("static:4443"), placeholder_handle(), DialSource::Static);
		dialed.insert(target("gossip:4443"), placeholder_handle(), DialSource::Gossip);
		dialed.insert(target("api-keep:4443"), placeholder_handle(), DialSource::Api);
		dialed.insert(target("api-drop:4443"), placeholder_handle(), DialSource::Api);

		// Desired: keep one existing API peer, drop the other, add a new one.
		// Static/Gossip peers are not in the list but must survive.
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
		assert!(dialed.contains("gossip:4443"), "gossip peer must survive reconcile");
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
		let gossip = DialTarget::parse("https://peer.example/?cost=1").unwrap();
		let api = DialTarget::parse("https://peer.example/?cost=3").unwrap();
		let old_task = tokio::spawn(std::future::pending::<()>());
		dialed.insert(gossip.clone(), old_task.abort_handle(), DialSource::Gossip);

		let desired = [(api.key.clone(), api.clone())].into_iter().collect();
		dialed.reconcile_api(&desired, |_| panic!("inactive source must not redial"));
		assert!(!old_task.is_finished());

		let now = Instant::now();
		dialed.mark_unannounced(&gossip.key, now - STALE_AFTER - Duration::from_secs(1));
		let mut spawned = Vec::new();
		dialed.sweep_stale(now, STALE_AFTER, &mut |target| {
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
		dialed.insert(target.clone(), task.abort_handle(), DialSource::Gossip);

		let desired = [(target.key.clone(), target.clone())].into_iter().collect();
		dialed.reconcile_api(&desired, |_| panic!("inactive source must not redial"));

		let now = Instant::now();
		dialed.mark_unannounced(&target.key, now - STALE_AFTER - Duration::from_secs(1));
		dialed.sweep_stale(now, STALE_AFTER, &mut |_| panic!("identical fallback must not redial"));

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
		let cluster = Cluster::new(ClusterConfig::default()).expect("cluster");
		let dialed = DialMap::default();
		let current = DialTarget::parse("https://peer.example/?cost=1").unwrap();
		let task = tokio::spawn(std::future::pending::<()>());
		dialed.insert(current.clone(), task.abort_handle(), DialSource::Api);

		cluster.apply_peer_list(
			vec!["https://peer.example/?cost=invalid".to_string()],
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
				"https://peer.example/?cost=1".to_string(),
				"https://peer.example/?cost=2".to_string(),
			],
			None,
		) else {
			panic!("conflicting duplicate must fail");
		};
		assert!(format!("{err:#}").contains("conflicting configurations"));
	}

	/// The peer-list wire format is a JSON array of URL strings.
	#[test]
	fn peer_list_parses_as_string_array() {
		let body = r#"["a.pop.example", "b.pop.example:4443"]"#;
		let list: Vec<String> = serde_json::from_str(body).expect("parse peer list");
		assert_eq!(
			list,
			vec!["a.pop.example".to_string(), "b.pop.example:4443".to_string()]
		);
	}

	/// The mesh tiebreaker only dials peers that sort after us, so exactly one
	/// side of each pair opens the dial. Self never dials self.
	#[test]
	fn should_dial_prefers_larger_url() {
		// Smaller hostname is the client: it dials the larger.
		assert!(should_dial("a.example.com:4443", "b.example.com:4443"));
		// Larger hostname is the server: it waits for the inbound dial.
		assert!(!should_dial("b.example.com:4443", "a.example.com:4443"));
		// Never dial self.
		assert!(!should_dial("self.example.com:4443", "self.example.com:4443"));
	}

	/// Enabling gossip (`--cluster-mesh`) without `--cluster-node` has no address to
	/// advertise, so it must fail fast with a message naming the missing flag.
	#[tokio::test]
	async fn gossip_without_node_errors() {
		let config = ClusterConfig {
			mesh: Some("true".to_string()),
			..Default::default()
		};
		let err = Cluster::new(config).unwrap().start().await.expect_err("should error");
		let msg = format!("{err}");
		assert!(msg.contains("--cluster-node"), "missing --cluster-node in: {msg}");
		assert!(msg.contains("--cluster-mesh"), "missing --cluster-mesh in: {msg}");
	}

	/// A valid `cluster.id` is used verbatim as the relay's origin id, giving the
	/// node a stable identity across restarts.
	#[tokio::test]
	async fn cluster_id_sets_origin() {
		let cluster = Cluster::new(ClusterConfig {
			id: Some(42),
			..Default::default()
		})
		.expect("valid id");
		assert_eq!(cluster.origin.id(), 42);
	}

	/// A reserved (0) or out-of-range (>= 2^62) `cluster.id` is rejected rather
	/// than producing an unencodable hop id.
	#[test]
	fn cluster_id_out_of_range_errors() {
		for bad in [0, 1u64 << 62] {
			let err = Cluster::new(ClusterConfig {
				id: Some(bad),
				..Default::default()
			})
			.err()
			.expect("should error");
			assert!(format!("{err}").contains("--cluster-id"), "got: {err}");
		}
	}

	/// A relay configured with `cluster.node` + `cluster.mesh` gossip and no peers
	/// (passive rendezvous) must run without a QUIC client, publish its
	/// self-registration on the cluster origin, and keep that registration alive
	/// (i.e. not exit and drop the broadcast).
	#[tokio::test(start_paused = true)]
	async fn passive_rendezvous_runs_without_client_and_advertises_self() {
		let cluster = Cluster::new(ClusterConfig {
			node: Some("rendezvous.example.com:4443".to_string()),
			mesh: Some("true".to_string()),
			..Default::default()
		})
		.unwrap();

		// Snapshot a consumer on the cluster origin before run() takes ownership of
		// `cluster` so we can later check that the registration was published.
		let mut watcher = cluster.origin.consume().announced();

		let started = cluster.clone().start().await.expect("cluster start");
		let mut handle = tokio::spawn(async move { started.run().await });

		// Give the runtime a moment to execute the synchronous setup work.
		tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

		// The self-registration broadcast must be visible on the origin.
		let moq_net::announce::Update { path, broadcast } =
			watcher.try_next().expect("self-registration must be published");
		assert_eq!(path.as_str(), ".internal/origins/rendezvous.example.com:4443");
		assert!(broadcast.is_some());

		// run() must NOT have returned: dropping the broadcast (via run returning)
		// would unannounce the registration immediately. Use a short timeout to
		// confirm we're still parked.
		let still_running = tokio::time::timeout(tokio::time::Duration::from_millis(50), &mut handle)
			.await
			.is_err();
		assert!(still_running, "passive rendezvous run() should park, not return");

		handle.abort();
	}

	/// `cluster.node` (identity) and `cluster.mesh` (gossip toggle) round-trip
	/// through TOML and survive the CLI re-parse when no flags override them.
	/// `mesh` is a string (clobber-safe) that accepts a TOML boolean.
	#[test]
	fn cluster_node_and_mesh_round_trip() {
		// clap reads the environment while parsing, so serialize with the tests
		// that mutate it.
		let _env = crate::test_env::EnvGuard::lock();

		let toml =
			"[cluster]\nnode = \"us-east.example.com:4443\"\nmesh = true\nconnect = [\"root.example.com:4443\"]\n";
		let dir = std::env::temp_dir().join("moq-relay-cluster-test");
		std::fs::create_dir_all(&dir).unwrap();
		let path = dir.join("cluster-node-mesh-toml.toml");
		std::fs::write(&path, toml).unwrap();

		let args = vec![std::ffi::OsString::from("moq-relay"), std::ffi::OsString::from(&path)];
		let config = Config::parse_and_merge(args).expect("config load");
		assert_eq!(config.cluster.node.as_deref(), Some("us-east.example.com:4443"));
		// A TOML boolean deserializes into the string form.
		assert_eq!(config.cluster.mesh.as_deref(), Some("true"));
		assert_eq!(config.cluster.connect, vec!["root.example.com:4443".to_string()]);
	}

	/// The legacy `--cluster-mesh <url>` form (now a boolean) is honored for
	/// backwards compatibility: it enables gossip and supplies the node URL.
	#[tokio::test]
	async fn legacy_mesh_url_enables_gossip_as_node() {
		let cluster = Cluster::new(ClusterConfig {
			mesh: Some("rendezvous.example.com:4443".to_string()),
			..Default::default()
		})
		.unwrap();
		let (gossip, node) = cluster.resolve_mesh().expect("legacy mesh url resolves");
		assert!(gossip);
		assert_eq!(node.as_deref(), Some("rendezvous.example.com:4443"));
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

	/// A malformed URL may contain a credential, so parse errors must not echo the
	/// raw input into logs.
	#[test]
	fn peer_url_error_redacts_inline_credential() {
		let err = peer_url("https://peer.example:bad/?jwt=top-secret").unwrap_err();
		assert!(!format!("{err:#}").contains("top-secret"));
	}

	/// What a discovering relay reads off `announced()` for this node.
	fn advertised(node: &str) -> String {
		Path::new(MESH_PREFIX)
			.join(node)
			.as_str()
			.strip_prefix(&format!("{MESH_PREFIX}/"))
			.expect("advertised under the mesh prefix")
			.to_owned()
	}

	/// A node URL is advertised as a broadcast path, which collapses the `//` after
	/// the scheme. It has to come back as the same address anyway, or every
	/// gossip-discovered peer is dialed as the hostname `https`.
	#[test]
	fn a_node_url_survives_a_broadcast_path() {
		for node in [
			"https://a.example/",
			"https://b.example:4443/",
			"tcp://c.example:4443",
			"https://d.example/?cost=7",
			"https://e.example/deep/path",
			// Legacy bare forms, which carry no scheme and default to https.
			"rendezvous.example.com:4443",
			"cdn.example.com",
		] {
			let dialed = advertised_node_url(&advertised(node));
			assert_eq!(
				peer_url(&dialed).unwrap(),
				peer_url(node).unwrap(),
				"{node} did not survive a round trip through a broadcast path"
			);
		}
	}

	/// The dialed address keeps its query. `run_remote` reads `?cost=` off it to
	/// price the link, so canonicalizing first (which drops the query) would leave
	/// every gossip-discovered link silently at the default cost.
	#[test]
	fn an_advertised_url_keeps_the_query_it_is_dialed_with() {
		let dialed = advertised_node_url(&advertised("https://a.example/?cost=10"));
		assert_eq!(dialed, "https://a.example/?cost=10");

		// The value has to reach where `run_remote` reads it.
		let mut url = peer_url(&dialed).unwrap();
		assert_eq!(take_cost(&mut url).unwrap(), Some(10));
		assert_eq!(url.as_str(), "https://a.example/");
	}

	/// The colon that ends a scheme is the only signal, so a bare `host:port` (whose
	/// colon sits mid-segment) must not be read as one.
	#[test]
	fn a_bare_host_port_is_not_read_as_a_scheme() {
		assert_eq!(
			advertised_node_url("rendezvous.example.com:4443"),
			"rendezvous.example.com:4443"
		);
		assert_eq!(
			canonicalize_peer_key(&advertised_node_url("rendezvous.example.com:4443")),
			"https://rendezvous.example.com:4443/"
		);
	}

	/// What a discovering relay reads off an mDNS record for this node, mirroring
	/// [`lan_discovery`] and `moq_tokio::mdns::Config::with_node`.
	fn advertised_lan(node: &str) -> String {
		let mut url = peer_url(node).expect("valid node");
		url.set_fragment(None);
		url.set_username("").ok();
		url.set_password(None).ok();
		url.to_string()
	}

	/// The two discovery paths must land on the same peer key, or one relay gets
	/// two [`DialMap`] entries and is dialed twice. This is self-triggering: the
	/// session the mDNS dial opens is what carries the peer's gossip
	/// advertisement, so the second dial follows the first.
	///
	/// Gossip round-trips the node URL through a [`Path`] (slashes trimmed and
	/// collapsed) while mDNS strips the fragment and userinfo, so the key has to
	/// absorb both. The realistic trigger is a trailing slash: `moqt` is not a
	/// special scheme, so `Url` keeps one that `Path` would have dropped.
	#[test]
	fn gossip_and_mdns_agree_on_a_peer_key() {
		for node in [
			"moqt://a.example:4443/",
			"https://a.example/edge/",
			"https://a.example/#pos",
			"https://user:pw@a.example/",
			// Controls: spellings both paths already agreed on.
			"https://a.example/",
			"https://b.example:4443/deep/path",
			"tcp://c.example:4443",
			"rendezvous.example.com:4443",
		] {
			let gossip = canonicalize_peer_key(&advertised_node_url(&advertised(node)));
			let mdns = canonicalize_peer_key(&advertised_lan(node));
			assert_eq!(gossip, mdns, "{node} has two peer keys, so it is dialed twice");
		}
	}

	/// With both sides canonical, exactly one of a pair dials.
	#[test]
	fn gossiped_urls_keep_the_tiebreaker_symmetric() {
		let a = canonicalize_peer_key("https://a.example/");
		let b = canonicalize_peer_key(&advertised_node_url("https:/b.example"));
		assert!(
			should_dial(&a, &b) != should_dial(&b, &a),
			"exactly one side of a pair must dial"
		);
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

	/// A legacy mesh URL that disagrees with an explicit `--cluster-node` is a
	/// conflict, not a silent pick.
	#[tokio::test]
	async fn legacy_mesh_url_conflicting_with_node_errors() {
		let cluster = Cluster::new(ClusterConfig {
			mesh: Some("a.example.com:4443".to_string()),
			node: Some("b.example.com:4443".to_string()),
			..Default::default()
		})
		.unwrap();
		let err = cluster.resolve_mesh().expect_err("conflict should error");
		assert!(format!("{err}").contains("conflicts with"), "got: {err}");
	}

	/// `cluster.connect_api` set in TOML must survive the CLI re-parse when no
	/// `--cluster-connect-api` flag is passed (same clap+TOML clobber pitfall the
	/// config tests guard, which is why the field is `Option<String>`).
	/// The nested `[cluster.lan]` table survives the TOML -> CLI merge, the
	/// footgun that `Option<T>` fields exist to avoid.
	#[cfg(feature = "cluster-lan")]
	#[test]
	fn cluster_lan_survives_toml_merge() {
		// clap reads the environment while parsing, so serialize with the tests
		// that mutate it.
		let _env = crate::test_env::EnvGuard::clear(&["MOQ_CLUSTER_LAN", "MOQ_CLUSTER_LAN_SECRET"]);

		let toml = "[cluster]\nnode = \"https://relay.example.com\"\n\n[cluster.lan]\nenabled = true\nsecret = \"cluster.key\"\n";
		let dir = std::env::temp_dir().join("moq-relay-cluster-test");
		std::fs::create_dir_all(&dir).unwrap();
		let path = dir.join("cluster-lan-toml.toml");
		std::fs::write(&path, toml).unwrap();

		let args = vec![std::ffi::OsString::from("moq-relay"), std::ffi::OsString::from(&path)];
		let config = Config::parse_and_merge(args).expect("config load");
		assert_eq!(config.cluster.lan.enabled, Some(true));
		assert_eq!(config.cluster.lan.secret.as_deref(), Some("cluster.key"));
		assert_eq!(config.cluster.node.as_deref(), Some("https://relay.example.com"));
	}

	/// A CLI flag overrides the same key from TOML, rather than the nesting
	/// hiding it from `update_from`.
	#[cfg(feature = "cluster-lan")]
	#[test]
	fn cli_overrides_toml_cluster_lan() {
		let _env = crate::test_env::EnvGuard::clear(&["MOQ_CLUSTER_LAN", "MOQ_CLUSTER_LAN_SECRET"]);

		let toml = "[cluster.lan]\nenabled = true\nsecret = \"from-toml.key\"\n";
		let dir = std::env::temp_dir().join("moq-relay-cluster-test");
		std::fs::create_dir_all(&dir).unwrap();
		let path = dir.join("cluster-lan-override.toml");
		std::fs::write(&path, toml).unwrap();

		let args = vec![
			std::ffi::OsString::from("moq-relay"),
			std::ffi::OsString::from(&path),
			std::ffi::OsString::from("--cluster-lan-secret"),
			std::ffi::OsString::from("from-cli.key"),
		];
		let config = Config::parse_and_merge(args).expect("config load");
		assert_eq!(config.cluster.lan.secret.as_deref(), Some("from-cli.key"));
		assert_eq!(config.cluster.lan.enabled, Some(true), "the untouched key survives");
	}

	/// The LAN needs an address to advertise, like gossip does.
	#[cfg(feature = "cluster-lan")]
	#[tokio::test]
	async fn lan_without_node_errors() {
		let config = ClusterConfig {
			lan: LanConfig {
				enabled: Some(true),
				..Default::default()
			},
			..Default::default()
		};
		let err = Cluster::new(config)
			.unwrap()
			.start()
			.await
			.expect_err("--cluster-lan without --cluster-node must fail");
		let msg = format!("{err}");
		assert!(msg.contains("--cluster-node"), "missing --cluster-node in: {msg}");
		assert!(msg.contains("--cluster-lan"), "missing --cluster-lan in: {msg}");
	}

	/// mDNS is unauthenticated, so a discovered URL is only safe to dial with
	/// `--cluster-token` attached once its advertiser proved it holds the shared
	/// key. Starting without one would leak the token to any advertiser, so it is
	/// refused rather than defaulted to an open mesh.
	#[cfg(feature = "cluster-lan")]
	#[tokio::test]
	async fn lan_without_a_secret_errors() {
		let config = ClusterConfig {
			node: Some("https://us-west.example.com".to_string()),
			lan: LanConfig {
				enabled: Some(true),
				..Default::default()
			},
			..Default::default()
		};
		let err = Cluster::new(config)
			.unwrap()
			.start()
			.await
			.expect_err("--cluster-lan without --cluster-lan-secret must fail");
		let msg = format!("{err}");
		assert!(msg.contains("--cluster-lan-secret"), "missing the secret in: {msg}");
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
		// clap reads the environment while parsing, so serialize with the tests
		// that mutate it.
		let _env = crate::test_env::EnvGuard::lock();

		let toml = "[cluster]\nconnect_api = \"https://api.example.com/cluster/connect\"\n";
		let dir = std::env::temp_dir().join("moq-relay-cluster-test");
		std::fs::create_dir_all(&dir).unwrap();
		let path = dir.join("cluster-connect-api-toml.toml");
		std::fs::write(&path, toml).unwrap();

		let args = vec![std::ffi::OsString::from("moq-relay"), std::ffi::OsString::from(&path)];
		let config = Config::parse_and_merge(args).expect("config load");
		assert_eq!(
			config.cluster.connect_api.as_deref(),
			Some("https://api.example.com/cluster/connect")
		);
	}
}
