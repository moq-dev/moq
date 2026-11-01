//! HLS endpoints: pull a remote playlist into MoQ (import), or serve HLS and
//! DASH over HTTP from MoQ broadcasts (export), fetching media groups on demand.

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Context;
use axum::http::Method;
use futures::FutureExt;
use hang::moq_net;
use url::Url;

use crate::moq::{ImportTarget, notify_ready};

mod record;

/// Where `export hls` serves when neither `--listen` nor `--output` is given.
const LISTEN: SocketAddr = SocketAddr::new(std::net::IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED), 8089);

/// HLS import (pull a remote playlist) args.
#[derive(usage_rs::Args, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub struct ImportArgs {
	/// Playlist URL (http/https) or local file path.
	#[usage(value_hint = usage_rs::ValueHint::AnyPath, extensions("m3u8", "m3u"))]
	pub playlist: String,

	/// Publish each fMP4 fragment whole as CMAF instead of splitting it into one frame per
	/// sample. Keeps the source's exact sample timing, but every player has to parse ISO BMFF.
	#[usage(long = "cmaf")]
	pub passthrough: bool,
}

/// HLS export (serve over HTTP, record to a directory) args.
#[derive(usage_rs::Args, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub struct ExportArgs {
	/// HTTP listener for the HLS endpoints. Defaults to `[::]:8089` unless `--output` records
	/// without serving.
	#[usage(long, value_name = "ADDR")]
	pub listen: Option<SocketAddr>,

	/// Record the broadcast's playlists and segments into this directory, which must be empty
	/// or absent. Fails if the recording ends abruptly or misses a segment.
	#[usage(long, value_name = "DIR", value_hint = usage_rs::ValueHint::DirPath)]
	pub output: Option<PathBuf>,

	/// TLS certificates, keys, self-signed generation, and optional mTLS roots.
	#[usage(flatten)]
	pub tls: Tls,

	/// Minimum media listed in each rendition's playlist window, at most 256 segments.
	/// Keep it within the relay's group-cache retention, since segments are fetched from
	/// there on request.
	#[usage(long, default = "16s")]
	pub window: moq_tokio::cli::Duration,

	/// Browser CORS policy for the HLS listener.
	#[usage(flatten)]
	pub cors: crate::web::Cors,
}

/// TLS for the HLS HTTP listener, independent of the MoQ listener's environment.
#[derive(usage_rs::Args, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub struct Tls {
	/// Load the HLS listener's PEM certificate.
	#[usage(long = "listen-tls-cert", env = "MOQ_HLS_TLS_CERT", value_hint = usage_rs::ValueHint::FilePath)]
	pub cert: Vec<PathBuf>,
	/// Load the HLS listener's PEM private key.
	#[usage(long = "listen-tls-key", env = "MOQ_HLS_TLS_KEY", value_hint = usage_rs::ValueHint::FilePath)]
	pub key: Vec<PathBuf>,
	/// Generate the HLS listener's self-signed certificate for these hostnames.
	#[usage(long = "listen-tls-generate", env = "MOQ_HLS_TLS_GENERATE", delimiter = ',')]
	pub generate: Vec<String>,
	/// Trust these PEM roots for optional HLS client certificates.
	#[usage(long = "listen-tls-root", env = "MOQ_HLS_TLS_ROOT", delimiter = ',', value_hint = usage_rs::ValueHint::FilePath)]
	pub root: Vec<PathBuf>,
	#[usage(
		name = "hls-legacy-cert",
		long = "tls-cert",
		alias = "server-tls-cert",
		env = "MOQ_SERVER_TLS_CERT",
		hide = true
	)]
	legacy_cert: Vec<PathBuf>,

	#[usage(
		name = "hls-legacy-key",
		long = "tls-key",
		alias = "server-tls-key",
		env = "MOQ_SERVER_TLS_KEY",
		hide = true
	)]
	legacy_key: Vec<PathBuf>,

	#[usage(
		name = "hls-legacy-generate",
		long = "tls-generate",
		alias = "server-tls-generate",
		env = "MOQ_SERVER_TLS_GENERATE",
		delimiter = ',',
		hide = true
	)]
	legacy_generate: Vec<String>,

	#[usage(
		name = "hls-legacy-root",
		long = "server-tls-root",
		env = "MOQ_SERVER_TLS_ROOT",
		delimiter = ',',
		hide = true
	)]
	legacy_root: Vec<PathBuf>,
}

