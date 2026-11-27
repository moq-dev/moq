//! Archive endpoints: record every broadcast announced under a prefix into an object store
//! (export), or republish one recorded broadcast from one (import), through `moq-archive`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use moq_tokio::RedactedUrl;
use object_store::path::{Path, PathPart};
use object_store::{ObjectStore, ObjectStoreScheme};
use url::Url;

use crate::moq::notify_ready;

/// How long a superseded object outlives its DVR window, unless `--retention-grace` says otherwise.
const GRACE: Duration = Duration::from_secs(30);

/// `export archive` args.
#[derive(usage_rs::Args, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub struct ExportArgs {
	/// Where to record: `file:///dir`, `s3://bucket/prefix`, `gs://bucket/prefix`, or `az://container/prefix`.
	/// Each broadcast records under `<broadcast>/@<token>` beneath it, `<token>` hashing its identity.
	pub store: Url,

	/// Keep only this much recent content (a DVR), deleting older objects. Unset keeps everything.
	#[usage(long)]
	pub retention: Option<moq_tokio::cli::Duration>,

	/// How long a deleted object outlives the retention, so readers can finish their downloads.
	/// Defaults to 30s; needs `--retention`.
	#[usage(long)]
	pub retention_grace: Option<moq_tokio::cli::Duration>,
}

/// `import archive` args.
#[derive(usage_rs::Args, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub struct ImportArgs {
	/// One recorded broadcast: the `<broadcast>/@<token>` key prefix `export archive` wrote.
	pub store: Url,

	/// How often a recording still growing is checked for new objects and tracks. Defaults to 1s.
	#[usage(long)]
	pub refresh: Option<moq_tokio::cli::Duration>,
}

/// One stream being recorded: its path, and its route's identity.
type Identity = (moq_net::Path, moq_net::Identity);

/// Record every broadcast announced at or under `prefix` into `args.store`, each stream under
/// `<path>/@<token>`, until the announcements end.
///
/// A broadcast that fails to record, or whose path cannot be followed, is logged and dropped:
/// every other broadcast keeps recording, whatever one publisher does. Only the origin
/// failing, or a task panicking, ends the export.
///
/// A wire that carries no identity has its relay mint one per announcement, so each one records
/// under a key of its own: a restarted export never stitches two streams together.
pub async fn export(origin: moq_net::origin::Consumer, prefix: String, args: ExportArgs) -> anyhow::Result<()> {
	let dvr = match (args.retention, args.retention_grace) {
		(Some(window), grace) => Some(moq_archive::producer::Dvr::new(
			window.into(),
			grace.map_or(GRACE, Into::into),
		)),
		(None, Some(_)) => anyhow::bail!("`--retention-grace` needs `--retention`"),
		(None, None) => None,
	};
	let config = moq_archive::producer::Config::default().with_dvr(dvr);
	let (store, root) = open_store(&args.store)?;

	// Announcements leave hidden routes out unless asked, and a prefix naming one asks.
	let prefix = moq_net::Path::new(&prefix)?;
	let hidden = prefix.parts().any(|segment| segment.starts_with('.'));
	let mut announced = origin.clone().with_hidden(hidden).announced();
	// A follower per listed path, reporting each identity that serves it, stopped with the export.
	let mut following = tokio::task::JoinSet::new();
	let mut followers: HashMap<moq_net::Path, tokio::task::AbortHandle> = HashMap::new();
	let (served, mut identities) = tokio::sync::mpsc::unbounded_channel::<Identity>();
	let mut recordings = tokio::task::JoinSet::new();
	let mut running = Running::default();
	let claims = Claims::default();

	tracing::info!(%prefix, store = %RedactedUrl::new(&args.store), "recording");
	notify_ready();

	loop {
		let identity = tokio::select! {
			event = announced.next() => {
				match event.context("announcements ended")? {
					moq_net::announce::Event::Present(announce) if announce.prefix.has_prefix(&prefix) => {
						let path = announce.prefix;
						let follower = following.spawn(follow(origin.clone(), path.clone(), served.clone()));
						if let Some(old) = followers.insert(path, follower) {
							old.abort();
						}
					}
					moq_net::announce::Event::Absent(announce) => {
						if let Some(follower) = followers.remove(&announce.prefix) {
							follower.abort();
						}
					}
					_ => {}
				}
				continue;
			}
			Some(followed) = following.join_next() => {
				match followed {
					Ok(Ok(())) => {}
					// One path cannot be followed, which ends only its own recordings.
					Ok(Err(err)) => tracing::warn!("not recording: {err:#}"),
					// Aborted once its path went absent.
					Err(err) if err.is_cancelled() => {}
					Err(err) => return Err(err).context("follower panicked"),
				}
				continue;
			}
			Some(identity) = identities.recv() => {
				if !running.start(&identity) {
					continue;
				}
				identity
			}
			Some(done) = recordings.join_next() => {
				let (identity, result) = done.context("recording panicked")?;
				// Recorded again if announced anew meanwhile, which fails or not on its own.
				if let Err(err) = result {
					tracing::warn!("recording ended: {err:#}");
				}
				if !running.done(&identity) {
					continue;
				}
				identity
			}
		};
		let key = key(&root, &identity)?;
		let store = moq_archive::Store::new(store.clone(), key);
		// Requested now, so an announcement resolves the identity serving it when read, not
		// whichever one serves by the time its task runs.
		let request = origin.request(&identity.0, Some(identity.1.clone()));
		recordings.spawn(record(request, identity, store, config.clone(), claims.clone()));
	}
}

