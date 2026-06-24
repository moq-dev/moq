use std::{
	collections::{HashMap, HashSet},
	path::PathBuf,
	sync::{Arc, Mutex},
	time::{Duration, Instant},
};

use anyhow::Context;
use moq_net::{BroadcastPublish, Origin, OriginProducer, Path, Stats, Tier};
use reqwest_middleware::ClientWithMiddleware;
use tokio::task::AbortHandle;
use url::Url;

use crate::AuthToken;

/// Path prefix under which cluster nodes advertise their own URLs for gossip-style
/// peer discovery.
const MESH_PREFIX: &str = ".internal/origins";

/// How often the discovery loop scans for stale entries.
const SWEEP_INTERVAL: Duration = Duration::from_secs(30);

/// How long a peer must stay unannounced before we abort the dial. Must clear the
/// "prefer shorter hop" restart flap (which arrives as unannounce-then-announce
/// within sub-milliseconds) plus reasonable churn from a peer restart.
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
}

/// The set of [`DialSource`]s currently keeping a dial alive.
#[derive(Clone, Copy, Default)]
struct DialSources {
	seeded: bool,
	gossip: bool,
	api: bool,
}

impl DialSources {
	fn set(&mut self, source: DialSource) {
		match source {
			DialSource::Static => self.seeded = true,
			DialSource::Gossip => self.gossip = true,
			DialSource::Api => self.api = true,
		}
	}

	fn clear(&mut self, source: DialSource) {
		match source {
			DialSource::Static => self.seeded = false,
			DialSource::Gossip => self.gossip = false,
			DialSource::Api => self.api = false,
		}
	}

	fn any(&self) -> bool {
		self.seeded || self.gossip || self.api
	}
}

/// One entry in [`DialMap`]. `unannounced_at` carries the gossip stale timer; the
/// sweep uses it to decide when the gossip source has truly gone vs. is just
/// flapping between paths. It's only meaningful while `sources.gossip` is set.
struct DialEntry {
	handle: AbortHandle,
	sources: DialSources,
	unannounced_at: Option<Instant>,
}