impl Tls {
	pub(super) fn deprecated(&self) -> moq_tokio::cli::Deprecated {
		let mut found = moq_tokio::cli::Deprecated::default();
		if !self.legacy_cert.is_empty() {
			found.flag(
				"--tls-cert",
				Some("MOQ_SERVER_TLS_CERT"),
				"--listen-tls-cert / MOQ_HLS_TLS_CERT",
			);
		}
		if !self.legacy_key.is_empty() {
			found.flag(
				"--tls-key",
				Some("MOQ_SERVER_TLS_KEY"),
				"--listen-tls-key / MOQ_HLS_TLS_KEY",
			);
		}
		if !self.legacy_generate.is_empty() {
			found.flag(
				"--tls-generate",
				Some("MOQ_SERVER_TLS_GENERATE"),
				"--listen-tls-generate / MOQ_HLS_TLS_GENERATE",
			);
		}
		if !self.legacy_root.is_empty() {
			found.flag(
				"--server-tls-root",
				Some("MOQ_SERVER_TLS_ROOT"),
				"--listen-tls-root / MOQ_HLS_TLS_ROOT",
			);
		}
		found
	}

	fn into_config(self) -> anyhow::Result<moq_tokio::tls::Listen> {
		let deprecated = self.deprecated();
		anyhow::ensure!(deprecated.is_empty(), "{deprecated}");
		let mut config = moq_tokio::tls::Listen::default();
		config.cert = self.cert;
		config.key = self.key;
		config.generate = self.generate;
		config.root = self.root;
		Ok(config)
	}
}

/// Pull a remote HLS/LL-HLS playlist (URL or file path) into the Origin under `target.name`,
/// announced under `epoch`. Once the playlist ends, serve the finished broadcast for the linger.
pub async fn import(target: ImportTarget, args: ImportArgs, epoch: moq_net::Epoch) -> anyhow::Result<()> {
	let ImportTarget {
		origin,
		name,
		max_age,
		linger,
		bandwidth,
	} = target;
	let producer = origin.create_broadcast(&name).context("failed to create broadcast")?;

	let config = moq_mux::catalog::Config::default()
		.with_max_age(max_age)
		.with_bandwidth(bandwidth);
	let catalog = moq_mux::catalog::Producer::new(&mut producer.broadcast().clone(), config)?;
	// Announced once the catalog first goes out, which waits for every rendition's init: a
	// relay queries a broadcast's tracks the moment it is announced, and keeps the refusals of a
	// publisher that cannot serve its declared tracks yet.
	let mut listed = catalog.consume()?;

	let mut config = moq_hls::import::Config::new(playlist_url(&args.playlist)?);
	config.passthrough = args.passthrough;
	let mut importer = moq_hls::import::Import::new(producer.broadcast().clone(), catalog, config)?;

	tracing::info!(%name, "importing HLS");

	let ran = match importer.init().await {
		Ok(()) => {
			notify_ready();
			run_announced(&mut importer, &mut listed, &producer, epoch).await
		}
		Err(err) => Err(err.into()),
	};
	if let Err(err) = ran {
		// Subscribers see why the import failed rather than a dropped producer.
		importer.abort(moq_net::Error::Transport(format!("{err:#}")));
		return Err(err);
	}

	// The playlist ended: serve the finished broadcast for the linger, so a subscriber that
	// arrives after the end still gets the tail. An interrupt drops it at once.
	importer.finish()?;
	tracing::info!(?linger, "playlist ended; serving the finished broadcast");
	moq_mux::Linger::new([producer.broadcast().clone()], importer, linger)
		.wait()
		.await;
	Ok(())
}

/// Run the import to the playlist's end, announcing the broadcast as its catalog first goes out.
///
/// Fails when the playlist ends without the catalog ever naming a track.
async fn run_announced(
	importer: &mut moq_hls::import::Import,
	listed: &mut moq_mux::catalog::hang::Consumer,
	publication: &moq_net::origin::Publication,
	epoch: moq_net::Epoch,
) -> anyhow::Result<()> {
	let run = importer.run();
	tokio::pin!(run);
	let mut ended = false;
	let catalog = tokio::select! {
		catalog = listed.next() => catalog?,
		res = &mut run => {
			res?;
			ended = true;
			// A finite playlist can publish its catalog and end in the same poll, so take what it
			// published rather than reading the end as empty.
			listed.next().now_or_never().transpose()?.flatten()
		}
	};
	anyhow::ensure!(catalog.is_some(), "the playlist ended before it declared a track");
	publication
		.announce(moq_net::origin::Route::default().with_epoch(epoch))
		.context("failed to announce broadcast")?;
	if !ended {
		run.await?;
	}
	Ok(())
}