/// Report each identity serving `path` as it comes online or takes over. An identity comes
/// again after the path went offline, since a return is announced anew.
///
/// Fails when the path cannot be followed or a handler refuses it, as a recording would;
/// the export drops the path then, as it drops a failed recording.
async fn follow(
	origin: moq_net::origin::Consumer,
	path: moq_net::Path,
	served: tokio::sync::mpsc::UnboundedSender<Identity>,
) -> anyhow::Result<()> {
	let unavailable = || format!("broadcast `{path}` is unavailable");
	let mut following = origin.follow(&path, None).with_context(unavailable)?;
	loop {
		let change = match following.available().await {
			Ok(change) => change,
			// The origin closed, which ends the export through its announcements.
			Err(moq_net::Error::Closed) => return Ok(()),
			Err(err) => return Err(err).with_context(unavailable),
		};
		let Some(identity) = change.and_then(|broadcast| broadcast.info().identity.clone()) else {
			continue;
		};
		if served.send((path.clone(), identity)).is_err() {
			return Ok(());
		}
	}
}

/// The broadcasts being recorded. Announcements read after another identity took over all
/// resolve that one, so it is recorded once.
#[derive(Clone, Default)]
struct Claims(Arc<std::sync::Mutex<HashMap<moq_net::Path, Vec<moq_net::broadcast::Consumer>>>>);

impl Claims {
	/// Claim `broadcast` for recording, or `None` when it is recorded already.
	fn claim(&self, path: &moq_net::Path, broadcast: &moq_net::broadcast::Consumer) -> Option<Claim> {
		let mut claims = self.0.lock().unwrap();
		let claimed = claims.entry(path.clone()).or_default();
		if claimed.iter().any(|other| other.is_clone(broadcast)) {
			return None;
		}
		claimed.push(broadcast.clone());
		Some(Claim {
			claims: self.clone(),
			path: path.clone(),
			broadcast: broadcast.clone(),
		})
	}
}

/// A broadcast being recorded, released on drop.
struct Claim {
	claims: Claims,
	path: moq_net::Path,
	broadcast: moq_net::broadcast::Consumer,
}

impl Drop for Claim {
	fn drop(&mut self) {
		let mut claims = self.claims.0.lock().unwrap();
		if let Some(claimed) = claims.get_mut(&self.path) {
			claimed.retain(|other| !other.is_clone(&self.broadcast));
			if claimed.is_empty() {
				claims.remove(&self.path);
			}
		}
	}
}

/// The streams recording, so each records under its key once at a time, and again when it was
/// announced anew meanwhile.
#[derive(Default)]
struct Running(HashMap<Identity, bool>);

