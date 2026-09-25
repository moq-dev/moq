//! Archive endpoints: record one broadcast into an object store (export), or
//! republish a recording from one (import), through `moq-archive`.

use std::collections::HashSet;
use std::time::Duration;

use anyhow::Context;
use hang::moq_net;
use moq_archive::writer::{Control, Retention};
use moq_mux::catalog::{CatalogFormat, Stream as _};
use object_store::ObjectStore;
use url::Url;

use crate::moq::notify_ready;

/// How long expired objects outlive the timeline that dropped them, unless
/// `--retention-grace` says otherwise.
const GRACE: Duration = Duration::from_secs(30);

/// `export archive` args.
#[derive(usage::Args, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub struct ExportArgs {
	/// Where to record: `file:///dir`, `s3://bucket/prefix`, `gs://bucket/prefix`, or `az://container/prefix`.
	pub store: Url,

	/// Keep only this much recent content (a DVR), deleting older segments. Unset keeps everything.
	#[usage(long)]
	pub retention: Option<crate::duration::Duration>,

	/// How long expired objects outlive the timeline that dropped them, so readers can finish
	/// their downloads. Defaults to 30s; needs `--retention`.
	#[usage(long)]
	pub retention_grace: Option<crate::duration::Duration>,
}

/// `import archive` args.
#[derive(usage::Args, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub struct ImportArgs {
	/// The recording to replay, as `export archive` wrote it.
	pub store: Url,

	/// Keep following a recording that is still growing, checking for new segments at this
	/// interval. Unset replays what is stored now and ends the timeline there.
	#[usage(long)]
	pub follow: Option<crate::duration::Duration>,
}

/// Record the broadcast `name` into `args.store` until the broadcast ends.
///
/// Reads the broadcast's own catalog: video and audio renditions pace the segments, while the
/// catalog and every other track are recorded alongside without pacing.
pub async fn export(
	origin: moq_net::origin::Consumer,
	name: String,
	format: CatalogFormat,
	args: ExportArgs,
) -> anyhow::Result<()> {
	let retention = match (args.retention, args.retention_grace) {
		(Some(window), grace) => Some(Retention::new(
			window.into_std(),
			grace.map_or(GRACE, crate::duration::Duration::into_std),
		)),
		(None, Some(_)) => anyhow::bail!("`--retention-grace` needs `--retention`"),
		(None, None) => None,
	};
	let catalog_track = catalog_track(format)?;
	let store = open(&args.store)?;

	let broadcast = origin
		.routed_broadcast(&name)
		.await
		.with_context(|| format!("broadcast `{name}` is unavailable"))?;
	let config = moq_archive::writer::Config::default().with_retention(retention);
	let writer = moq_archive::Writer::new(store, broadcast.clone(), config)
		.await
		.with_context(|| format!("failed to start recording into {}", args.store))?;
	let control = writer.control();
	control.track(catalog_track).await?;
	let catalog = moq_mux::catalog::Consumer::<()>::new(&broadcast, format).await?;

	tracing::info!(%name, store = %args.store, "recording");
	notify_ready();

	// The writer decides when the recording is over; the catalog only feeds it tracks.
	tokio::select! {
		result = writer.run() => result.context("recording failed"),
		Err(err) = enroll(control, catalog) => Err(err),
	}
}

/// Republish the recording at `args.store` as the broadcast `name`.
///
/// The timeline replays as a live track and every other track's groups are served on request.
pub async fn import(origin: moq_net::origin::Producer, name: String, args: ImportArgs) -> anyhow::Result<()> {
	let store = open(&args.store)?;
	let broadcast = origin.create_broadcast(&name).context("failed to create broadcast")?;
	let config = moq_archive::reader::Config::new(hang::timeline::DEFAULT_NAME);
	let mut reader = moq_archive::Reader::open(store, &broadcast, config)
		.await
		.with_context(|| format!("no readable recording at {}", args.store))?;
	let serve = reader.serve();
	broadcast
		.announce(Default::default())
		.context("failed to announce broadcast")?;

	tracing::info!(%name, store = %args.store, "replaying");
	notify_ready();

	// The store holds no end marker, so only the caller can say the recording is complete.
	let Some(interval) = args.follow.map(crate::duration::Duration::into_std) else {
		reader.finish()?;
		serve.await;
		return Ok(());
	};
	let refresh = async {
		loop {
			tokio::time::sleep(interval).await;
			reader.refresh().await?;
		}
	};
	tokio::select! {
		() = serve => Ok(()),
		result = refresh => result,
	}
}

