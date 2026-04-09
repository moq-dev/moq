use std::time::Duration;

use clap::Subcommand;
use hang::moq_lite;
use moq_mux::import;

#[derive(Subcommand, Clone)]
pub enum PublishFormat {
	Avc3,
	Fmp4 {
		/// Transmit the fMP4 container directly instead of decoding it.
		#[arg(long)]
		passthrough: bool,
	},
	// NOTE: No aac support because it needs framing.
	Hls {
		/// URL or file path of an HLS playlist to ingest.
		#[arg(long)]
		playlist: String,

		/// Transmit the fMP4 segments directly instead of decoding them.
		#[arg(long)]
		passthrough: bool,
	},
}

enum PublishDecoder {
	Avc3(Box<import::Avc3>),
	Fmp4(Box<import::Fmp4>),
	Hls(Box<import::Hls>),
}

impl PublishDecoder {
	/// Decode a chunk of bytes from stdin (Avc3 or Fmp4 only).
	fn decode_buf(&mut self, buffer: &mut bytes::BytesMut) -> anyhow::Result<()> {
		match self {
			Self::Avc3(d) => d.decode_stream(buffer, None),
			Self::Fmp4(d) => d.decode(buffer),
			Self::Hls(_) => unreachable!(),
		}
	}

	fn stats(&self) -> import::Stats {
		match self {
			Self::Avc3(d) => d.stats(),
			Self::Fmp4(d) => d.stats(),
			Self::Hls(d) => d.stats(),
		}
	}
}

pub struct Publish {
	decoder: PublishDecoder,
	broadcast: moq_lite::BroadcastProducer,
}

impl Publish {
	pub fn new(format: &PublishFormat) -> anyhow::Result<Self> {
		let mut broadcast = moq_lite::BroadcastProducer::default();
		let catalog = moq_mux::CatalogProducer::new(&mut broadcast)?;

		let decoder = match format {
			PublishFormat::Avc3 => {
				let avc3 = import::Avc3::new(broadcast.clone(), catalog.clone());
				PublishDecoder::Avc3(Box::new(avc3))
			}
			PublishFormat::Fmp4 { passthrough } => {
				let fmp4 = import::Fmp4::new(
					broadcast.clone(),
					catalog.clone(),
					import::Fmp4Config {
						passthrough: *passthrough,
					},
				);
				PublishDecoder::Fmp4(Box::new(fmp4))
			}
			PublishFormat::Hls { playlist, passthrough } => {
				let hls = import::Hls::new(
					broadcast.clone(),
					catalog.clone(),
					import::HlsConfig {
						playlist: playlist.clone(),
						client: None,
						passthrough: *passthrough,
					},
				)?;
				PublishDecoder::Hls(Box::new(hls))
			}
		};

		Ok(Self { decoder, broadcast })
	}

	pub fn consume(&self) -> moq_lite::BroadcastConsumer {
		self.broadcast.consume()
	}

	pub async fn run(mut self, stats_interval: Option<Duration>) -> anyhow::Result<()> {
		// The interval value doesn't matter when stats is None — the select! guard disables polling.
		let mut ticker = tokio::time::interval(stats_interval.unwrap_or(Duration::from_secs(1)));
		ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
		ticker.tick().await; // skip the first immediate tick

		let stats_enabled = stats_interval.is_some();
		let mut prev = import::Stats::default();
		let mut last_instant = tokio::time::Instant::now();

		if let PublishDecoder::Hls(decoder) = &mut self.decoder {
			decoder.init().await?;

			loop {
				let delay = decoder.step().await?;
				let sleep = tokio::time::sleep(delay);
				tokio::pin!(sleep);

				loop {
					tokio::select! {
						_ = &mut sleep => break,
						_ = ticker.tick(), if stats_enabled => {
							Self::tick_stats(decoder.stats(), &mut prev, &mut last_instant);
						}
					}
				}
			}
		} else {
			let mut stdin = tokio::io::stdin();
			let mut buffer = bytes::BytesMut::new();

			loop {
				tokio::select! {
					result = tokio::io::AsyncReadExt::read_buf(&mut stdin, &mut buffer) => {
						let n = result?;
						if n == 0 {
							return Ok(());
						}
						self.decoder.decode_buf(&mut buffer)?;
					}
					_ = ticker.tick(), if stats_enabled => {
						Self::tick_stats(self.decoder.stats(), &mut prev, &mut last_instant);
					}
				}
			}
		}
	}

	fn tick_stats(current: import::Stats, prev: &mut import::Stats, last_instant: &mut tokio::time::Instant) {
		let now = tokio::time::Instant::now();
		let elapsed = now - *last_instant;
		*last_instant = now;

		let delta = &current - prev;
		let secs = elapsed.as_secs_f64();

		let fps = delta.frames as f64 / secs;
		let bps = delta.bytes as f64 * 8.0 / secs;

		let drift_str = match delta.drift.mean() {
			Some(mean) => format!("μ={:.1}ms", mean.as_secs_f64() * 1000.0),
			None => "n/a".to_string(),
		};

		let bitrate_str = if bps >= 1_000_000.0 {
			format!("{:.1} Mbps", bps / 1_000_000.0)
		} else if bps >= 1_000.0 {
			format!("{:.1} Kbps", bps / 1_000.0)
		} else {
			format!("{:.0} bps", bps)
		};

		eprintln!("frames: {:.0}/s  bitrate: {}  drift: {}", fps, bitrate_str, drift_str);

		*prev = current;
	}
}