impl Running {
	/// Whether to record `identity` now that it was announced.
	fn start(&mut self, identity: &Identity) -> bool {
		match self.0.get_mut(identity) {
			Some(again) => {
				*again = true;
				false
			}
			None => {
				self.0.insert(identity.clone(), false);
				true
			}
		}
	}

	/// Whether to record `identity` again, now that its recording ended.
	fn done(&mut self, identity: &Identity) -> bool {
		match self.0.get_mut(identity) {
			Some(again) if *again => {
				*again = false;
				true
			}
			_ => {
				self.0.remove(identity);
				false
			}
		}
	}
}

/// Record one publisher identity until its broadcast ends, unless another recording has it.
async fn record(
	request: impl Future<Output = Result<moq_net::broadcast::Consumer, moq_net::Error>>,
	identity: Identity,
	store: moq_archive::Store<Arc<dyn ObjectStore>>,
	config: moq_archive::producer::Config,
	claims: Claims,
) -> (Identity, anyhow::Result<()>) {
	let result = async {
		let path = &identity.0;
		let broadcast = request
			.await
			.with_context(|| format!("broadcast `{path}` is unavailable"))?;
		let Some(_claim) = claims.claim(path, &broadcast) else {
			return Ok(());
		};
		tracing::info!(%path, identity = %identity.1, key = %store.prefix(), "recording broadcast");
		moq_archive::Producer::new(store, broadcast, config)
			.run()
			.await
			.with_context(|| format!("recording `{path}` failed"))
	}
	.await;
	(identity, result)
}

/// The key prefix one stream records under: its path's segments, then `@<token>`, the
/// [`token`](moq_hls::export::token) its HLS URLs carry. Hashing the identity with its minted flag
/// keys a minted identity apart from a named one of the same text, and keeps the segment short
/// for an identity of up to 256 bytes, which percent-encoded could pass a filesystem's name limit.
///
/// Each key segment is a path segment's content, percent-encoded, so one holding a `/` stays
/// one segment. A path segment's key never starts with `@`, since a leading one is
/// percent-encoded like `%` always is, so no stream's prefix lies inside another's, whatever
/// paths a publisher picks.
fn key(root: &Path, (path, identity): &Identity) -> anyhow::Result<Path> {
	let mut key = root.clone();
	for segment in path.parts() {
		let segment = moq_net::path::unescape(segment);
		let part = PathPart::from(segment.as_ref());
		key = match part.as_ref().strip_prefix('@') {
			Some(rest) => key.join(PathPart::parse(&format!("%40{rest}"))?),
			None => key.join(part),
		};
	}
	Ok(key.join(PathPart::from(
		format!("@{}", moq_hls::export::token(identity)).as_str(),
	)))
}

/// Republish the recording at `args.store` as the broadcast `name`, announced under `identity`.
///
/// Every stored track is served with its original groups: each live from its newest stored
/// group, and history on request. A recording still growing is followed until its producer
/// marks it complete.
pub async fn import(
	origin: moq_net::origin::Producer,
	name: String,
	args: ImportArgs,
	identity: moq_net::Identity,
) -> anyhow::Result<()> {
	let store = open(&args.store)?;
	let url = RedactedUrl::new(&args.store);
	let mut config = moq_archive::consumer::Config::default();
	if let Some(refresh) = args.refresh {
		config = config.with_refresh(refresh.into());
	}
	// Hidden until the recording opens, then held while the replay runs.
	let route = origin
		.route(
			&name,
			moq_net::origin::RouteOptions::hidden().with_identity(identity, 0),
		)
		.context("failed to create broadcast")?;
	let consumer = moq_archive::Consumer::open(store, route.broadcast().clone(), config)
		.await
		.with_context(|| format!("no readable recording at {url}"))?;
	route.announce().context("failed to announce broadcast")?;

	tracing::info!(%name, store = %url, "replaying");
	notify_ready();
	consumer.run().await.with_context(|| format!("replaying {url} failed"))
}

