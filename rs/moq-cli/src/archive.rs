//! Archive endpoints: record every broadcast announced under a prefix into an object store
//! (export), or republish one recorded broadcast from one (import), through `moq-archive`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use hang::moq_net;
use object_store::ObjectStore;
use object_store::path::{Path, PathPart};
use url::Url;

use crate::moq::notify_ready;

/// How long a superseded object outlives its DVR window, unless `--retention-grace` says otherwise.
const GRACE: Duration = Duration::from_secs(30);

/// `export archive` args.
#[derive(usage_rs::Args, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub struct ExportArgs {
	/// Where to record: `file:///dir`, `s3://bucket/prefix`, `gs://bucket/prefix`, or `az://container/prefix`.
	/// Each broadcast records under `<broadcast>/@<epoch>` beneath it.
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
	/// One recorded broadcast: the `<broadcast>/@<epoch>` key prefix `export archive` wrote.
	pub store: Url,

	/// How often a recording still growing is checked for new objects and tracks. Defaults to 1s.
	#[usage(long)]
	pub refresh: Option<moq_tokio::cli::Duration>,
}

/// One publisher instance being recorded: its path, and its epoch when its route has one.
type Instance = (moq_net::Path, Option<moq_net::Epoch>);

/// Record every broadcast announced at or under `prefix` into `args.store`, each publisher
/// instance under `<path>/@<epoch>`, until the announcements end.
///
/// A route without an epoch names no instance, so each one it announces records under an epoch
/// minted here: a restarted export never resumes it, and never stitches two instances together.
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
	let hidden = prefix.split('/').any(|segment| segment.starts_with('.'));
	let prefix = moq_net::Path::new(&prefix)?;
	let mut announced = origin.clone().with_hidden(hidden).announced();
	let mut recordings = tokio::task::JoinSet::new();
	let mut running = Running::default();
	let claims = Claims::default();

	tracing::info!(%prefix, store = %args.store, "recording");
	notify_ready();

	loop {
		let instance = tokio::select! {
			event = announced.next() => {
				let announce = match event.context("announcements ended")? {
					// Another publisher instance, or the same one announced again.
					moq_net::announce::Event::Start(announce) | moq_net::announce::Event::Restart(announce) | moq_net::announce::Event::Takeover { announce, .. } => announce,
					_ => continue,
				};
				if !announce.prefix.has_prefix(&prefix) {
					continue;
				}
				let instance: Instance = (announce.prefix, announce.route.epoch);
				if !running.start(&instance) {
					continue;
				}
				instance
			}
			Some(done) = recordings.join_next() => {
				let (instance, result) = done.context("recording panicked")?;
				result?;
				if !running.done(&instance) {
					continue;
				}
				instance
			}
		};
		let key = key(&root, &instance)?;
		let store = moq_archive::Store::new(store.clone(), key);
		// Requested now, so an announcement resolves the instance serving it when read, not
		// whichever one serves by the time its task runs.
		let request = origin.request(&instance.0, instance.1.clone());
		recordings.spawn(record(request, instance, store, config.clone(), claims.clone()));
	}
}

/// The broadcasts being recorded. Announcements read after another instance took over all
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

/// The epoch-pinned instances recording, so each records under its key once at a time, and again
/// when it was announced anew meanwhile.
#[derive(Default)]
struct Running(HashMap<Instance, bool>);

impl Running {
	/// Whether to record `instance` now that it was announced. An epochless one is a new instance
	/// each time, under a fresh key.
	fn start(&mut self, instance: &Instance) -> bool {
		if instance.1.is_none() {
			return true;
		}
		match self.0.get_mut(instance) {
			Some(again) => {
				*again = true;
				false
			}
			None => {
				self.0.insert(instance.clone(), false);
				true
			}
		}
	}

	/// Whether to record `instance` again, now that its recording ended.
	fn done(&mut self, instance: &Instance) -> bool {
		match self.0.get_mut(instance) {
			Some(again) if *again => {
				*again = false;
				true
			}
			_ => {
				self.0.remove(instance);
				false
			}
		}
	}
}

/// Record one publisher instance until its broadcast ends, unless another recording has it.
async fn record(
	request: impl Future<Output = Result<moq_net::broadcast::Consumer, moq_net::Error>>,
	instance: Instance,
	store: moq_archive::Store<Arc<dyn ObjectStore>>,
	config: moq_archive::producer::Config,
	claims: Claims,
) -> (Instance, anyhow::Result<()>) {
	let result = async {
		let path = &instance.0;
		let broadcast = request
			.await
			.with_context(|| format!("broadcast `{path}` is unavailable"))?;
		let Some(_claim) = claims.claim(path, &broadcast) else {
			return Ok(());
		};
		tracing::info!(%path, key = %store.prefix(), "recording broadcast");
		moq_archive::Producer::new(store, broadcast, config)
			.run()
			.await
			.with_context(|| format!("recording `{path}` failed"))
	}
	.await;
	(instance, result)
}

