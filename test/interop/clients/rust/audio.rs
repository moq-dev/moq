//! Audio interop subscriber: decode a broadcast's audio with moq-audio for tone.py to judge.
//!
//! Waits for a catalog with an enabled audio rendition, decodes its first AUDIO_MS at the codec's
//! own rate and layout, and writes PREFIX.txt (`<rate> <channels>`, then `<timestamp us> <samples>`
//! per frame) and PREFIX.pcm (f32le interleaved). The CLI's `export` covers the video side.
//!
//!     interop-audio URL BROADCAST PREFIX AUDIO_MS [VERSION]
use std::io::Write;
use std::time::Duration;

use anyhow::{Context, ensure};
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

	let broadcast = consumer.routed_broadcast(&args[2]).await?;
	let source = moq_mux::Source::new(consumer, broadcast.clone());
	let mut catalogs = moq_mux::catalog::Consumer::<()>::new(&broadcast, CatalogFormat::Hang).await?;
	// A lazy publisher (the browser) may add audio in a later catalog update.
	let (name, audio) = loop {
		let catalog = catalogs.next().await?.context("catalog ended without audio")?;
		if let Some((name, audio)) = catalog.audio.enabled().next() {
			break (name.clone(), audio.clone());
		}
	};
	let rendition = source.resolve(audio.broadcast.as_ref(), audio.epoch.as_ref()).await?;

	let mut options = moq_audio::decode::Options::new();
	options.output.format = moq_audio::Format::F32;
	options.max_delay = Duration::from_secs(1);
	let mut decoder = moq_audio::decode::Consumer::new(&rendition, &audio, &name, options).await?;
	let rate = decoder.sample_rate();
	let channels = decoder.layout().channels();

	let mut log = std::fs::File::create(format!("{prefix}.txt"))?;
	let mut pcm = std::fs::File::create(format!("{prefix}.pcm"))?;
	// The catalog's rate and layout, not the decoder's: a decoder that disagrees with the
	// catalog is exactly what tone.py should catch.
	writeln!(log, "{} {}", audio.sample_rate, audio.channel_count)?;
	ensure!(
		rate == audio.sample_rate && channels as u32 == audio.channel_count,
		"decoder emits {channels} channels at {rate} Hz, the catalog says {} at {}",
		audio.channel_count,
		audio.sample_rate
	);

	let want = audio_ms * u64::from(rate) / 1000;
	let mut decoded = 0;
	while decoded < want {
		let read = decoder.read().await?.context("audio track ended early")?;
		let frame = match read {
			moq_audio::decode::Read::Frame(frame) => frame,
			// moq-audio reports a hole it saw directly; tone.py checks the timestamps too.
			moq_audio::decode::Read::Gap { .. } => anyhow::bail!("audio gap after {decoded} samples"),
			moq_audio::decode::Read::End { .. } => anyhow::bail!("audio track ended early"),
		};
		let samples = frame.data.len() as u64 / (4 * channels as u64);
		writeln!(log, "{} {samples}", frame.timestamp.as_micros())?;
		pcm.write_all(&frame.data)?;
		decoded += samples;
	}
	eprintln!("decoded {decoded} samples of {name} with {}", decoder.name());

	drop(decoder);
	session.close().await?;
	// Dropping only queues the QUIC close; send it before the runtime stops.
	client.close().await;
	Ok(())
}