/// Open the object store `url` names, taking cloud credentials from the `AWS_*`, `GOOGLE_*`,
/// and `AZURE_*` environment variables, with the prefix the URL names.
///
/// A password, query, or fragment is refused: no store reads one, and object_store echoes the
/// URL it was given in its errors, so refusing them keeps credentials out of every message.
fn open_store(url: &Url) -> anyhow::Result<(Arc<dyn ObjectStore>, Path)> {
	let redacted = RedactedUrl::new(url).to_string();
	anyhow::ensure!(
		url.password().is_none() && url.query().is_none() && url.fragment().is_none(),
		"archive store {redacted} has a password, query, or fragment, which no store reads; \
		 pass credentials through the environment instead",
	);
	// object_store quotes the URL it was given, userinfo included, so its message is redacted too.
	let unsupported = |err: anyhow::Error| {
		let reason = format!("{err:#}").replace(url.as_str(), &redacted);
		anyhow::anyhow!(reason).context(format!("unsupported archive store {redacted}"))
	};
	let (scheme, prefix) = ObjectStoreScheme::parse(url).map_err(|err| unsupported(err.into()))?;
	// Each cloud builder signs through rind, which `parse_url_opts` has no way to pass.
	let store: Arc<dyn ObjectStore> = match scheme {
		ObjectStoreScheme::Local => Arc::new(object_store::local::LocalFileSystem::new()),
		#[cfg(feature = "s3")]
		ObjectStoreScheme::AmazonS3 => Arc::new(
			object_store::aws::AmazonS3Builder::from_env()
				.with_url(url.as_str())
				.with_crypto_provider(rind_object_store::default_provider()?)
				.build()
				.map_err(|err| unsupported(err.into()))?,
		),
		#[cfg(feature = "gcs")]
		ObjectStoreScheme::GoogleCloudStorage => Arc::new(
			object_store::gcp::GoogleCloudStorageBuilder::from_env()
				.with_url(url.as_str())
				.with_crypto_provider(rind_object_store::default_provider()?)
				.build()
				.map_err(|err| unsupported(err.into()))?,
		),
		#[cfg(feature = "azure")]
		ObjectStoreScheme::MicrosoftAzure => Arc::new(
			object_store::azure::MicrosoftAzureBuilder::from_env()
				.with_url(url.as_str())
				.with_crypto_provider(rind_object_store::default_provider()?)
				.build()
				.map_err(|err| unsupported(err.into()))?,
		),
		_ => anyhow::bail!("unsupported archive store {redacted}"),
	};
	Ok((store, prefix))
}

/// [`open_store`] as one recorded broadcast.
fn open(url: &Url) -> anyhow::Result<moq_archive::Store<Arc<dyn ObjectStore>>> {
	let (store, prefix) = open_store(url)?;
	Ok(moq_archive::Store::new(store, prefix))
}

#[cfg(test)]
mod tests {
	use super::*;

	use moq_net::{Timescale, Timestamp, group, track};

	/// Every file under `dir`, relative to it.
	fn files(dir: &std::path::Path) -> Vec<String> {
		let mut files = Vec::new();
		let mut pending = vec![dir.to_path_buf()];
		while let Some(next) = pending.pop() {
			for entry in std::fs::read_dir(next).unwrap() {
				let path = entry.unwrap().path();
				if path.is_dir() {
					pending.push(path);
				} else {
					files.push(path.strip_prefix(dir).unwrap().to_string_lossy().into_owned());
				}
			}
		}
		files.sort();
		files
	}