/// Open the object store `url` names, taking cloud credentials from the `AWS_*`, `GOOGLE_*`,
/// and `AZURE_*` environment variables.
fn open(url: &Url) -> anyhow::Result<moq_archive::Store<Box<dyn ObjectStore>>> {
	let credentials = std::env::vars_os()
		.filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
		.filter(|(key, _)| ["AWS_", "GOOGLE_", "AZURE_"].iter().any(|cloud| key.starts_with(cloud)));
	let (store, prefix) =
		object_store::parse_url_opts(url, credentials).with_context(|| format!("unsupported archive store {url}"))?;
	Ok(moq_archive::Store::new(store, prefix))
}

/// The track carrying the catalog `format` reads.
fn catalog_track(format: CatalogFormat) -> anyhow::Result<&'static str> {
	Ok(match format {
		CatalogFormat::Hang => hang::Catalog::DEFAULT_NAME,
		CatalogFormat::HangZ => hang::Catalog::COMPRESSED_NAME,
		CatalogFormat::Msf => moq_msf::DEFAULT_NAME,
		other => anyhow::bail!("`export archive` cannot record a {other:?} catalog"),
	})
}

/// Enroll each track as the catalog lists it, until the catalog ends.
async fn enroll<S: ObjectStore>(
	control: Control<S>,
	mut catalog: moq_mux::catalog::Consumer<()>,
) -> anyhow::Result<()> {
	let mut tracks = Tracks::default();
	while let Some(snapshot) = catalog.next().await? {
		for change in tracks.update(&snapshot)? {
			match change {
				Change::Pacing(name) => control.pacing_track(&name).await?,
				Change::Track(name) => control.track(&name).await?,
				Change::Remove(name) => control.remove(&name)?,
			}
		}
	}
	Ok(())
}

/// A change to the recorded track set.
#[derive(Debug, PartialEq, Eq)]
enum Change {
	/// Record a rendition that paces the segments.
	Pacing(String),
	/// Record a track without letting it pace the segments.
	Track(String),
	/// Stop recording a track the catalog dropped.
	Remove(String),
}

/// The catalog tracks being recorded, diffed against each new snapshot.
#[derive(Default)]
struct Tracks {
	/// Every name ever enrolled. The writer never takes a name back.
	enrolled: HashSet<String>,
	/// The names the latest snapshot listed.
	listed: HashSet<String>,
}