/// Map of in-flight cluster dials, keyed by peer URL. Cloneable: the inner
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

	/// Record a freshly-spawned dial for `peer` under `source`. If `peer` is
	/// already dialed, add `source` to its set and abort the redundant `handle`
	/// (the existing dial stands, since dialing dedupes by URL). Always spawn the
	/// task first, then call this: it resolves the "two sources discover the same
	/// peer at once" race without leaking a task.
	fn insert(&self, peer: String, handle: AbortHandle, source: DialSource) {
		let mut map = self.inner.lock().expect("dial map poisoned");
		if let Some(entry) = map.get_mut(&peer) {
			entry.sources.set(source);
			if source == DialSource::Gossip {
				entry.unannounced_at = None;
			}
			drop(map);
			handle.abort();
		} else {
			let mut sources = DialSources::default();
			sources.set(source);
			map.insert(
				peer,
				DialEntry {
					handle,
					sources,
					unannounced_at: None,
				},
			);
		}
	}

	/// Add `source` to an already-dialed `peer` (no-op if absent). Used when a
	/// peer reached via one source is also discovered via another, without opening
	/// a second dial. Adding [`DialSource::Gossip`] also clears any pending
	/// unannounce; returns whether such a timestamp was cleared (a reannounce).
	fn add_source(&self, peer: &str, source: DialSource) -> bool {
		let mut map = self.inner.lock().expect("dial map poisoned");
		let Some(entry) = map.get_mut(peer) else { return false };
		entry.sources.set(source);
		source == DialSource::Gossip && entry.unannounced_at.take().is_some()
	}

	/// Start the gossip stale timer on `peer` if it isn't already pending. No-op
	/// unless the peer is currently wanted by gossip. Idempotent: a repeat
	/// unannounce while a timestamp is pending doesn't reset the clock.
	fn mark_unannounced(&self, peer: &str, now: Instant) {
		let mut map = self.inner.lock().expect("dial map poisoned");
		if let Some(entry) = map.get_mut(peer)
			&& entry.sources.gossip
		{
			entry.unannounced_at.get_or_insert(now);
		}
	}

	/// Release the gossip source from entries whose unannounce has stuck for at
	/// least `threshold`, aborting the dial only if no other source still wants it.
	fn sweep_stale(&self, now: Instant, threshold: Duration) {
		let mut map = self.inner.lock().expect("dial map poisoned");
		map.retain(|peer, entry| {
			let Some(at) = entry.unannounced_at else { return true };
			if now.duration_since(at) < threshold {
				return true;
			}
			entry.unannounced_at = None;
			entry.sources.clear(DialSource::Gossip);
			if entry.sources.any() {
				tracing::debug!(%peer, "peer no longer gossiped; still wanted by another source");
				true
			} else {
				tracing::info!(%peer, "peer no longer gossiped; abandoning dial");
				entry.handle.abort();
				false
			}
		});
	}

	/// Reconcile the API source against `desired`: release [`DialSource::Api`] from
	/// entries no longer listed (aborting only those nothing else wants), mark the
	/// API source on already-dialed peers that are listed, and return the desired
	/// peers not yet dialed (the caller spawns those and re-inserts them).
	fn reconcile_api(&self, desired: &HashSet<String>) -> Vec<String> {
		let mut map = self.inner.lock().expect("dial map poisoned");

		// One mutable pass: set the API source on listed peers, release it from the
		// rest (aborting only those nothing else wants).
		map.retain(|peer, entry| {
			if desired.contains(peer) {
				entry.sources.set(DialSource::Api);
				return true;
			}
			if !entry.sources.api {
				return true;
			}
			entry.sources.clear(DialSource::Api);
			if entry.sources.any() {
				tracing::debug!(%peer, "peer dropped from cluster-connect-api; still wanted by another source");
				true
			} else {
				tracing::info!(%peer, "peer dropped from cluster-connect-api; abandoning dial");
				entry.handle.abort();
				false
			}
		});

		// Whatever's left in `desired` but absent from the map needs a fresh dial.
		desired
			.iter()
			.filter(|peer| !map.contains_key(*peer))
			.cloned()
			.collect()
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
	/// of peer hostnames: `["a.pop.example", "b.pop.example"]`. An http(s) URL is
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

	/// JWT presented on outbound cluster dials, read from this file. Applied to
	/// any peer whose URL doesn't already carry a `?jwt=` (so it authenticates
	/// gossip- and `connect_api`-discovered peers, whose addresses can't embed a
	/// token). For static `--cluster-connect` peers, prefer an inline `?jwt=`.
	#[arg(id = "cluster-token", long = "cluster-token", env = "MOQ_CLUSTER_TOKEN")]
	pub token: Option<PathBuf>,

	/// Billing tier label that cluster-peer (relay-to-relay) traffic records
	/// stats under. Default `internal`. An empty value selects the default
	/// (unprefixed) tier.
	#[arg(id = "cluster-tier", long = "cluster-tier", env = "MOQ_CLUSTER_TIER")]
	pub tier: Option<String>,
}

/// A relay cluster built around a single [`OriginProducer`].
///
/// Local sessions and remote cluster connections all publish into the same
/// origin. Loop prevention and shortest-path preference come from the
/// hop list carried on each broadcast (see [`moq_net::BroadcastInfo::hops`]).
///
/// Construct with [`Cluster::new`], then attach a QUIC client and (optionally)
/// a [`Stats`] aggregator with the `with_*` builder methods. A cluster without
/// a client can serve local sessions but cannot dial remote peers.
#[derive(Clone)]
pub struct Cluster {
	config: ClusterConfig,
	client: Option<moq_native::Client>,

	/// Client TLS config used to build the `--cluster-connect-api` HTTP client, so
	/// peer-list fetches present the same cluster cert the QUIC dials do. `Arc` so
	/// cloning a `Cluster` per connection stays cheap.
	client_tls: Option<Arc<rustls::ClientConfig>>,

	/// All broadcasts, local and remote. Downstream sessions read from here
	/// (filtered by their auth token) and remote dials both read and write here.
	pub origin: OriginProducer,

	/// Stats aggregator. One instance per relay; sessions pick a billing tier via
	/// [`Stats::tier`] at acceptance time (default tier for JWT/public, `internal`
	/// for mTLS / cluster peers, or any label the auth API returns) so traffic
	/// classes land in separate counter sets. Defaults to a no-op aggregator
	/// ([`Stats::default`]) until [`with_stats`](Self::with_stats) is called.
	pub stats: Stats,
}