	/// `export archive` records each broadcast announced under its prefix into its own
	/// `<path>/@<token>`, and leaves out what is announced elsewhere.
	#[tokio::test(start_paused = true)]
	async fn each_broadcast_records_under_its_own_key_prefix() {
		let dir = tempfile::tempdir().unwrap();
		let url = Url::from_directory_path(dir.path()).unwrap();

		let origin = moq_tokio::origin::spawn();
		let args = ExportArgs {
			store: url,
			retention: None,
			retention_grace: None,
		};
		let recording = tokio::spawn(export(origin.consume(), "live".into(), args));

		let info = track::Info::default()
			.with_timescale(Timescale::MILLI)
			.with_max_age(Duration::from_secs(3600));
		let mut identities = Vec::new();
		let mut broadcasts = Vec::new();
		for path in ["live/a", "live/b", "other"] {
			let broadcast = origin.route(path, moq_net::origin::RouteOptions::hidden()).unwrap();
			let track = broadcast.broadcast().create_track("video", info.clone()).unwrap();
			let mut group = track.create_group(group::Info { start: 0 }).unwrap();
			group.write_frame(Timestamp::from_millis(0).unwrap(), path).unwrap();
			group.finish().unwrap();
			broadcast.announce().unwrap();
			identities.push(broadcast.identity());
			broadcasts.push((broadcast, track));
		}

		// Each recording ends with its broadcast, once it subscribed.
		for (broadcast, mut track) in broadcasts.drain(..2) {
			kio::clock::timeout(Duration::from_secs(10), track.subscription_changed())
				.await
				.expect("the export subscribes")
				.unwrap();
			let _track = track.finish().unwrap();
			broadcast.broadcast().close();
		}
		let complete = [
			format!("live/a/@{}/.end", moq_hls::export::token(&identities[0])),
			format!("live/b/@{}/.end", moq_hls::export::token(&identities[1])),
		];
		kio::clock::timeout(Duration::from_secs(10), async {
			while !complete.iter().all(|end| dir.path().join(end).exists()) {
				kio::clock::sleep(Duration::from_millis(10)).await;
			}
		})
		.await
		.expect("both recordings complete");
		recording.abort();

		let files = files(dir.path());
		for (path, identity) in [("live/a", &identities[0]), ("live/b", &identities[1])] {
			let identity = moq_hls::export::token(identity);
			for file in [".end", "video/.info", "video/end"] {
				assert!(files.contains(&format!("{path}/@{identity}/{file}")), "{files:?}");
			}
			assert!(
				files.contains(&format!(
					"{path}/@{identity}/video/0000000000000000000-0000000000000000000"
				)),
				"{files:?}"
			);
		}
		assert!(!files.iter().any(|file| file.starts_with("other")), "{files:?}");
	}

	/// Publish `path` with one group and announce it, hidden like a broadcast a relay serves.
	fn publish(
		origin: &moq_net::origin::Producer,
		path: &str,
		timescale: Timescale,
	) -> (moq_net::origin::Route, moq_net::track::Producer) {
		let info = track::Info::default()
			.with_timescale(timescale)
			.with_max_age(Duration::from_secs(3600));
		let broadcast = origin.route(path, moq_net::origin::RouteOptions::hidden()).unwrap();
		let track = broadcast.broadcast().create_track("video", info).unwrap();
		let mut group = track.create_group(group::Info { start: 0 }).unwrap();
		group.write_frame(Timestamp::from_millis(0).unwrap(), path).unwrap();
		group.finish().unwrap();
		broadcast.announce().unwrap();
		(broadcast, track)
	}

	/// Wait for `file` under `dir`, which the export writes once it subscribed and the broadcast ended.
	async fn complete(dir: &std::path::Path, file: String) {
		kio::clock::timeout(Duration::from_secs(10), async {
			while !dir.join(&file).exists() {
				kio::clock::sleep(Duration::from_millis(10)).await;
			}
		})
		.await
		.unwrap_or_else(|_| panic!("{file} is written"));
	}

	/// End `broadcast` once the export subscribed to its track.
	async fn finish((broadcast, mut track): (moq_net::origin::Route, moq_net::track::Producer)) {
		kio::clock::timeout(Duration::from_secs(10), track.subscription_changed())
			.await
			.expect("the export subscribes")
			.unwrap();
		let _track = track.finish().unwrap();
		broadcast.broadcast().close();
	}