fn playlist_url(playlist: &str) -> anyhow::Result<Url> {
	if playlist.starts_with("http://") || playlist.starts_with("https://") {
		return Url::parse(playlist).context("invalid HLS playlist URL");
	}

	let path = PathBuf::from(playlist);
	let absolute = if path.is_absolute() {
		path
	} else {
		std::env::current_dir()?.join(path)
	};
	Url::from_file_path(&absolute).map_err(|_| anyhow::anyhow!("invalid HLS playlist path: {}", absolute.display()))
}

/// Serve HLS and DASH over HTTP for the single broadcast `name` (reached at
/// `/<name>/master.m3u8` and `/<name>/manifest.mpd`); other broadcasts in the
/// Origin are not served.
pub async fn export(origin: moq_net::origin::Consumer, args: ExportArgs, name: String) -> anyhow::Result<()> {
	let scope = moq_net::Patterns::from(
		moq_net::Pattern::subtree(&name).with_context(|| format!("invalid broadcast name `{name}`"))?,
	);
	let scoped = origin
		.scope("", &scope)
		.with_context(|| format!("failed to scope origin to broadcast `{name}`"))?;

	let mut config = moq_hls::export::Config::default();
	config.window = args.window.into();

	let tls = args.tls.into_config()?;
	let listen = match (args.listen, &args.output) {
		(None, Some(_)) => None,
		(listen, _) => Some(listen.unwrap_or(LISTEN)),
	};
	let serving = match listen {
		Some(listen) => Some(serve(scoped.clone(), config.clone(), listen, tls, &args.cors)?),
		None => {
			anyhow::ensure!(
				tls.cert.is_empty() && tls.key.is_empty() && tls.generate.is_empty() && tls.root.is_empty(),
				"the HLS listener's TLS flags only apply with `export hls --listen`"
			);
			anyhow::ensure!(
				args.cors.origin.is_empty(),
				"`--cors-origin` only applies to `export hls --listen`"
			);
			None
		}
	};
	let recording = args.output.map(record::Recording::create).transpose()?;
	notify_ready();

	let recording = async {
		let Some(recording) = recording else {
			return Ok(());
		};
		// Wait for a covering route rather than asking on the spot: a recording started beside
		// its publisher, or over a session that discovers only watched paths, sees no route yet.
		let broadcast = scoped
			.follow(name.as_str(), None)?
			.available()
			.await
			.with_context(|| format!("failed to resolve broadcast `{name}`"))?;
		let source = moq_mux::Source::new(scoped.clone(), broadcast);
		let (broadcaster, driver) = moq_hls::export::Broadcaster::new(source, config);
		tokio::spawn(driver.run());
		tracing::info!(%name, "recording HLS");
		recording.run(&broadcaster).await
	};
	match serving {
		Some(serving) => tokio::try_join!(serving, recording).map(|_| ()),
		None => recording.await,
	}
}

/// Bind the HLS listener, returning the server to run.
fn serve(
	origin: moq_net::origin::Consumer,
	config: moq_hls::export::Config,
	listen: SocketAddr,
	tls: moq_tokio::tls::Listen,
	cors: &crate::web::Cors,
) -> anyhow::Result<impl Future<Output = anyhow::Result<()>> + use<>> {
	let server = moq_hls::Server::new(origin, config);
	let app = server.router().layer(cors.layer([Method::GET])?);
	let tls = if tls.cert.is_empty() && tls.generate.is_empty() {
		None
	} else {
		let alpn = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
		Some(tls.server_config(alpn)?)
	};
	let listener = moq_tokio::bind::tcp(listen)?;
	tracing::info!(%listen, "serving HLS");
	Ok(crate::web::serve(listener, app, tls))
}

#[cfg(test)]
mod test {
	use super::*;