impl Cluster {
	/// Creates a new cluster with a fresh origin and no peers, client, or stats.
	///
	/// Use [`with_client`](Self::with_client) to enable dialing remote peers
	/// (required when `config.connect` is non-empty), and
	/// [`with_stats`](Self::with_stats) to enable metrics publishing.
	///
	/// Errors if `config.id` is set but invalid: it must be non-zero and below
	/// 2^62 (the wire varint limit). An unset id picks a fresh random origin.
	pub fn new(config: ClusterConfig) -> anyhow::Result<Self> {
		let origin = match config.id {
			Some(0) => anyhow::bail!("--cluster-id must be non-zero"),
			Some(id) if id >= 1 << 62 => {
				anyhow::bail!("--cluster-id must be below 2^62 (wire varint limit), got {id}")
			}
			Some(id) => Origin::from(id),
			None => Origin::random(),
		}
		.produce();
		tracing::info!(origin_id = %origin.id, configured = config.id.is_some(), "cluster initialized");
		Ok(Cluster {
			config,
			client: None,
			client_tls: None,
			origin,
			stats: Stats::default(),
		})
	}

	/// Attach a QUIC client used to dial cluster peers.
	///
	/// Required when `config.connect` is non-empty; [`run`](Self::run) returns
	/// an error otherwise.
	pub fn with_client(mut self, client: moq_native::Client) -> Self {
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

	/// Attach a [`Stats`] aggregator. Replaces the default no-op aggregator.
	///
	/// Build the value with [`StatsConfig::build`](crate::StatsConfig::build),
	/// passing [`Self::origin`] so the aggregator publishes through the same
	/// origin cluster peers read from.
	pub fn with_stats(mut self, stats: Stats) -> Self {
		self.stats = stats;
		self
	}

	/// Billing tier cluster-peer traffic records under (`--cluster-tier`,
	/// default `internal`). An empty label is the default (unprefixed) tier.
	fn cluster_tier(&self) -> Tier {
		crate::trusted_tier(self.config.tier.clone())
	}

	/// Returns an [`OriginProducer`] scoped to this session's subscribe permissions.
	///
	/// Passed by reference to [`moq_net::Server::with_publisher`] (or the
	/// equivalent per-request setter), which derives the read handle.
	pub fn subscriber(&self, token: &AuthToken) -> Option<OriginProducer> {
		self.origin.with_root(&token.root)?.scope(&token.subscribe)
	}

	/// Returns an [`OriginProducer`] scoped to this session's publish permissions.
	pub fn publisher(&self, token: &AuthToken) -> Option<OriginProducer> {
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

	/// Runs the cluster event loop.
	///
	/// Modes are derived from config: standalone (no work) returns immediately;
	/// passive rendezvous (`node` + `mesh` gossip, no peers to dial) parks after
	/// publishing self-registration and does not require a QUIC client; active
	/// (`connect` / `connect_api` set) dials peers and, when `mesh` gossip is on,
	/// also advertises `node` and runs discovery.
	///
	/// Bails when `mesh` gossip is on without `node`, or when peers are configured
	/// to dial but no client was attached via [`with_client`](Self::with_client).
	pub async fn run(self) -> anyhow::Result<()> {
		let (gossip, node) = self.resolve_mesh()?;
		anyhow::ensure!(
			!gossip || node.is_some(),
			"`--cluster-mesh` (gossip) requires `--cluster-node <self-url>` so there's an address to advertise. \
			 See https://doc.moq.dev/bin/relay/cluster."
		);

		let has_outbound = !self.config.connect.is_empty() || self.config.connect_api.is_some();
		let has_work = has_outbound || gossip;
		if !has_work {
			tracing::info!("no cluster peers configured; running standalone");
			return Ok(());
		}

		if has_outbound {
			anyhow::ensure!(
				self.client.is_some(),
				"cluster peers configured but no QUIC client attached (call Cluster::with_client)"
			);
		}

		// Token presented on outbound dials whose URL doesn't already carry a
		// `?jwt=`. This is how gossip- and connect_api-discovered peers (whose
		// addresses can't carry an inline token) authenticate, so it isn't
		// deprecated; for static `connect` peers, an inline `?jwt=` is preferred.
		let token = match &self.config.token {
			Some(path) => std::fs::read_to_string(path)
				.context("failed to read cluster token")?
				.trim()
				.to_string(),
			None => String::new(),
		};

		// Static `--cluster-connect` peers and gossip-discovered peers share one
		// dial map so a peer reached via both paths only opens a single dial.
		// Gossip-driven unannounces don't abort immediately — the discovery loop
		// runs a periodic sweep that only aborts entries whose unannounce has
		// stuck for [`STALE_AFTER`]. That filters out the prefer-shorter-hop flap
		// (sub-millisecond unannounce-then-announce) while still cleaning up
		// peers that truly left.
		let dialed = DialMap::default();
		let mut tasks = tokio::task::JoinSet::new();

		for peer in &self.config.connect {
			let key = canonicalize_peer_key(peer);
			if dialed.contains(&key) {
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
			let peer_for_task = peer.clone();
			let handle = tasks.spawn(async move {
				if let Err(err) = this.run_remote(&peer_for_task, token).await {
					tracing::warn!(%err, peer = %peer_for_task, "cluster peer connection ended");
				}
			});
			dialed.insert(key, handle, DialSource::Static);
		}

		if let Some(source) = self.config.connect_api.clone() {
			// Only http(s) sources need the TLS client; a local file doesn't.
			anyhow::ensure!(
				!connect_api_is_http(&source) || self.client_tls.is_some(),
				"cluster.connect_api with an http(s) URL needs client TLS (call Cluster::with_client_tls)"
			);
			let this = self.clone();
			let token = token.clone();
			let dialed = dialed.clone();
			let node = node.clone();
			tasks.spawn(async move {
				this.run_connect_api(source, node, token, dialed).await;
			});
		}

		// Held in scope so the registration stays announced until `run` exits.
		// Discovery is paired with it: a gossip-only relay (passive rendezvous) has
		// nothing to discover, so we only run it when we also have an outbound peer.
		let _self_registration: Option<BroadcastPublish> = if gossip {
			// Checked above: gossip requires `node`.
			let node = node.as_deref().expect("gossip requires --cluster-node");
			let path = Path::new(MESH_PREFIX).join(node);
			let broadcast = self
				.origin
				.create_broadcast(&path)
				.expect(".internal/origins is within the relay origin's root");
			tracing::info!(%node, %path, "advertising cluster node URL");

			if has_outbound {
				let this = self.clone();
				let token = token.clone();
				let dialed = dialed.clone();
				let self_url = node.to_owned();
				tasks.spawn(async move {
					this.run_discovery(self_url, token, dialed).await;
				});
			}

			Some(broadcast)
		} else {
			None
		};

		if tasks.is_empty() {
			// Passive rendezvous: park to keep `_self_registration` alive. The
			// process still exits via the other arms of `tokio::select!` in main.
			std::future::pending::<()>().await
		}

		while tasks.join_next().await.is_some() {}
		Ok(())
	}

	/// Watch `.internal/origins/*` for peer registrations and dial each newly-
	/// announced URL that sorts after our own (see [`should_dial`]); the peer on
	/// the other side of that comparison dials us, so each pair opens one session
	/// instead of two. Unannounces don't abort immediately. They just mark the
	/// entry as "pending cleanup" with a timestamp. A periodic sweep evicts
	/// entries whose unannounce has stuck for [`STALE_AFTER`]. The "prefer
	/// shorter hop" path in OriginProducer delivers restartments as
	/// unannounce-then-announce within sub-milliseconds, which clears the
	/// pending-cleanup timestamp long before the sweep fires.
	async fn run_discovery(self, self_url: String, token: String, dialed: DialMap) {
		let Some(consumer) = self.origin.consume().with_root(MESH_PREFIX) else {
			tracing::warn!("could not scope cluster origin to {MESH_PREFIX}; discovery disabled");
			return;
		};
		let mut announced = consumer.announced();

		let mut sweep = tokio::time::interval(SWEEP_INTERVAL);
		sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
		// Skip the first immediate tick; nothing has had a chance to go stale yet.
		sweep.tick().await;

		loop {
			tokio::select! {
				ann = announced.next() => {
					let Some((relative, event)) = ann else { return; };
					let peer = relative.as_str();
					// Skip self and any peer we lose the tiebreaker to; that side
					// dials us instead, so each pair forms a single session.
					if !should_dial(&self_url, peer) {
						continue;
					}
					let peer = peer.to_owned();
					let key = canonicalize_peer_key(&peer);
					match event.broadcast() {
						Some(_) => {
							if dialed.contains(&key) {
								// Already dialed (possibly via another source). Mark gossip as
								// a wanter and cancel any pending stale-sweep.
								if dialed.add_source(&key, DialSource::Gossip) {
									tracing::debug!(%peer, "reannounce within sweep window; keeping dial");
								}
								continue;
							}
							tracing::info!(%peer, "discovered cluster peer; dialing");
							let this = self.clone();
							let token = token.clone();
							let peer_for_task = peer.clone();
							let handle = tokio::spawn(async move {
								if let Err(err) = this.run_remote(&peer_for_task, token).await {
									tracing::warn!(%err, peer = %peer_for_task, "cluster peer connection ended");
								}
							});
							dialed.insert(key, handle.abort_handle(), DialSource::Gossip);
						}
						None => {
							dialed.mark_unannounced(&key, Instant::now());
						}
					}
				}
				_ = sweep.tick() => {
					dialed.sweep_stale(Instant::now(), STALE_AFTER);
				}
			}
		}
	}

	/// Drive `--cluster-connect-api`: an http(s) URL is polled, a local path (or
	/// `file://` URL) is watched for changes. Either way the source yields a JSON
	/// array of peer hostnames that's reconciled into the shared dial map.
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
	/// [`moq_native::watch::FileWatcher`] (OS notifications with a polling fallback).
	/// Fails static: a missing or malformed file keeps the current dials, and the
	/// next change triggers a fresh attempt.
	async fn run_connect_api_file(&self, path: PathBuf, node: Option<String>, token: String, dialed: DialMap) {
		self.reload_connect_api_file(&path, &node, &token, &dialed);

		let mut watcher = match moq_native::watch::FileWatcher::new(std::slice::from_ref(&path)) {
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
	/// current dials; the [`FileWatcher`](moq_native::watch::FileWatcher) only
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

		serde_json::from_str(&body).context("cluster.connect_api response is not a JSON array of hostnames")
	}

	/// Reconcile a freshly fetched peer list into the dial map: dial peers that
	/// are new and drop API peers that disappeared. The relay's own [`node`] URL
	/// is filtered out so it never dials itself.
	fn apply_peer_list(&self, list: Vec<String>, node: &Option<String>, token: &str, dialed: &DialMap) {
		// Dedupe against the shared dial map (and filter out self) on the canonical
		// key, so an API entry matches the same peer reached via `connect`/gossip
		// regardless of how each spells it. reconcile_api then yields canonical keys.
		let self_key = node.as_deref().map(canonicalize_peer_key);
		let desired: HashSet<String> = list
			.into_iter()
			.map(|peer| canonicalize_peer_key(&peer))
			.filter(|key| Some(key) != self_key.as_ref())
			.collect();

		for peer in dialed.reconcile_api(&desired) {
			tracing::info!(%peer, "cluster.connect_api peer; dialing");
			let this = self.clone();
			let token = token.to_string();
			let peer_for_task = peer.clone();
			let handle = tokio::spawn(async move {
				if let Err(err) = this.run_remote(&peer_for_task, token).await {
					tracing::warn!(%err, peer = %peer_for_task, "cluster peer connection ended");
				}
			});
			dialed.insert(peer, handle.abort_handle(), DialSource::Api);
		}
	}

	#[tracing::instrument("remote", skip_all, err, fields(%remote))]
	async fn run_remote(self, remote: &str, token: String) -> anyhow::Result<()> {
		let mut url = peer_url(remote)?;
		// Apply the shared cluster token unless the URL already carries its own
		// non-empty `?jwt=` (an inline token on a static `connect` peer wins; the
		// shared token still covers discovered peers that have none). An empty
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
			let result = self.run_remote_once(&url).await;
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

	async fn run_remote_once(&self, url: &Url) -> anyhow::Result<()> {
		let mut log_url = url.clone();
		log_url.set_query(None);
		tracing::info!(url = %log_url, "dialing cluster peer");

		// Checked at the start of `run`; per-peer tasks inherit that guarantee.
		let client = self
			.client
			.clone()
			.context("internal: cluster peer dial without an attached QUIC client")?;

		// Cluster-to-cluster traffic is internal by definition.
		let cs = client
			.with_publisher(&self.origin)
			.with_subscriber(self.origin.clone())
			.with_stats(self.stats.tier(self.cluster_tier()))
			.connect(url.clone())
			.await
			.context("failed to connect to cluster peer")?;

		cs.closed().await.map_err(Into::into)
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
/// verbatim. A bare host or `host:port` is still accepted for backwards
/// compatibility and wrapped in `https://.../` (callers warn about this legacy
/// form for user-supplied `--cluster-connect` entries).
fn peer_url(peer: &str) -> anyhow::Result<Url> {
	// A full URL has a scheme separator; a bare host or `host:port` does not
	// (and `Url::parse` would otherwise mis-read `host:port` as scheme `host`).
	if peer.contains("://") {
		return Url::parse(peer).with_context(|| format!("invalid cluster peer URL: {peer}"));
	}

	Url::parse(&format!("https://{peer}/")).with_context(|| format!("invalid cluster peer host: {peer}"))
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
fn canonicalize_peer_key(peer: &str) -> String {
	match peer_url(peer) {
		Ok(mut url) => {
			url.set_query(None);
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

	/// Stand-in dial task: never makes progress, exposes an AbortHandle.
	fn placeholder_handle() -> AbortHandle {
		tokio::spawn(std::future::pending::<()>()).abort_handle()
	}

	/// `mark_unannounced` is a no-op for static peers (operator intent says
	/// "always dial"), so the sweep never has a stale timestamp to act on.
	#[tokio::test]
	async fn sweep_preserves_static_peer() {
		let dialed = DialMap::default();
		dialed.insert("static-peer:4443".into(), placeholder_handle(), DialSource::Static);

		let long_ago = Instant::now() - Duration::from_secs(3600);
		dialed.mark_unannounced("static-peer:4443", long_ago);
		dialed.sweep_stale(Instant::now(), STALE_AFTER);

		assert!(dialed.contains("static-peer:4443"));
	}

	/// A gossip-discovered peer whose unannounce has stuck longer than the
	/// threshold gets aborted and removed.
	#[tokio::test]
	async fn sweep_evicts_stale_gossip_peer() {
		let dialed = DialMap::default();
		let now = Instant::now();
		dialed.insert("gone:4443".into(), placeholder_handle(), DialSource::Gossip);
		dialed.mark_unannounced("gone:4443", now - STALE_AFTER - Duration::from_secs(1));

		dialed.sweep_stale(now, STALE_AFTER);

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
		dialed.insert("flapping:4443".into(), placeholder_handle(), DialSource::Gossip);
		dialed.mark_unannounced("flapping:4443", now - Duration::from_millis(50));

		dialed.sweep_stale(now, STALE_AFTER);

		assert!(dialed.contains("flapping:4443"));
	}

	/// A peer that's currently announced (no pending unannounce) is never swept.
	#[tokio::test]
	async fn sweep_keeps_currently_announced_peer() {
		let dialed = DialMap::default();
		dialed.insert("healthy:4443".into(), placeholder_handle(), DialSource::Gossip);
		// No mark_unannounced -> stays announced.

		dialed.sweep_stale(Instant::now(), STALE_AFTER);

		assert!(dialed.contains("healthy:4443"));
	}

	/// A restart after an unannounce clears the pending-sweep timestamp, so
	/// the entry survives even if the original unannounce was old enough to
	/// otherwise trigger eviction.
	#[tokio::test]
	async fn reannounce_cancels_pending_sweep() {
		let dialed = DialMap::default();
		let now = Instant::now();
		dialed.insert("flap:4443".into(), placeholder_handle(), DialSource::Gossip);
		dialed.mark_unannounced("flap:4443", now - STALE_AFTER - Duration::from_secs(1));

		// Re-adding the gossip source (a reannounce) clears the pending sweep.
		assert!(
			dialed.add_source("flap:4443", DialSource::Gossip),
			"should report a cleared pending-sweep"
		);
		dialed.sweep_stale(now, STALE_AFTER);

		assert!(dialed.contains("flap:4443"));
		// A second reannounce has nothing to clear.
		assert!(!dialed.add_source("flap:4443", DialSource::Gossip));
	}

	/// A peer wanted by both gossip and the API survives losing either source: the
	/// dial is only torn down once the last source releases it.
	#[tokio::test]
	async fn multi_source_peer_survives_until_last_release() {
		let dialed = DialMap::default();
		let now = Instant::now();
		// Gossiped first, then also appears in the API list.
		dialed.insert("both:4443".into(), placeholder_handle(), DialSource::Gossip);
		let desired: HashSet<String> = ["both:4443".to_string()].into_iter().collect();
		assert!(
			dialed.reconcile_api(&desired).is_empty(),
			"already dialed; no new spawn"
		);

		// Dropped from the API list -> still wanted by gossip.
		assert!(dialed.reconcile_api(&HashSet::new()).is_empty());
		assert!(dialed.contains("both:4443"), "gossip still wants it");

		// Now gossip goes stale too -> the dial is finally released.
		dialed.mark_unannounced("both:4443", now - STALE_AFTER - Duration::from_secs(1));
		dialed.sweep_stale(now, STALE_AFTER);
		assert!(!dialed.contains("both:4443"));
	}

	/// `insert` for an already-dialed peer merges the source onto the existing
	/// entry (and aborts the redundant handle) rather than opening a second dial.
	#[tokio::test]
	async fn insert_merges_redundant_dial() {
		let dialed = DialMap::default();
		dialed.insert("p:4443".into(), placeholder_handle(), DialSource::Gossip);
		dialed.insert("p:4443".into(), placeholder_handle(), DialSource::Api);

		// Dropping the API source leaves the gossip source holding the dial.
		assert!(dialed.reconcile_api(&HashSet::new()).is_empty());
		assert!(dialed.contains("p:4443"), "gossip source still holds the dial");
	}

	/// `reconcile_api` drops API dials missing from the desired set, reports the
	/// newly desired ones for the caller to spawn, and never touches Static or
	/// Gossip dials (even when they're absent from the API list).
	#[tokio::test]
	async fn reconcile_api_adds_and_removes_only_api() {
		let dialed = DialMap::default();
		dialed.insert("static:4443".into(), placeholder_handle(), DialSource::Static);
		dialed.insert("gossip:4443".into(), placeholder_handle(), DialSource::Gossip);
		dialed.insert("api-keep:4443".into(), placeholder_handle(), DialSource::Api);
		dialed.insert("api-drop:4443".into(), placeholder_handle(), DialSource::Api);

		// Desired: keep one existing API peer, drop the other, add a new one.
		// Static/Gossip peers are not in the list but must survive.
		let desired: HashSet<String> = ["api-keep:4443".to_string(), "api-new:4443".to_string()]
			.into_iter()
			.collect();
		let mut to_add = dialed.reconcile_api(&desired);
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
		dialed.insert("shared:4443".into(), placeholder_handle(), DialSource::Static);

		let desired: HashSet<String> = ["shared:4443".to_string()].into_iter().collect();
		assert!(dialed.reconcile_api(&desired).is_empty());
		assert!(dialed.contains("shared:4443"));
	}

	/// The peer-list wire format is a bare JSON array of host strings.
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
		let err = Cluster::new(config).unwrap().run().await.expect_err("should error");
		let msg = format!("{err}");
		assert!(msg.contains("--cluster-node"), "missing --cluster-node in: {msg}");
		assert!(msg.contains("--cluster-mesh"), "missing --cluster-mesh in: {msg}");
	}

	/// A valid `cluster.id` is used verbatim as the relay's origin id, giving the
	/// node a stable identity across restarts.
	#[test]
	fn cluster_id_sets_origin() {
		let cluster = Cluster::new(ClusterConfig {
			id: Some(42),
			..Default::default()
		})
		.expect("valid id");
		assert_eq!(cluster.origin.id, 42);
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

		let cluster_run = cluster.clone();
		let mut handle = tokio::spawn(async move { cluster_run.run().await });

		// Give the runtime a moment to execute the synchronous setup work.
		tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

		// The self-registration broadcast must be visible on the origin.
		let (path, event) = watcher.try_next().expect("self-registration must be published");
		assert_eq!(path.as_str(), ".internal/origins/rendezvous.example.com:4443");
		assert!(event.broadcast().is_some());

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
	#[test]
	fn legacy_mesh_url_enables_gossip_as_node() {
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
			canonicalize_peer_key("https://host:4443/?jwt=abc"),
			placeholder_handle(),
			DialSource::Static,
		);
		assert!(dialed.contains(&canonicalize_peer_key("host:4443")));

		// Different ports stay distinct.
		assert_ne!(canonicalize_peer_key("host:4443"), canonicalize_peer_key("host:5555"));
	}

	/// A legacy mesh URL that disagrees with an explicit `--cluster-node` is a
	/// conflict, not a silent pick.
	#[test]
	fn legacy_mesh_url_conflicting_with_node_errors() {
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
	#[test]
	fn cluster_connect_api_survives_toml_merge() {
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