	/// A recording that fails, here a track re-added with another timescale than its stored
	/// one, is dropped alone: the other broadcast records and the export keeps running.
	#[tokio::test(start_paused = true)]
	async fn a_failed_recording_leaves_the_others_recording() {
		let dir = tempfile::tempdir().unwrap();
		let args = ExportArgs {
			store: Url::from_directory_path(dir.path()).unwrap(),
			retention: None,
			retention_grace: None,
		};
		let origin = moq_tokio::origin::spawn();
		let recording = tokio::spawn(export(origin.consume(), "live".into(), args));

		// `live/a` already holds a recording whose track has another timescale.
		let bad = publish(&origin, "live/a", Timescale::MILLI);
		let stored = dir
			.path()
			.join(format!("live/a/@{}/video", moq_hls::export::token(&bad.0.identity())));
		std::fs::create_dir_all(&stored).unwrap();
		std::fs::write(
			stored.join(".info"),
			moq_archive::Info::new(0, Some(90_000)).unwrap().encode().unwrap(),
		)
		.unwrap();
		let good = publish(&origin, "live/b", Timescale::MILLI);
		let good_end = format!("live/b/@{}/.end", moq_hls::export::token(&good.0.identity()));

		finish(good).await;
		complete(dir.path(), good_end).await;
		assert!(!recording.is_finished(), "one broadcast's failure ended the export");

		// A broadcast announced after the failure records too.
		let later = publish(&origin, "live/c", Timescale::MILLI);
		let later_end = format!("live/c/@{}/.end", moq_hls::export::token(&later.0.identity()));
		finish(later).await;
		complete(dir.path(), later_end).await;
		assert!(!recording.is_finished());
		recording.abort();
		drop(bad);
	}

	/// A handler refusing a listed path drops that path, not the export.
	#[tokio::test(start_paused = true)]
	async fn a_refused_path_leaves_the_others_recording() {
		let dir = tempfile::tempdir().unwrap();
		let args = ExportArgs {
			store: Url::from_directory_path(dir.path()).unwrap(),
			retention: None,
			retention_grace: None,
		};
		let origin = moq_tokio::origin::spawn();
		let claim = origin
			.claim("live/a", moq_net::origin::RouteOptions::default())
			.unwrap();
		let recording = tokio::spawn(export(origin.consume(), "live".into(), args));

		let request = kio::clock::timeout(Duration::from_secs(10), claim.requested())
			.await
			.expect("the export follows the listed path")
			.unwrap();
		request.reject(moq_net::Error::NotFound);

		let good = publish(&origin, "live/b", Timescale::MILLI);
		let good_end = format!("live/b/@{}/.end", moq_hls::export::token(&good.0.identity()));
		finish(good).await;
		complete(dir.path(), good_end).await;
		assert!(!recording.is_finished(), "a refused path ended the export");
		recording.abort();
	}

	/// A track aborted with an error code, then its route dropped, ends only its own recording.
	/// The archive records the abort as an end and returns `Ok`, so this does not reach the
	/// failure isolation the tests above pin.
	#[tokio::test(start_paused = true)]
	async fn an_aborted_track_leaves_the_others_recording() {
		let dir = tempfile::tempdir().unwrap();
		let args = ExportArgs {
			store: Url::from_directory_path(dir.path()).unwrap(),
			retention: None,
			retention_grace: None,
		};
		let origin = moq_tokio::origin::spawn();
		let recording = tokio::spawn(export(origin.consume(), "live".into(), args));

		let (gone, mut track) = publish(&origin, "live/a", Timescale::MILLI);
		kio::clock::timeout(Duration::from_secs(10), track.subscription_changed())
			.await
			.expect("the export subscribes")
			.unwrap();
		track.abort(moq_net::Error::Cancel).unwrap();
		drop(gone);
		kio::clock::sleep(Duration::from_secs(1)).await;
		assert!(!recording.is_finished(), "a dropped publisher ended the export");

		let good = publish(&origin, "live/b", Timescale::MILLI);
		let good_end = format!("live/b/@{}/.end", moq_hls::export::token(&good.0.identity()));
		finish(good).await;
		complete(dir.path(), good_end).await;
		assert!(!recording.is_finished());
		recording.abort();
	}

