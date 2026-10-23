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

/// HLS import (pull a remote playlist) args.
#[derive(usage_rs::Args, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub struct ImportArgs {
	/// Playlist URL (http/https) or local file path.
	#[usage(value_hint = usage_rs::ValueHint::AnyPath, extensions("m3u8", "m3u"))]
	pub playlist: String,

	/// Publish each fMP4 fragment whole as CMAF instead of splitting it into one frame per
	/// sample. Keeps the source's exact sample timing, but every player has to parse ISO BMFF.
	#[usage(long)]
	pub passthrough: bool,
}

/// HLS export (serve over HTTP) args.
#[derive(usage_rs::Args, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub struct ExportArgs {
	/// HTTP listener for the HLS endpoints.
	#[usage(long, default = "[::]:8089")]
	pub listen: SocketAddr,

	/// TLS certificates, keys, self-signed generation, and optional mTLS roots.
	#[usage(flatten)]
	pub tls: moq_tokio::tls::Listen,

	/// Minimum media listed in each rendition's playlist window, at most 256 segments.
	/// Keep it within the relay's group-cache retention, since segments are fetched from
	/// there on request.
	#[usage(long, default = "16s")]
	pub window: crate::duration::Duration,

	/// Browser CORS policy for the HLS listener.
	#[usage(flatten)]
	pub cors: crate::web::Cors,
}

/// Pull a remote HLS/LL-HLS playlist (URL or file path) into the Origin under `target.name`,
/// announced under `epoch`. Once the playlist ends, serve the finished broadcast until its
/// media is stale.
pub async fn import(target: ImportTarget, args: ImportArgs, epoch: moq_net::Epoch) -> anyhow::Result<()> {
	let ImportTarget {
		origin,
		name,
		max_age,
		bandwidth,
	} = target;
	let mut producer = origin.create_broadcast(&name).context("failed to create broadcast")?;

	let config = moq_mux::catalog::Config::default()
		.with_max_age(max_age)
		.with_bandwidth(bandwidth);
	let catalog = moq_mux::catalog::Producer::new(&mut producer, config)?;
	// Announced once the catalog first goes out, which waits for every rendition's init: a
	// relay queries a broadcast's tracks the moment it is announced, and keeps the refusals of a
	// publisher that cannot serve its declared tracks yet.
	let mut listed = catalog.consume()?;

	let mut config = moq_hls::import::Config::new(playlist_url(&args.playlist)?);
	config.passthrough = args.passthrough;
	let mut importer = moq_hls::import::Import::new(producer.clone(), catalog, config)?;

	tracing::info!(%name, "importing HLS");

	importer.init().await?;
	notify_ready();
	run_announced(&mut importer, &mut listed, &mut producer, epoch).await?;

	// The playlist ended: serve the finished broadcast until its media is stale, so
	// subscribers still catching up drain the tail. An interrupt drops it at once.
	let finish = importer.finish()?;
	tracing::info!(linger = ?finish.linger(), "playlist ended; serving the finished broadcast until its media is stale");
	finish.wait().await;
	Ok(())
}

/// Run the import to the playlist's end, announcing the broadcast as its catalog first goes out.
///
/// Fails when the playlist ends without the catalog ever naming a track.
async fn run_announced(
	importer: &mut moq_hls::import::Import,
	listed: &mut moq_mux::catalog::hang::Consumer,
	producer: &mut moq_net::broadcast::Producer,
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
	producer
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
	config.window = args.window.into_std();
	let server = moq_hls::Server::new(scoped, config);
	let app = server.router().layer(args.cors.layer([Method::GET])?);

	let tls = if args.tls.cert.is_empty() && args.tls.generate.is_empty() {
		None
	} else {
		let alpn = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
		Some(args.tls.server_config(alpn)?)
	};

	let listener = moq_tokio::bind::tcp(args.listen)?;

	tracing::info!(listen = %args.listen, "serving HLS");
	notify_ready();

	crate::web::serve(listener, app, tls).await
}

#[cfg(test)]
mod test {
	use super::*;

	/// bbb's init and its first `segments` fragments in `media.mp4`, and a `media.m3u8` listing
	/// them with `EXT-X-ENDLIST`, or listing nothing for no segments.
	fn ended_playlist(segments: usize) -> (tempfile::TempDir, Url) {
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
		playlist += "#EXT-X-ENDLIST\n";

		let dir = tempfile::tempdir().unwrap();
		std::fs::write(dir.path().join("media.mp4"), &data[..]).unwrap();
		std::fs::write(dir.path().join("media.m3u8"), playlist).unwrap();
		let url = Url::from_file_path(dir.path().join("media.m3u8")).unwrap();
		(dir, url)
	}

	/// An initialized importer of `playlist`, the catalog it lists, and its broadcast.
	async fn importer(
		playlist: Url,
	) -> anyhow::Result<(
		moq_hls::import::Import,
		moq_mux::catalog::hang::Consumer,
		moq_net::broadcast::Producer,
	)> {
		let (origin, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::default());
		tokio::spawn(moq_net::time::run(driver));
		let mut producer = origin.create_broadcast("hls")?;
		let catalog = moq_mux::catalog::Producer::new(&mut producer, moq_mux::catalog::Config::default())?;
		let listed = catalog.consume()?;
		let mut importer =
			moq_hls::import::Import::new(producer.clone(), catalog, moq_hls::import::Config::new(playlist))?;
		importer.init().await?;
		Ok((importer, listed, producer))
	}

	/// A finite playlist can publish its catalog and end in one poll, which the race between
	/// the run and the catalog sees either way round; both must announce. `select!` picks its
	/// first branch at random, so this repeats.
	#[tokio::test]
	async fn an_ended_playlist_announces_its_catalog() {
		let (_dir, playlist) = ended_playlist(1);
		for _ in 0..32 {
			let (mut importer, mut listed, mut producer) = importer(playlist.clone()).await.unwrap();
			run_announced(&mut importer, &mut listed, &mut producer, moq_net::Epoch::mint())
				.await
				.unwrap();
			assert!(producer.route().is_some(), "the finished playlist was not announced");
		}
	}

	/// What `run_announced` relies on when the run wins: a run that ended has already published
	/// the catalog it declared, so the listing is ready without waiting.
	#[tokio::test]
	async fn a_finished_run_has_published_its_catalog() {
		let (_dir, playlist) = ended_playlist(1);
		let (mut importer, mut listed, _producer) = importer(playlist).await.unwrap();
		importer.run().await.unwrap();
		let catalog = listed.next().now_or_never().expect("published before the run ended");
		assert!(catalog.unwrap().is_some());
	}

	/// A playlist that ends having declared nothing still fails, and announces nothing.
	#[tokio::test]
	async fn an_empty_playlist_fails_unannounced() {
		let (_dir, playlist) = ended_playlist(0);
		let result = match importer(playlist).await {
			Ok((mut importer, mut listed, mut producer)) => {
				let result = run_announced(&mut importer, &mut listed, &mut producer, moq_net::Epoch::mint()).await;
				assert!(producer.route().is_none(), "an empty playlist was announced");
				result
			}
			Err(err) => Err(err),
		};
		let err = result.expect_err("an empty playlist imports nothing");
		assert!(format!("{err:#}").contains("before it declared a track"), "{err:#}");
	}
}