/// The key prefix one instance records under: its path's segments, then `@<epoch>`.
///
/// A path segment's key never starts with `@`, since a leading one is percent-encoded like `%`
/// always is, so no instance's prefix lies inside another's, whatever paths a publisher picks.
fn key(root: &Path, (path, epoch): &Instance) -> anyhow::Result<Path> {
	let epoch = epoch.clone().unwrap_or_else(moq_net::Epoch::mint);
	let mut key = root.clone();
	for segment in path.as_str().split('/').filter(|segment| !segment.is_empty()) {
		let part = PathPart::from(segment);
		key = match part.as_ref().strip_prefix('@') {
			Some(rest) => key.join(PathPart::parse(&format!("%40{rest}"))?),
			None => key.join(part),
		};
	}
	let epoch = format!("@{epoch}");
	Ok(key.join(PathPart::parse(&epoch).context("an epoch is one key segment")?))
}

/// Republish the recording at `args.store` as the broadcast `name`, announced under `epoch`.
///
/// Every stored track is served with its original groups: each live from its newest stored
/// group, and history on request. A recording still growing is followed until its producer
/// marks it complete.
pub async fn import(
	origin: moq_net::origin::Producer,
	name: String,
	args: ImportArgs,
	epoch: moq_net::Epoch,
) -> anyhow::Result<()> {
	let store = open(&args.store)?;
	let mut config = moq_archive::consumer::Config::default();
	if let Some(refresh) = args.refresh {
		config = config.with_refresh(refresh.into());
	}
	let broadcast = origin.create_broadcast(&name).context("failed to create broadcast")?;
	let consumer = moq_archive::Consumer::open(store, broadcast.broadcast().clone(), config)
		.await
		.with_context(|| format!("no readable recording at {}", args.store))?;
	broadcast
		.announce(moq_net::origin::Route::default().with_epoch(epoch))
		.context("failed to announce broadcast")?;

	tracing::info!(%name, store = %args.store, "replaying");
	notify_ready();
	consumer
		.run()
		.await
		.with_context(|| format!("replaying {} failed", args.store))
}

