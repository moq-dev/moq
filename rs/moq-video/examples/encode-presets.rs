//! Measure each encoder [`Preset`] on this machine's backends.
//!
//! Feeds raw I420 through one backend for each preset (or just the one named last) and reports, per preset:
//! p50 / p95 / max frame-to-packet latency at real-time pacing, back-to-back
//! throughput and CPU time per frame, the most frames the codec held at once,
//! the frames it skipped, and the achieved bitrate. Each preset's stream is
//! written next to the input, with the indices of any skipped frames beside it,
//! so quality can be scored against the source with ffmpeg:
//!
//! ```sh
//! ffmpeg -f lavfi -i "mandelbrot=size=1280x720:rate=30" -t 10 -pix_fmt yuv420p src.yuv
//! cargo run --release -p moq-video --example encode-presets -- src.yuv 1280x720 30 3000000 nvenc h264
//! ffmpeg -i src.yuv.nvenc-h264-balanced.h264 -s 1280x720 -f rawvideo -pix_fmt yuv420p -i src.yuv \
//!     -lavfi "[0:v][1:v]psnr;[0:v][1:v]ssim" -f null -
//! ```
//!
//! Hardware on another machine needs its own run; nothing here extrapolates.

use std::time::{Duration, Instant};

use moq_video::encode::{Codec, Config, Encoder, Kind, Preset};
use moq_video::{Frame, I420, Rate, Size, Surface};

const PRESETS: [(Preset, &str); 3] = [
	(Preset::LowLatency, "low-latency"),
	(Preset::Balanced, "balanced"),
	(Preset::Quality, "quality"),
];

fn main() -> anyhow::Result<()> {
	let args: Vec<String> = std::env::args().skip(1).collect();
	let (args, only) = match args.as_slice() {
		[rest @ .., only] if args.len() == 7 => (rest, Some(only.as_str())),
		rest => (rest, None),
	};
	let [input, size, fps, bitrate, backend, codec] = args else {
		anyhow::bail!("usage: encode-presets <input.yuv> <WxH> <fps> <bitrate bps> <backend> <h264|h265> [preset]");
	};
	let (width, height) = size.split_once('x').ok_or_else(|| anyhow::anyhow!("size is WxH"))?;
	let size = Size::new(width.parse()?, height.parse()?);
	let fps: u32 = fps.parse()?;
	let bitrate: u64 = bitrate.parse()?;
	let codec = match codec.as_str() {
		"h264" => Codec::H264,
		"h265" => Codec::H265,
		other => anyhow::bail!("unknown codec {other}"),
	};

	let raw = std::fs::read(input)?;
	let len = I420::len(size)?;
	let pictures: Vec<&[u8]> = raw.chunks_exact(len).collect();
	anyhow::ensure!(!pictures.is_empty(), "input holds no {size} I420 frame");

	println!("preset\tcontrols\tp50 ms\tp95 ms\tmax ms\tfps\tcpu ms/frame\theld\tskipped\tkbps");
	for (preset, label) in PRESETS
		.into_iter()
		.filter(|(_, label)| only.is_none_or(|only| only == *label))
	{
		let mut config = Config::new(size.width, size.height, Rate::new(fps, 1)?);
		config.codec = codec;
		config.kind = Kind::Named(backend.clone());
		config.bitrate = Some(moq_net::bandwidth::Rate::from_bps(bitrate));
		config.preset = preset;

		let paced = run(&config, &pictures, true)?;
		let burst = run(&config, &pictures, false)?;

		let extension = match codec {
			Codec::H265 => "hevc",
			_ => "h264",
		};
		let output = format!("{input}.{backend}-{extension}-{label}.{extension}");
		std::fs::write(&output, &paced.stream)?;
		// The frames the stream lacks, so a quality comparison can drop them from the source.
		let skipped: Vec<String> = paced.skipped.iter().map(ToString::to_string).collect();
		std::fs::write(format!("{output}.skipped"), skipped.join(" "))?;

		let mut latency = paced.latency.clone();
		latency.sort();
		let pick = |q: f64| latency[((latency.len() - 1) as f64 * q).round() as usize].as_secs_f64() * 1e3;
		let seconds = pictures.len() as f64 / fps as f64;
		println!(
			"{label}\t{}\t{:.2}\t{:.2}\t{:.2}\t{:.0}\t{:.2}\t{}\t{}\t{:.0}",
			paced.controls,
			pick(0.5),
			pick(0.95),
			pick(1.0),
			pictures.len() as f64 / burst.wall.as_secs_f64(),
			burst.cpu.as_secs_f64() * 1e3 / pictures.len() as f64,
			paced.held.max(burst.held),
			paced.skipped.len(),
			paced.stream.len() as f64 * 8.0 / seconds / 1e3,
		);
	}
	Ok(())
}