	/// Write bbb's init and fragments to `media.mp4` in `dir`, and a `media.m3u8` listing the first
	/// `segments` of them, ended with `EXT-X-ENDLIST` or not. The media file is the same whatever
	/// `segments`, so rewriting the playlist models a live window that ends.
	fn write_playlist(dir: &std::path::Path, segments: usize, end_list: bool) {
		let data = include_bytes!("../../moq-mux/src/container/fmp4/test_data/bbb.mp4");
		let mut moofs = Vec::new();
		let mut position = 0usize;
		while position + 8 <= data.len() {
			let size = u32::from_be_bytes(data[position..position + 4].try_into().unwrap()) as usize;
			if size < 8 || position + size > data.len() {
				break;
			}
			if &data[position + 4..position + 8] == b"moof" {
				moofs.push(position);
			}
			position += size;
		}
		let init = moofs[0];
		let mut playlist = "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:2\n".to_string();
		if segments > 0 {
			playlist += &format!("#EXT-X-MAP:URI=\"media.mp4\",BYTERANGE=\"{init}@0\"\n");
		}
		for window in moofs.windows(2).take(segments) {
			playlist += &format!(
				"#EXTINF:1,\n#EXT-X-BYTERANGE:{}@{}\nmedia.mp4\n",
				window[1] - window[0],
				window[0]
			);
		}
		if end_list {
			playlist += "#EXT-X-ENDLIST\n";
		}

		std::fs::write(dir.join("media.mp4"), &data[..]).unwrap();
		std::fs::write(dir.join("media.m3u8"), playlist).unwrap();
	}

	/// An importer initialized on a live playlist of `segments`, which then ends without adding
	/// any, the catalog it lists, and its broadcast. Import refuses a playlist that had already
	/// ended when it joined, so a finite run joins live first.
	async fn joined_then_ended(
		segments: usize,
	) -> anyhow::Result<(
		tempfile::TempDir,
		moq_hls::import::Import,
		moq_mux::catalog::hang::Consumer,
		moq_net::origin::Publication,
	)> {
		let dir = tempfile::tempdir()?;
		write_playlist(dir.path(), segments, false);
		let playlist = Url::from_file_path(dir.path().join("media.m3u8")).unwrap();

		let (origin, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::default());
		tokio::spawn(moq_net::time::run(driver));
		let producer = origin.create_broadcast("hls")?;
		let catalog =
			moq_mux::catalog::Producer::new(&mut producer.broadcast().clone(), moq_mux::catalog::Config::default())?;
		let listed = catalog.consume()?;
		let mut importer = moq_hls::import::Import::new(
			producer.broadcast().clone(),
			catalog,
			moq_hls::import::Config::new(playlist),
		)?;
		importer.init().await?;

		write_playlist(dir.path(), segments, true);
		Ok((dir, importer, listed, producer))
	}

	/// A finite playlist can publish its catalog and end in one poll, which the race between
	/// the run and the catalog sees either way round; both must announce. `select!` picks its
	/// first branch at random, so this repeats.
	#[tokio::test]
	async fn an_ended_playlist_announces_its_catalog() {
		for _ in 0..32 {
			let (_dir, mut importer, mut listed, producer) = joined_then_ended(1).await.unwrap();
			run_announced(&mut importer, &mut listed, &producer, moq_net::Epoch::mint())
				.await
				.unwrap();
			assert!(producer.route().is_some(), "the finished playlist was not announced");
		}
	}

	/// What `run_announced` relies on when the run wins: a run that ended has already published
	/// the catalog it declared, so the listing is ready without waiting.
	#[tokio::test]
	async fn a_finished_run_has_published_its_catalog() {
		let (_dir, mut importer, mut listed, _producer) = joined_then_ended(1).await.unwrap();
		importer.run().await.unwrap();
		let catalog = listed.next().now_or_never().expect("published before the run ended");
		assert!(catalog.unwrap().is_some());
	}

	/// A playlist that ends having declared nothing still fails, and announces nothing.
	#[tokio::test]
	async fn an_empty_playlist_fails_unannounced() {
		let (_dir, mut importer, mut listed, producer) = joined_then_ended(0).await.unwrap();
		let result = run_announced(&mut importer, &mut listed, &producer, moq_net::Epoch::mint()).await;
		assert!(producer.route().is_none(), "an empty playlist was announced");
		let err = result.expect_err("an empty playlist imports nothing");
		assert!(format!("{err:#}").contains("before it declared a track"), "{err:#}");
	}