	/// `import archive` republishes a recording `export archive` wrote under the given name and
	/// identity, serving its stored groups.
	#[tokio::test(start_paused = true)]
	async fn an_exported_broadcast_imports() {
		let dir = tempfile::tempdir().unwrap();
		let url = Url::from_directory_path(dir.path()).unwrap();

		let source = moq_net::broadcast::Info::new().produce();
		// Both groups are written before the recorder reads them, so the superseded one must
		// stay fresh.
		let info = track::Info::default()
			.with_timescale(Timescale::MILLI)
			.with_max_age(None);
		let video = source.create_track("video", info).unwrap();
		for sequence in 0..2 {
			let mut group = video.create_group(group::Info { start: sequence * 1000 }).unwrap();
			let pts = Timestamp::from_millis(sequence * 1000).unwrap();
			group.write_frame(pts, format!("frame {sequence}")).unwrap();
			group.finish().unwrap();
		}
		let _video = video.finish().unwrap();
		let recording =
			tokio::spawn(moq_archive::Producer::new(open(&url).unwrap(), source.consume(), Default::default()).run());
		// Its objects, then the markers once the broadcast ends.
		kio::clock::timeout(Duration::from_secs(10), async {
			while files(dir.path()).len() < 2 {
				kio::clock::sleep(Duration::from_millis(10)).await;
			}
		})
		.await
		.expect("the recording stores its track");
		source.close();
		recording.await.unwrap().unwrap();

		let origin = moq_tokio::origin::spawn();
		let args = ImportArgs {
			store: url,
			refresh: None,
		};
		let identity = moq_net::Identity::random();
		let _import = tokio::spawn(import(origin.clone(), "replay".into(), args, identity.clone()));
		let mut announced = origin.consume().announced();
		kio::clock::timeout(Duration::from_secs(10), async {
			loop {
				if let Some(moq_net::announce::Event::Present(announce)) = announced.next().await {
					assert_eq!(announce.prefix.as_str(), "replay");
					assert_eq!(announce.route.identity(), &identity);
					break;
				}
			}
		})
		.await
		.expect("the replay is announced");
		let broadcast = origin.consume().request("replay", None).await.unwrap();
		let track = broadcast.track("video").unwrap();
		let mut group = track.fetch_group(0, None).await.unwrap();
		let frame = group.read_frame().await.unwrap().unwrap();
		assert_eq!(&frame.payload[..], b"frame 0");
	}

	/// A stream announced again while its last recording drains records again once that
	/// recording ends.
	#[test]
	fn a_re_announced_identity_is_recorded() {
		let mut running = Running::default();
		let pinned = (moq_net::Path::from_static("live/b"), moq_net::Identity::random());
		assert!(running.start(&pinned));
		assert!(!running.start(&pinned));
		assert!(running.done(&pinned));
		assert!(!running.done(&pinned));
		assert!(running.start(&pinned));
	}

	/// Announcements that resolve the same broadcast record it once at a time, while another
	/// identity at the path records beside it.
	#[tokio::test]
	async fn a_resolved_broadcast_is_recorded_once() {
		let origin = moq_tokio::origin::spawn();
		let path = moq_net::Path::from_static("live/a");
		let newest = origin.route(&path, moq_net::origin::RouteOptions::hidden()).unwrap();
		let older = origin.route(&path, moq_net::origin::RouteOptions::hidden()).unwrap();

		let claims = Claims::default();
		let claim = claims.claim(&path, &newest.broadcast().consume()).expect("first claim");
		assert!(claims.claim(&path, &newest.broadcast().consume()).is_none());
		let beside = claims
			.claim(&path, &older.broadcast().consume())
			.expect("another identity");

		drop(claim);
		assert!(claims.claim(&path, &newest.broadcast().consume()).is_some());
		drop(beside);
		assert!(claims.0.lock().unwrap().is_empty());
	}

