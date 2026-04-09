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
}

impl Publish {
	pub async fn run(mut self, stats_interval: Option<Duration>) -> anyhow::Result<()> {
		match stats_interval {
			Some(interval) => self.run_with_stats(interval).await,
			None => self.run_plain().await,
		}
	}

	async fn run_plain(&mut self) -> anyhow::Result<()> {
		let mut stdin = tokio::io::stdin();

		match &mut self.decoder {
			PublishDecoder::Avc3(decoder) => decoder.decode_from(&mut stdin).await,
			PublishDecoder::Fmp4(decoder) => decoder.decode_from(&mut stdin).await,
			PublishDecoder::Hls(decoder) => decoder.run().await,
		}
	}

	async fn run_with_stats(&mut self, interval: Duration) -> anyhow::Result<()> {
		let mut ticker = tokio::time::interval(interval);
		ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
		// Skip the first immediate tick.
		ticker.tick().await;

		let mut prev = import::Stats::default();
		let mut last_instant = tokio::time::Instant::now();

		match &mut self.decoder {
			PublishDecoder::Avc3(decoder) => {
				let mut stdin = tokio::io::stdin();
				let mut buffer = bytes::BytesMut::new();

				loop {
					tokio::select! {
						result = tokio::io::AsyncReadExt::read_buf(&mut stdin, &mut buffer) => {
							let n = result?;
							if n == 0 {
								return Ok(());
							}
							decoder.decode_stream(&mut buffer, None)?;
						}
						_ = ticker.tick() => {
							let now = tokio::time::Instant::now();
							let elapsed = now - last_instant;
							last_instant = now;
							let current = decoder.stats();
							Self::print_stats(&current, &prev, elapsed);
							prev = current;
						}
					}
				}
			}
			PublishDecoder::Fmp4(decoder) => {
				let mut stdin = tokio::io::stdin();
				let mut buffer = bytes::BytesMut::new();

				loop {
					tokio::select! {
						result = tokio::io::AsyncReadExt::read_buf(&mut stdin, &mut buffer) => {
							let n = result?;
							if n == 0 {
								return Ok(());
							}
							decoder.decode(&mut buffer)?;
						}
						_ = ticker.tick() => {
							let now = tokio::time::Instant::now();
							let elapsed = now - last_instant;
							last_instant = now;
							let current = decoder.stats();
							Self::print_stats(&current, &prev, elapsed);
							prev = current;
						}
					}
				}
			}
			PublishDecoder::Hls(decoder) => {
				decoder.init().await?;

				loop {
					let delay = decoder.step().await?;
					let sleep = tokio::time::sleep(delay);
					tokio::pin!(sleep);

					// Wait for the full delay, printing stats if a tick fires mid-sleep.
					loop {
						tokio::select! {
							_ = &mut sleep => break,
							_ = ticker.tick() => {
								let now = tokio::time::Instant::now();
								let elapsed = now - last_instant;
								last_instant = now;
								let current = decoder.stats();
								Self::print_stats(&current, &prev, elapsed);
								prev = current;
							}
						}
					}
				}
			}
		}
	}

	fn print_stats(current: &import::Stats, prev: &import::Stats, elapsed: Duration) {
		let delta = current.delta(prev);
		let secs = elapsed.as_secs_f64();

		let fps = delta.frames as f64 / secs;
		let kps = delta.keyframes as f64 / secs;
		let bps = delta.bytes as f64 / secs;

		let drift_str = match delta.drift.mean() {
			Some(mean) => format!("μ={:.1}ms", mean.as_secs_f64() * 1000.0),
			None => "n/a".to_string(),
		};

		let bytes_str = if bps >= 1_000_000.0 {
			format!("{:.1} MB/s", bps / 1_000_000.0)
		} else if bps >= 1_000.0 {
			format!("{:.1} KB/s", bps / 1_000.0)
		} else {
			format!("{:.0} B/s", bps)
		};

		eprintln!(
			"frames: {:.0}/s  keyframes: {:.0}/s  bytes: {}  drift: {}",
			fps, kps, bytes_str, drift_str
		);
	}
}