impl Tracks {
	/// The changes that bring the recording in line with `catalog`.
	///
	/// Refuses a track served from another broadcast, since an archive holds exactly one, and a
	/// track that returns after the catalog dropped it, since the writer cannot resume one.
	fn update(&mut self, catalog: &hang::Catalog) -> anyhow::Result<Vec<Change>> {
		let video = catalog
			.video
			.renditions
			.iter()
			.map(|(name, c)| (name, &c.broadcast, true));
		let audio = catalog
			.audio
			.renditions
			.iter()
			.map(|(name, c)| (name, &c.broadcast, true));
		let text = catalog
			.text
			.renditions
			.iter()
			.map(|(name, c)| (name, &c.broadcast, false));
		let json = catalog.json.tracks.iter().map(|(name, c)| (name, &c.broadcast, false));
		let binary = catalog
			.binary
			.tracks
			.iter()
			.map(|(name, c)| (name, &c.broadcast, false));

		let mut listed = HashSet::new();
		let mut changes = Vec::new();
		for (name, broadcast, pacing) in video.chain(audio).chain(text).chain(json).chain(binary) {
			if let Some(broadcast) = broadcast {
				anyhow::bail!(
					"track `{name}` is served from broadcast `{broadcast}`; an archive records one broadcast"
				);
			}
			if !listed.insert(name.clone()) || self.listed.contains(name) {
				continue;
			}
			anyhow::ensure!(
				self.enrolled.insert(name.clone()),
				"track `{name}` returned to the catalog after it was dropped; the recording cannot resume it"
			);
			changes.push(match pacing {
				true => Change::Pacing(name.clone()),
				false => Change::Track(name.clone()),
			});
		}

		let mut dropped: Vec<_> = self.listed.difference(&listed).cloned().collect();
		dropped.sort();
		changes.extend(dropped.into_iter().map(Change::Remove));
		self.listed = listed;
		Ok(changes)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	use hang::catalog::{AudioCodec, AudioConfig, JsonConfig, Mode};
	use moq_net::{Timescale, Timestamp, group, track};

	fn audio() -> AudioConfig {
		AudioConfig::new(AudioCodec::Opus, 48_000, 2)
	}

	/// Renditions pace, data tracks don't, and a later snapshot only adds what is new.
	#[test]
	fn renditions_pace_and_the_rest_follow() {
		let mut tracks = Tracks::default();
		let mut catalog = hang::Catalog::default();
		catalog.audio.renditions.insert("audio".into(), audio());
		catalog
			.json
			.tracks
			.insert("chat".into(), JsonConfig::new(Mode::Snapshot));

		assert_eq!(
			tracks.update(&catalog).unwrap(),
			[Change::Pacing("audio".into()), Change::Track("chat".into())]
		);

		catalog.audio.renditions.insert("audio2".into(), audio());
		assert_eq!(tracks.update(&catalog).unwrap(), [Change::Pacing("audio2".into())]);
		assert_eq!(tracks.update(&catalog).unwrap(), []);
	}

	/// A dropped rendition stops pacing, and coming back is refused rather than silently lost.
	#[test]
	fn a_dropped_track_is_removed_for_good() {
		let mut tracks = Tracks::default();
		let mut catalog = hang::Catalog::default();
		catalog.audio.renditions.insert("audio".into(), audio());
		tracks.update(&catalog).unwrap();

		let removed = catalog.audio.renditions.remove("audio").unwrap();
		assert_eq!(tracks.update(&catalog).unwrap(), [Change::Remove("audio".into())]);

		catalog.audio.renditions.insert("audio".into(), removed);
		let err = tracks.update(&catalog).unwrap_err().to_string();
		assert!(err.contains("cannot resume"), "{err}");
	}

	/// A rendition living in a sibling broadcast would be missing from the recording.
	#[test]
	fn another_broadcast_is_refused() {
		let mut rendition = audio();
		rendition.broadcast = Some(moq_net::path::RelativeOwned::new("./source"));
		let mut catalog = hang::Catalog::default();
		catalog.audio.renditions.insert("audio".into(), rendition);

		let err = Tracks::default().update(&catalog).unwrap_err().to_string();
		assert!(err.contains("broadcast `source`"), "{err}");
	}

	/// What `export archive` records, `import archive` serves back group for group, and the
	/// export ends cleanly with its broadcast.
	#[tokio::test]
	async fn a_recording_replays_its_groups() {
		// `open` reads the credential variables.
		let _env = crate::test_env::EnvGuard::clear(&[]);
		let dir = tempfile::tempdir().unwrap();
		let url = Url::from_directory_path(dir.path()).unwrap();

		let origin = moq_tokio::origin::spawn();
		let mut broadcast = origin.create_broadcast("live.hang").unwrap();
		let mut catalog = moq_mux::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
		let info = track::Info::default()
			.with_timescale(Timescale::MILLI)
			.with_max_age(Duration::from_secs(3600));
		let track = broadcast.create_track("audio", info).unwrap();
		catalog
			.mutate(|catalog| {
				catalog.audio.renditions.insert("audio".into(), audio());
			})
			.unwrap();
		broadcast.announce(Default::default()).unwrap();

		let args = ExportArgs {
			store: url.clone(),
			retention: None,
			retention_grace: None,
		};
		let recording = tokio::spawn(export(origin.consume(), "live.hang".into(), CatalogFormat::Hang, args));

		for sequence in 0..3 {
			let mut group = track.create_group(group::Info { sequence }).unwrap();
			for offset in [0, 500] {
				let timestamp = Timestamp::from_millis(sequence * 1000 + offset).unwrap();
				group.write_frame(timestamp, format!("{sequence}+{offset}")).unwrap();
			}
			group.finish().unwrap();
		}

		// Finish only once the rendition is enrolled, which writes its `.info`.
		tokio::time::timeout(Duration::from_secs(10), async {
			while !dir.path().join("audio/.info").exists() {
				tokio::time::sleep(Duration::from_millis(10)).await;
			}
		})
		.await
		.expect("the rendition is enrolled");
		track.finish().unwrap();
		catalog.finish().unwrap();
		broadcast.finish();

		tokio::time::timeout(Duration::from_secs(10), recording)
			.await
			.expect("the export ends with its broadcast")
			.unwrap()
			.expect("the recording succeeds");

		let replay = moq_tokio::origin::spawn();
		let args = ImportArgs {
			store: url,
			follow: None,
		};
		let serving = tokio::spawn(import(replay.clone(), "replay.hang".into(), args));

		let consumer = replay.consume().routed_broadcast("replay.hang").await.unwrap();
		let audio = consumer.track("audio").unwrap();
		for sequence in 0..3 {
			let mut group = audio.fetch_group(sequence, None).await.unwrap();
			for offset in [0, 500] {
				let frame = group.read_frame().await.unwrap().expect("a frame");
				assert_eq!(frame.timestamp.as_millis(), u128::from(sequence * 1000 + offset));
				assert_eq!(frame.payload, format!("{sequence}+{offset}"));
			}
			assert!(group.read_frame().await.unwrap().is_none());
		}

		serving.abort();
	}
}