	/// A stream records under its identity's token: one short hex segment whatever the identity
	/// holds, even 256 bytes that percent-encoded would pass a filesystem's name limit.
	#[test]
	fn an_identity_keys_one_segment() {
		let identity = moq_net::Identity::random();
		let pinned = (moq_net::Path::from_static("live/a"), identity.clone());
		assert_eq!(
			key(&Path::from("rec"), &pinned).unwrap().as_ref(),
			format!("rec/live/a/@{}", moq_hls::export::token(&identity))
		);
		for identity in ["a/b?%23#", "音声\"\n&", "~0123", &"é".repeat(128)] {
			let pinned = (moq_net::Path::from_static("live/a"), identity.parse().unwrap());
			let key = key(&Path::from("rec"), &pinned).unwrap();
			assert_eq!(key.parts().count(), 4);
			let part = key.parts().next_back().unwrap();
			assert_eq!(part.as_ref(), format!("@{}", moq_hls::export::token(&pinned.1)));
			assert_eq!(part.as_ref().len(), 17);

			// The store key is itself URL-encoded when naming the recording to import.
			for base in ["file:///", "s3://bucket/", "gs://bucket/", "az://container/"] {
				let mut url = Url::parse(base).unwrap();
				url.path_segments_mut()
					.unwrap()
					.extend(key.parts().map(|part| part.as_ref().to_owned()));
				assert_eq!(url.query(), None);
				assert_eq!(url.fragment(), None);
				let (_, reopened) = ObjectStoreScheme::parse(&url).unwrap();
				assert_eq!(reopened, key);
			}
		}
	}

	/// A path segment holding a `/` or `\` keys as one segment of its content.
	#[test]
	fn an_escaped_segment_keys_one_segment() {
		let identity = moq_net::Identity::random();
		let pinned = (moq_net::Path::from_static(r"live/a\/b\\c"), identity.clone());
		let key = key(&Path::from("rec"), &pinned).unwrap();
		assert_eq!(
			key.as_ref(),
			format!("rec/live/a%2Fb%5Cc/@{}", moq_hls::export::token(&identity))
		);
		assert_eq!(key.parts().count(), 4);
	}

	/// A path segment's key never starts with `@`, so a path that spells out another stream's
	/// key prefix records beside it, never inside it.
	#[test]
	fn keys_are_prefix_free() {
		let identity = moq_net::Identity::random();
		let token = moq_hls::export::token(&identity);
		let victim = key(
			&Path::from("rec"),
			&(moq_net::Path::from_static("live/a"), identity.clone()),
		)
		.unwrap();
		let hostile = format!("live/a/@{token}");
		let nested = key(
			&Path::from("rec"),
			&(moq_net::Path::new(&hostile).unwrap(), moq_net::Identity::random()),
		)
		.unwrap();
		assert!(nested.prefix_match(&victim).is_none(), "{nested} is under {victim}");
		assert!(
			nested.as_ref().starts_with(&format!("rec/live/a/%40{token}/@")),
			"{nested}"
		);

		// `%` is escaped too, so an escaped-looking segment keys apart from the `@` one.
		let literal = key(
			&Path::from("rec"),
			&(moq_net::Path::new(&format!("live/a/%40{token}")).unwrap(), identity),
		)
		.unwrap();
		assert!(literal.as_ref().starts_with("rec/live/a/%2540"), "{literal}");
	}

	/// A store URL that fails to open is reported without its userinfo or query, whichever
	/// layer refuses it.
	#[test]
	fn a_refused_store_hides_its_credentials() {
		let mut urls = vec![
			// Refused up front: no store reads a password, query, or fragment.
			"s3://user:SECRET@bucket/prefix",
			"file:///rec?token=SECRET",
			"file:///rec#SECRET",
			// Refused by object_store, which quotes the URL it was given.
			"ftp://SECRET@example.com/rec",
			// Recognised, but not a store an archive opens.
			"https://SECRET@example.com/rec",
		];
		if cfg!(feature = "azure") {
			urls.push("az://SECRET@account.example.com/rec");
		}
		for url in urls {
			let url = Url::parse(url).unwrap();
			let Err(err) = open(&url) else {
				panic!("{url} opened");
			};
			let err = format!("{err:#} {err:?}");
			assert!(!err.contains("SECRET"), "{err}");
			assert!(err.contains(&RedactedUrl::new(&url).to_string()), "{err}");
		}
	}
}