struct Run {
	controls: String,
	/// Per emitted frame, from `encode` being called to the packet stamped with it coming back.
	latency: Vec<Duration>,
	/// Frames the codec dropped rather than encoded (openh264 skips to hold its rate).
	skipped: Vec<usize>,
	/// The most frames submitted and later emitted, but not emitted yet.
	held: usize,
	stream: Vec<u8>,
	wall: Duration,
	cpu: Duration,
}

/// Encode every picture, at real-time pacing when `paced` and back to back otherwise.
fn run(config: &Config, pictures: &[&[u8]], paced: bool) -> anyhow::Result<Run> {
	let mut encoder = Encoder::new(config)?;
	let applied = encoder.applied();
	let controls = format!("{} {:?}: {}", encoder.name(), applied.preset, applied.controls);

	let frame_time = Duration::from_micros(1_000_000 / config.framerate.rounded() as u64);
	let mut submitted: Vec<Instant> = Vec::with_capacity(pictures.len());
	// Per frame, how long its packet took, or `None` while (or if never) emitted.
	let mut emitted: Vec<Option<Duration>> = vec![None; pictures.len()];
	// Per call, how many frames had come back by its end.
	let mut returned = Vec::with_capacity(pictures.len());
	let mut stream = Vec::new();

	let mut collect =
		|packets: Vec<moq_video::encode::Encoded>, submitted: &[Instant], emitted: &mut [Option<Duration>]| {
			let now = Instant::now();
			for packet in packets {
				let index = (packet.timestamp.as_micros() / frame_time.as_micros()) as usize;
				emitted[index] = Some(now - submitted[index]);
				stream.extend_from_slice(&packet.payload);
			}
		};

	let start = Instant::now();
	let cpu = cpu_time();
	for (index, picture) in pictures.iter().enumerate() {
		if paced {
			let due = start + frame_time * index as u32;
			if let Some(wait) = due.checked_duration_since(Instant::now()) {
				std::thread::sleep(wait);
			}
		}
		let i420 = I420::new(config.size(), picture.to_vec())?;
		let timestamp = moq_net::Timestamp::from_micros((frame_time * index as u32).as_micros() as u64)?;
		submitted.push(Instant::now());
		let packets = encoder.encode(&Frame::new(Surface::I420(i420), timestamp))?;
		collect(packets, &submitted, &mut emitted);
		returned.push(emitted.iter().filter(|e| e.is_some()).count());
	}
	collect(encoder.finish()?, &submitted, &mut emitted);
	let (wall, cpu) = (start.elapsed(), cpu_time() - cpu);

	// Only knowable at the end: a frame that never came back was skipped, not held.
	let skipped: Vec<usize> = (0..emitted.len()).filter(|&i| emitted[i].is_none()).collect();
	let held = returned
		.iter()
		.enumerate()
		.map(|(call, &back)| call + 1 - skipped.iter().filter(|&&i| i <= call).count() - back)
		.max()
		.unwrap_or(0);

	Ok(Run {
		controls,
		latency: emitted.into_iter().flatten().collect(),
		skipped,
		held,
		stream,
		wall,
		cpu,
	})
}

/// CPU time this process has used, across every thread (a codec may spread out).
#[cfg(not(unix))]
fn cpu_time() -> Duration {
	Duration::ZERO
}

/// CPU time this process has used, across every thread (a codec may spread out).
#[cfg(unix)]
fn cpu_time() -> Duration {
	let mut now = libc::timespec { tv_sec: 0, tv_nsec: 0 };
	// SAFETY: `now` is a valid out-pointer for the duration of the call.
	unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut now) };
	Duration::new(now.tv_sec as u64, now.tv_nsec as u32)
}