	/// A recording started before its broadcast is announced waits for it instead of failing.
	#[tokio::test(start_paused = true)]
	async fn a_recording_waits_for_its_broadcast() {
		let dir = tempfile::tempdir().unwrap();
		let args = ExportArgs {
			listen: None,
			output: Some(dir.path().join("recording")),
			tls: Tls {
				cert: Vec::new(),
				key: Vec::new(),
				generate: Vec::new(),
				root: Vec::new(),
				legacy_cert: Vec::new(),
				legacy_key: Vec::new(),
				legacy_generate: Vec::new(),
				legacy_root: Vec::new(),
			},
			window: std::time::Duration::from_secs(16).into(),
			cors: Default::default(),
		};
		let origin = moq_tokio::origin::spawn();
		let exporting = export(origin.consume(), args, "live".to_string());
		let result = kio::clock::timeout(std::time::Duration::from_secs(1), exporting).await;
		assert!(result.is_err(), "the recording gave up: {result:?}");
	}

	/// The next group a subscriber reads after the import stopped: the error it ended with.
	async fn ended_with(track: &mut moq_net::track::Subscriber) -> moq_net::Error {
		loop {
			match track.recv_group().await {
				Ok(Some(_)) => continue,
				Ok(None) => panic!("the track finished cleanly"),
				Err(err) => return err,
			}
		}
	}

	/// An announced import that fails, here on a timestamp reset, aborts its tracks and catalog
	/// with the cause, so subscribers see the failure rather than a dropped producer.
	#[tokio::test]
	async fn a_failed_import_aborts_its_tracks() {
		use moq_mux::container::{Frame, fmp4::Muxer};

		let dir = tempfile::tempdir().unwrap();
		let muxer = Muxer::audio(&hang::catalog::AudioConfig::new(
			hang::catalog::AudioCodec::Opus,
			48_000,
			2,
		))
		.unwrap();
		std::fs::write(dir.path().join("init.mp4"), muxer.init().unwrap().unwrap()).unwrap();
		for (index, micros) in [5_000_000, 0].into_iter().enumerate() {
			let frame = Frame {
				timestamp: moq_net::Timestamp::from_micros(micros).unwrap(),
				duration: Some(moq_net::Timestamp::from_micros(20_000).unwrap()),
				payload: bytes::Bytes::from_static(&[0x78, 0x00, 0x00, 0x00]),
				keyframe: true,
			};
			let fragment = muxer.fragment(index as u32, &[frame], None).unwrap();
			std::fs::write(dir.path().join(format!("{index}.m4s")), fragment).unwrap();
		}
		let live = "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:1,\n0.m4s\n";
		let playlist = dir.path().join("media.m3u8");
		std::fs::write(&playlist, live).unwrap();

		let origin = moq_tokio::origin::spawn();
		let consumer = origin.consume();
		let mut announced = consumer.announced();
		let target = ImportTarget {
			origin,
			name: "hls".to_string(),
			max_age: None,
			linger: moq_mux::Linger::DEFAULT,
			bandwidth: Default::default(),
		};
		let args = ImportArgs {
			playlist: playlist.to_str().unwrap().to_string(),
			passthrough: false,
		};
		let importing = tokio::spawn(import(target, args, moq_net::Epoch::mint()));

		announced.next().await.expect("announced");
		let broadcast = consumer.request("hls", None).await.unwrap();
		let mut catalogs = hang::catalog::Catalog::<()>::subscribe(&broadcast).await.unwrap();
		let catalog = catalogs.next().await.unwrap().expect("a catalog");
		let name = catalog.audio.renditions.keys().next().expect("an audio rendition");
		let mut media = broadcast.track(name).unwrap().subscribe(None).await.unwrap();
		let mut listing = broadcast.track("catalog.json").unwrap().subscribe(None).await.unwrap();

		// The source restarts at zero under the same init segment.
		std::fs::write(&playlist, format!("{live}#EXT-X-DISCONTINUITY\n#EXTINF:1,\n1.m4s\n")).unwrap();
		let err = importing.await.unwrap().expect_err("a reset fails the import");
		assert!(format!("{err:#}").contains("rewound"), "{err:#}");

		for track in [&mut media, &mut listing] {
			let ended = ended_with(track).await;
			assert!(matches!(ended, moq_net::Error::Transport(_)), "{ended:?}");
		}
	}
}
