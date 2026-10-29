//! Audio interop subscriber: decode a broadcast's audio with moq-audio for tone.py to judge.
//!
//! Waits for a catalog with an enabled audio rendition, then decodes the first AUDIO_MS of every
//! enabled rendition at its codec's own rate and layout. Each writes PREFIX.CODEC.txt
//! (`<rate> <channels>`, then `<timestamp us> <samples>` per frame) and PREFIX.CODEC.pcm (f32le
//! interleaved), CODEC being its catalog codec up to the first `.` (`opus`, `mp4a`). The CLI's
//! `export` covers the video side.
//!
//!     interop-audio URL BROADCAST PREFIX AUDIO_MS [VERSION]
use std::io::Write;
use std::time::Duration;

use anyhow::{Context, ensure};
use hang::catalog::AudioConfig;
use moq_mux::catalog::{CatalogFormat, Stream};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	let args: Vec<_> = std::env::args().collect();
	ensure!(
		args.len() == 5 || args.len() == 6,
		"usage: interop-audio URL BROADCAST PREFIX AUDIO_MS [VERSION]"
	);
	let url: url::Url = args[1].parse()?;
	let prefix = &args[3];
	let audio_ms: u64 = args[4].parse()?;

	let mut config = moq_tokio::connect::Config::default();
	// An empty version offers the defaults.
	if let Some(version) = args.get(5).filter(|version| !version.is_empty()) {
		config.version.push(version.parse().map_err(anyhow::Error::msg)?);
	}
	config.websocket.enabled = Some(false);
	let client = config.init(Default::default())?.with_reconnect(false);
	let origin = moq_tokio::origin::spawn();
	let consumer = origin.consume();
	let session = client
		.clone()
		.with_subscriber(origin)
		.connect(url)
		.established()
		.await?;

	let broadcast = consumer.follow(&args[2], None)?.available().await?;
	let source = moq_mux::Source::new(consumer, broadcast.clone());
	let mut catalogs = moq_mux::catalog::Consumer::<()>::new(&broadcast, CatalogFormat::Hang).await?;
	// A lazy publisher (the browser) may add audio in a later catalog update.
	let renditions: Vec<_> = loop {
		let catalog = catalogs.next().await?.context("catalog ended without audio")?;
		let renditions: Vec<_> = catalog
			.audio
			.enabled()
			.map(|(name, audio)| (name.clone(), audio.clone()))
			.collect();
		if !renditions.is_empty() {
			break renditions;
		}
	};
	futures::future::try_join_all(
		renditions
			.iter()
			.map(|(name, audio)| decode(&source, name, audio, prefix, audio_ms)),
	)
	.await?;

	session.close().await?;
	// Dropping only queues the QUIC close; send it before the runtime stops.
	client.close().await;
	Ok(())
}

/// Decode the first `audio_ms` of one rendition to PREFIX.CODEC.{txt,pcm}.
async fn decode(
	source: &moq_mux::Source,
	name: &str,
	audio: &AudioConfig,
	prefix: &str,
	audio_ms: u64,
) -> anyhow::Result<()> {
	let codec = audio.codec.to_string();
	let out = format!("{prefix}.{}", codec.split('.').next().unwrap_or_default());
	// `create_new`: two renditions of one codec would otherwise overwrite each other's decode.
	let create = |ext: &str| {
		std::fs::File::create_new(format!("{out}.{ext}")).with_context(|| format!("{name}: a second {codec} rendition"))
	};
	let mut log = create("txt")?;
	let mut pcm = create("pcm")?;

	let rendition = source.resolve(audio.broadcast.as_ref(), audio.epoch.as_ref()).await?;
	let mut options = moq_audio::decode::Options::new();
	options.output.format = moq_audio::Format::F32;
	// How far the read may fall behind live before it skips. A gap fails the run, which a reader
	// skipping behind live would cause itself: the matrix starts every subscriber at once, and a
	// runtime starved by the others can fall a second behind. That is this reader's speed, not the
	// delivery under test. A track's max-age caps it, so interop.sh's Rust publisher keeps its
	// media at least this long.
	options.max_delay = Duration::from_secs(5);
	let mut decoder = moq_audio::decode::Consumer::new(&rendition, audio, name, options).await?;
	let rate = decoder.sample_rate();
	let channels = decoder.layout().channels();

	// The catalog's rate and layout, not the decoder's: a decoder that disagrees with the
	// catalog is exactly what tone.py should catch.
	writeln!(log, "{} {}", audio.sample_rate, audio.channel_count)?;
	ensure!(
		rate == audio.sample_rate && channels as u32 == audio.channel_count,
		"{name}: decoder emits {channels} channels at {rate} Hz, the catalog says {} at {}",
		audio.channel_count,
		audio.sample_rate
	);

	let want = audio_ms * u64::from(rate) / 1000;
	let mut decoded = 0;
	while decoded < want {
		let read = decoder
			.read()
			.await?
			.with_context(|| format!("{name}: audio track ended early"))?;
		let frame = match read {
			moq_audio::decode::Read::Frame(frame) => frame,
			// moq-audio reports a hole it saw directly; tone.py checks the timestamps too.
			moq_audio::decode::Read::Gap { .. } => anyhow::bail!("{name}: audio gap after {decoded} samples"),
			moq_audio::decode::Read::End { .. } => anyhow::bail!("{name}: audio track ended early"),
		};
		let samples = frame.data.len() as u64 / (4 * channels as u64);
		writeln!(log, "{} {samples}", frame.timestamp.as_micros())?;
		pcm.write_all(&frame.data)?;
		decoded += samples;
	}
	eprintln!("decoded {decoded} samples of {name} ({codec}) with {}", decoder.name());
	Ok(())
}