/// Open the object store `url` names, taking cloud credentials from the `AWS_*`, `GOOGLE_*`,
/// and `AZURE_*` environment variables, with the prefix the URL names.
fn open_store(url: &Url) -> anyhow::Result<(Arc<dyn ObjectStore>, Path)> {
	let credentials = std::env::vars_os()
		.filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
		.filter(|(key, _)| ["AWS_", "GOOGLE_", "AZURE_"].iter().any(|cloud| key.starts_with(cloud)));
	let (store, prefix) =
		object_store::parse_url_opts(url, credentials).with_context(|| format!("unsupported archive store {url}"))?;
	Ok((Arc::from(store), prefix))
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
	/// `<path>/@<epoch>`, and leaves out what is announced elsewhere.
	#[tokio::test]
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
		let mut epochs = Vec::new();
		let mut broadcasts = Vec::new();
		for path in ["live/a", "live/b", "other"] {
			let broadcast = origin.create_broadcast(path).unwrap();
			let track = broadcast.create_track("video", info.clone()).unwrap();
			let mut group = track.create_group(group::Info { sequence: 0 }).unwrap();
			group.write_frame(Timestamp::from_millis(0).unwrap(), path).unwrap();
			group.finish().unwrap();
			let epoch = moq_net::Epoch::mint();
			broadcast
				.announce(moq_net::origin::Route::default().with_epoch(epoch.clone()))
				.unwrap();
			epochs.push(epoch);
			broadcasts.push((broadcast, track));
		}

		// Each recording ends with its broadcast, once it subscribed.
		for (broadcast, mut track) in broadcasts.drain(..2) {
			moq_clock::timeout(Duration::from_secs(10), track.subscription_changed())
				.await
				.expect("the export subscribes")
				.unwrap();
			track.finish().unwrap();
			broadcast.close();
		}
		let complete = [
			format!("live/a/@{}/.end", epochs[0]),
			format!("live/b/@{}/.end", epochs[1]),
		];
		moq_clock::timeout(Duration::from_secs(10), async {
			while !complete.iter().all(|end| dir.path().join(end).exists()) {
				moq_clock::sleep(Duration::from_millis(10)).await;
			}
		})
		.await
		.expect("both recordings complete");
		recording.abort();

		let files = files(dir.path());
		for (path, epoch) in [("live/a", &epochs[0]), ("live/b", &epochs[1])] {
			for file in [".end", "video/.info", "video/end"] {
				assert!(files.contains(&format!("{path}/@{epoch}/{file}")), "{files:?}");
			}
			assert!(
				files.contains(&format!(
					"{path}/@{epoch}/video/0000000000000000000-0000000000000000000-0000000000000000000"
				)),
				"{files:?}"
			);
		}
		assert!(!files.iter().any(|file| file.starts_with("other")), "{files:?}");
	}

	/// `import archive` republishes a recording `export archive` wrote under the given name and
	/// epoch, serving its stored groups.
	#[tokio::test]
	async fn an_exported_broadcast_imports() {
		let dir = tempfile::tempdir().unwrap();
		let url = Url::from_directory_path(dir.path()).unwrap();

		let source = moq_net::broadcast::Info::new().produce();
		let info = track::Info::default().with_timescale(Timescale::MILLI);
		let video = source.create_track("video", info).unwrap();
		for sequence in 0..2 {
			let mut group = video.create_group(group::Info { sequence }).unwrap();
			let pts = Timestamp::from_millis(sequence * 1000).unwrap();
			group.write_frame(pts, format!("frame {sequence}")).unwrap();
			group.finish().unwrap();
		}
		video.finish().unwrap();
		let recording =
			tokio::spawn(moq_archive::Producer::new(open(&url).unwrap(), source.consume(), Default::default()).run());
		// Its objects, then the markers once the broadcast ends.
		moq_clock::timeout(Duration::from_secs(10), async {
			while files(dir.path()).len() < 2 {
				moq_clock::sleep(Duration::from_millis(10)).await;
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
		let epoch = moq_net::Epoch::mint();
		let _import = tokio::spawn(import(origin.clone(), "replay".into(), args, epoch.clone()));
		let mut announced = origin.consume().announced();
		moq_clock::timeout(Duration::from_secs(10), async {
			loop {
				if let Some(moq_net::announce::Event::Start(announce)) = announced.next().await {
					assert_eq!(announce.prefix.as_str(), "replay");
					assert_eq!(announce.route.epoch, Some(epoch.clone()));
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

	/// An epochless route announced again while its last recording drains is a new recording, and
	/// an epoch-pinned one records again once its recording ends.
	#[test]
	fn a_re_announced_instance_is_recorded() {
		let mut running = Running::default();
		let epochless = (moq_net::Path::from_static("live/a"), None);
		assert!(running.start(&epochless));
		assert!(running.start(&epochless));
		assert!(!running.done(&epochless));

		let pinned = (moq_net::Path::from_static("live/b"), Some(moq_net::Epoch::mint()));
		assert!(running.start(&pinned));
		assert!(!running.start(&pinned));
		assert!(running.done(&pinned));
		assert!(!running.done(&pinned));
		assert!(running.start(&pinned));
	}

	/// Announcements that resolve the same broadcast record it once at a time, while another
	/// instance at the path records beside it.
	#[tokio::test]
	async fn a_resolved_broadcast_is_recorded_once() {
		let origin = moq_tokio::origin::spawn();
		let path = moq_net::Path::from_static("live/a");
		let newest = origin.create_broadcast(&path).unwrap();
		let older = origin.create_broadcast(&path).unwrap();

		let claims = Claims::default();
		let claim = claims.claim(&path, &newest.consume()).expect("first claim");
		assert!(claims.claim(&path, &newest.consume()).is_none());
		let beside = claims.claim(&path, &older.consume()).expect("another instance");

		drop(claim);
		assert!(claims.claim(&path, &newest.consume()).is_some());
		drop(beside);
		assert!(claims.0.lock().unwrap().is_empty());
	}

	/// A route without an epoch records under one minted per instance, so two instances never
	/// share a key prefix.
	#[test]
	fn an_epochless_instance_gets_a_fresh_key() {
		let instance = (moq_net::Path::from_static("live/a"), None);
		let first = key(&Path::from("rec"), &instance).unwrap();
		let second = key(&Path::from("rec"), &instance).unwrap();
		assert_ne!(first, second);
		assert!(first.as_ref().starts_with("rec/live/a/"), "{first}");

		let epoch = moq_net::Epoch::mint();
		let pinned = (moq_net::Path::from_static("live/a"), Some(epoch.clone()));
		assert_eq!(
			key(&Path::from("rec"), &pinned).unwrap().as_ref(),
			format!("rec/live/a/@{epoch}")
		);
	}

	/// A path segment's key never starts with `@`, so a path that spells out another instance's
	/// key prefix records beside it, never inside it.
	#[test]
	fn keys_are_prefix_free() {
		let epoch = moq_net::Epoch::mint();
		let victim = key(
			&Path::from("rec"),
			&(moq_net::Path::from_static("live/a"), Some(epoch.clone())),
		)
		.unwrap();
		let hostile = format!("live/a/@{epoch}");
		let nested = key(
			&Path::from("rec"),
			&(moq_net::Path::new(&hostile).unwrap(), Some(moq_net::Epoch::mint())),
		)
		.unwrap();
		assert!(nested.prefix_match(&victim).is_none(), "{nested} is under {victim}");
		assert!(
			nested.as_ref().starts_with(&format!("rec/live/a/%40{epoch}/@")),
			"{nested}"
		);

		// `%` is escaped too, so an escaped-looking segment keys apart from the `@` one.
		let literal = key(
			&Path::from("rec"),
			&(moq_net::Path::new(&format!("live/a/%40{epoch}")).unwrap(), Some(epoch)),
		)
		.unwrap();
		assert!(literal.as_ref().starts_with("rec/live/a/%2540"), "{literal}");
	}
}
