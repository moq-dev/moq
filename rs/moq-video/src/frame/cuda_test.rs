use std::num::NonZeroUsize;
use std::time::Duration;

use super::*;
use crate::frame::vulkan::tests::Producer;
use crate::frame::vulkan::{Channels, Image, ImportError, Importer, Timeline};
use crate::{Frame as VideoFrame, Surface};

/// A gradient with structure on both axes, so a pitch, plane, or channel-order
/// bug moves the picture. Bytes are in `channels` order with alpha 255.
fn gradient(size: Size, channels: Channels, shift: usize) -> Vec<u8> {
	let (w, h) = (size.width as usize, size.height as usize);
	let mut pixels = vec![0u8; w * h * 4];
	for y in 0..h {
		for x in 0..w {
			let (r, g, b) = (
				((x + shift) * 255 / w) as u8,
				(y * 255 / h) as u8,
				((x + y + shift) * 255 / (w + h)) as u8,
			);
			let i = (y * w + x) * 4;
			pixels[i..i + 4].copy_from_slice(&match channels {
				Channels::Rgba => [r, g, b, 255],
				Channels::Bgra => [b, g, r, 255],
			});
		}
	}
	pixels
}

/// The kernel's arithmetic on the CPU: per-pixel luma through the matrix, and
/// chroma from the 2x2 block average. Test instrumentation, not a runtime path.
fn reference(rgba: &[u8], size: Size, color: Color) -> I420 {
	let (w, h) = (size.width as usize, size.height as usize);
	let weights = color.coefficients();
	let mut data = vec![0u8; I420::len(size).unwrap()];
	let (y_plane, chroma) = data.split_at_mut(w * h);
	let (u_plane, v_plane) = chroma.split_at_mut(w * h / 4);
	let rgb = |x: usize, y: usize| {
		let i = (y * w + x) * 4;
		[rgba[i], rgba[i + 1], rgba[i + 2]]
	};
	for y in 0..h {
		for x in 0..w {
			y_plane[y * w + x] = weights.apply(rgb(x, y))[0];
		}
	}
	let dot = |weights: [f32; 4], rgb: [f32; 3]| {
		(weights[0] * rgb[0] + weights[1] * rgb[1] + weights[2] * rgb[2] + weights[3])
			.round()
			.clamp(0.0, 255.0) as u8
	};
	for cy in 0..h / 2 {
		for cx in 0..w / 2 {
			let mut sum = [0.0f32; 3];
			for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
				let p = rgb(cx * 2 + dx, cy * 2 + dy);
				for (s, c) in sum.iter_mut().zip(p) {
					*s += f32::from(c) / 4.0;
				}
			}
			u_plane[cy * w / 2 + cx] = dot(weights.u, sum);
			v_plane[cy * w / 2 + cx] = dot(weights.v, sum);
		}
	}
	I420::new(size, data).unwrap().with_color(color)
}

fn mae(a: &[u8], b: &[u8]) -> u64 {
	assert_eq!(a.len(), b.len());
	a.iter().zip(b).map(|(x, y)| u64::from(x.abs_diff(*y))).sum::<u64>() / a.len() as u64
}

fn max_diff(a: &[u8], b: &[u8]) -> u8 {
	assert_eq!(a.len(), b.len());
	a.iter().zip(b).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0)
}

/// H.264 NAL unit types in an Annex-B buffer, by 3-byte start code.
fn nal_types(annexb: &[u8]) -> Vec<u8> {
	let mut types = Vec::new();
	let mut i = 0;
	while i + 3 < annexb.len() {
		if annexb[i..i + 3] == [0, 0, 1] {
			types.push(annexb[i + 3] & 0x1f);
			i += 3;
		} else {
			i += 1;
		}
	}
	types
}

fn nvenc(size: Size, color: Color) -> crate::encode::Encoder {
	let mut config = crate::encode::Config::new(size.width, size.height, crate::Rate::new(30, 1).unwrap());
	config.kind = crate::encode::Kind::Named("nvenc".into());
	config.color = Some(color);
	config.gop = crate::encode::Gop::Keyframe { interval: 30 };
	crate::encode::Encoder::new(&config).expect("open NVENC")
}

/// Real hardware only, and loud about it: the opt-in `just rs vulkan-cuda`
/// recipe runs this, and a machine without the GPU path fails rather than
/// reporting a pass it did not earn.
///
/// A native Vulkan producer uploads gradients into one exportable image, CUDA
/// converts it to NV12 in RGBA and BGRA channel orders, scales it, and NVENC
/// encodes both renditions from the converted buffers. Readbacks compare each
/// stage against a CPU reference; they are instrumentation, the production API
/// exposes no CPU pixel path.
#[tokio::test]
#[ignore = "requires a Linux NVIDIA GPU with Vulkan/CUDA external memory and NVENC"]
async fn vulkan_cuda_convert_resize_encode() {
	// Both renditions clear NVENC's minimum encode width, which an RTX 3070 Ti
	// reports as 145: 160x96 opens a session, 144x128 is refused with "Frame
	// Dimension less than the minimum supported value". Smaller pictures
	// convert and scale fine, there is just no encoder to hand them to.
	let size = Size::new(320, 192);
	let sd = Size::new(160, 96);
	let color = Color::Bt709Limited;
	let mut producer = Producer::new(size).expect("no Vulkan NVIDIA device: this opt-in test needs one");
	let importer = Importer::new(0, NonZeroUsize::new(1).unwrap()).expect("CUDA importer");
	let converter = Converter::new(0, color, NonZeroUsize::new(3).unwrap()).expect("CUDA converter");
	assert_eq!(converter.color(), color);

	// Round one, RGBA: conversion, resize, and the pool bound.
	let image = Image::rgba8(producer.uuid, size, producer.allocation_size).unwrap();
	let slot = importer
		.import(producer.export(), image, ())
		.map_err(ImportError::into_parts)
		.expect("import RGBA image");
	let rgba = gradient(size, Channels::Rgba, 0);
	let expected = reference(&rgba, size, color);
	producer.upload(&rgba, None, 1);
	let (frame, completion) = slot.publish(Timeline::new(1, 2).unwrap()).unwrap();

	let converted = converter
		.reserve()
		.expect("an empty pool")
		.convert(&frame)
		.expect("convert RGBA");
	assert_eq!(converted.size(), size);
	assert_eq!(converted.color(), Some(color));
	assert!(converted.pitch.is_multiple_of(256) && converted.pitch >= size.width);
	let actual = converted.download_i420().unwrap();
	assert_eq!(actual.color(), Some(color));
	eprintln!(
		"rgba conversion max diff y={} u={} v={}",
		max_diff(actual.y(), expected.y()),
		max_diff(actual.u(), expected.u()),
		max_diff(actual.v(), expected.v())
	);
	assert!(
		max_diff(actual.y(), expected.y()) <= 1,
		"luma differs from the reference"
	);
	assert!(max_diff(actual.u(), expected.u()) <= 1, "u differs from the reference");
	assert!(max_diff(actual.v(), expected.v()) <= 1, "v differs from the reference");

	let scaled = converter
		.reserve()
		.expect("a free buffer")
		.resize(&converted, sd)
		.expect("GPU resize");
	assert_eq!(scaled.size(), sd);
	assert_eq!(scaled.color(), Some(color));
	let actual = scaled.download_i420().unwrap();
	let expected_sd = expected.resize(sd).unwrap();
	eprintln!(
		"resize mae y={} u={} v={}",
		mae(actual.y(), expected_sd.y()),
		mae(actual.u(), expected_sd.u()),
		mae(actual.v(), expected_sd.v())
	);
	assert!(
		mae(actual.y(), expected_sd.y()) < 4,
		"scaled luma disagrees with the CPU"
	);
	assert!(mae(actual.u(), expected_sd.u()) < 4, "scaled u disagrees with the CPU");
	assert!(mae(actual.v(), expected_sd.v()) < 4, "scaled v disagrees with the CPU");

	// Three buffers live fills the pool; a fourth is not reserved, let alone
	// allocated.
	let third = converter.reserve().expect("third buffer");
	assert!(converter.reserve().is_none(), "a full pool must refuse");
	// A slot dropped unused, or consumed by a failed resize, gives its buffer
	// back.
	drop(third);
	let odd = converter.reserve().expect("an unfilled slot returned its buffer");
	odd.resize(&converted, Size::new(81, 49))
		.expect_err("an odd size is an error");
	let third = converter
		.reserve()
		.expect("a failed resize returned its buffer")
		.convert(&frame)
		.expect("third conversion");
	assert!(converter.reserve().is_none());
	drop(third);
	let third = converter.reserve().expect("a returned buffer is reusable");
	drop(third.convert(&frame).expect("convert into a reused buffer"));
	eprintln!("pool bound held at capacity 3");

	drop((converted, scaled, frame));
	let slot = tokio::time::timeout(std::time::Duration::from_secs(2), completion.wait())
		.await
		.expect("CUDA completion timed out")
		.expect("completion worker stopped");
	drop(slot);

	// Round two, BGRA: the same picture in the other channel order converts to
	// the same samples, and both renditions encode through NVENC in place.
	let image = Image::bgra8(producer.uuid, size, producer.allocation_size).unwrap();
	let mut slot = importer
		.import(producer.export(), image, ())
		.map_err(ImportError::into_parts)
		.expect("import BGRA image");
	let mut hd = nvenc(size, color);
	let mut sd_encoder = nvenc(sd, color);
	assert_eq!(hd.name(), "nvenc");
	let decode = crate::decode::Config {
		kind: crate::decode::Kind::Software,
		..crate::decode::Config::new()
	};
	let mut decoder = crate::decode::backend::open(crate::decode::backend::Codec::H264, &decode).unwrap();
	let mut decoded = None;
	let mut sd_packets = 0;
	let mut expected = None;

	for i in 0..8u64 {
		let ready = 2 * i + 3;
		let bgra = gradient(size, Channels::Bgra, i as usize * 4);
		producer.upload(&bgra, Some(ready - 1), ready);
		let (frame, completion) = slot.publish(Timeline::new(ready, ready + 1).unwrap()).unwrap();

		let converted = converter
			.reserve()
			.expect("a free buffer")
			.convert(&frame)
			.expect("convert BGRA");
		if i == 0 {
			let rgba = gradient(size, Channels::Rgba, 0);
			let actual = converted.download_i420().unwrap();
			let reference = reference(&rgba, size, color);
			assert_eq!(actual.y(), reference.y(), "BGRA luma differs from RGBA");
			assert!(max_diff(actual.u(), reference.u()) <= 1);
			assert!(max_diff(actual.v(), reference.v()) <= 1);
		}
		let scaled = converter
			.reserve()
			.expect("a free buffer")
			.resize(&converted, sd)
			.expect("GPU resize");
		drop(frame);

		let timestamp = moq_net::Timestamp::from_micros(i * 33_333).unwrap();
		if i == 4 {
			hd.cut().expect("NVENC cuts on request");
			sd_encoder.cut().expect("NVENC cuts on request");
		}
		let packets = hd
			.encode(&VideoFrame::new(Surface::Cuda(converted), timestamp))
			.unwrap();
		assert_eq!(packets.len(), 1, "one access unit per frame at {i}");
		assert_eq!(packets[0].timestamp, timestamp, "timestamp preserved at {i}");
		let types = nal_types(&packets[0].payload);
		let idr = types.contains(&5);
		assert_eq!(idr, i == 0 || i == 4, "IDR placement at {i}: {types:?}");
		if idr {
			assert!(types.contains(&7) && types.contains(&8), "IDR at {i} lacks SPS/PPS");
		}
		for out in decoder.decode(packets[0].payload.clone(), timestamp, i == 0).unwrap() {
			decoded = Some(out.surface.to_i420().unwrap().into_owned());
			expected = Some(reference(&gradient(size, Channels::Rgba, i as usize * 4), size, color));
		}

		let packets = sd_encoder
			.encode(&VideoFrame::new(Surface::Cuda(scaled), timestamp))
			.unwrap();
		assert_eq!(packets.len(), 1);
		assert_eq!(packets[0].timestamp, timestamp);
		sd_packets += packets.len();

		slot = tokio::time::timeout(std::time::Duration::from_secs(2), completion.wait())
			.await
			.expect("CUDA completion timed out")
			.expect("completion worker stopped");
	}
	assert_eq!(sd_packets, 8);

	// The decoded picture is the uploaded gradient: the registered NVENC input
	// read the converted buffer at the right pitch and planes.
	let decoded = decoded.expect("openh264 decoded the NVENC stream");
	let expected = expected.unwrap();
	eprintln!(
		"encode roundtrip mae y={} u={} v={}",
		mae(decoded.y(), expected.y()),
		mae(decoded.u(), expected.u()),
		mae(decoded.v(), expected.v())
	);
	assert!(mae(decoded.y(), expected.y()) < 8, "decoded luma corrupt");
	assert!(mae(decoded.u(), expected.u()) < 8, "decoded u corrupt");
	assert!(mae(decoded.v(), expected.v()) < 8, "decoded v corrupt");

	drop((hd, sd_encoder, slot, importer, converter));
	eprintln!("encoders, importer, and converter torn down");
}

/// Process CPU time, the sum over every thread: the conversion runs on the
/// caller, but CUDA's completion worker and the driver's own threads are part
/// of what the pipeline costs.
fn cpu_now() -> Duration {
	let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
	// SAFETY: writes a timespec this function owns; the clock always exists on Linux.
	assert_eq!(
		unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut ts) },
		0
	);
	Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32)
}

/// Wall and CPU samples for one pipeline stage.
#[derive(Default)]
struct Stage {
	wall: Vec<Duration>,
	cpu: Vec<Duration>,
}

impl Stage {
	fn measure<T>(&mut self, f: impl FnOnce() -> T) -> T {
		let (wall, cpu) = (std::time::Instant::now(), cpu_now());
		let out = f();
		self.wall.push(wall.elapsed());
		self.cpu.push(cpu_now() - cpu);
		out
	}

	/// Mean / p50 / p95 / max wall latency and mean CPU, in microseconds.
	fn report(&self, name: &str) -> Duration {
		let micros = |d: &Duration| d.as_secs_f64() * 1e6;
		let mut sorted: Vec<f64> = self.wall.iter().map(micros).collect();
		sorted.sort_by(f64::total_cmp);
		let at = |q: f64| sorted[((sorted.len() - 1) as f64 * q).round() as usize];
		let total: Duration = self.wall.iter().sum();
		let cpu: Duration = self.cpu.iter().sum();
		let n = self.wall.len() as f64;
		eprintln!(
			"{name:<10} wall mean={:>7.0}us p50={:>7.0}us p95={:>7.0}us max={:>7.0}us  cpu mean={:>7.0}us",
			micros(&total) / n,
			at(0.5),
			at(0.95),
			at(1.0),
			micros(&cpu) / n,
		);
		total
	}
}

/// Real hardware only: the target workload, three 1280x720 views at 30 fps,
/// each converted on the GPU, scaled to a 640x360 second rendition and encoded
/// through NVENC in place.
///
/// There are no acceptance thresholds. The numbers it prints are the point:
/// per-stage latency, what the pipeline costs in CPU, and how much of a 30 fps
/// tick the three views leave unused. Nothing here reads a pixel back, so the
/// only host traffic is each rendition's compressed bitstream; run the binary
/// under `nsys profile --trace=cuda` to see that no `cuMemcpy` happens at all.
#[tokio::test]
#[ignore = "requires a Linux NVIDIA GPU with Vulkan/CUDA external memory and NVENC"]
async fn vulkan_cuda_three_view_workload() {
	const VIEWS: usize = 3;
	const WARMUP: u64 = 10;
	const TICKS: u64 = 90;
	let size = Size::new(1280, 720);
	let sd = Size::new(640, 360);
	let color = Color::Bt709Limited;
	let tick = Duration::from_nanos(1_000_000_000 / 30);

	// One picture per tick phase, so the encoders see real motion instead of a
	// frozen frame no P-slice has to code.
	let pictures: Vec<Vec<u8>> = (0..8).map(|i| gradient(size, Channels::Rgba, i * 37)).collect();

	let importer = Importer::new(0, NonZeroUsize::new(VIEWS).unwrap()).expect("CUDA importer");
	// Two live buffers per view (the converted frame and its scaled copy) plus
	// one of slack, the sizing the `Converter` docs recommend.
	let converter = Converter::new(0, color, NonZeroUsize::new(2 * VIEWS + 1).unwrap()).expect("CUDA converter");

	let mut views = Vec::new();
	for _ in 0..VIEWS {
		let mut producer = Producer::new(size).expect("no Vulkan NVIDIA device: this opt-in test needs one");
		let image = Image::rgba8(producer.uuid, size, producer.allocation_size).unwrap();
		let slot = importer
			.import(producer.export(), image, ())
			.map_err(ImportError::into_parts)
			.expect("import view");
		producer.upload(&pictures[0], None, 1);
		views.push((producer, Some(slot), nvenc(size, color), nvenc(sd, color), 1u64));
	}

	let mut upload = Stage::default();
	let (mut publish, mut convert, mut resize) = (Stage::default(), Stage::default(), Stage::default());
	let (mut encode_hd, mut encode_sd, mut complete) = (Stage::default(), Stage::default(), Stage::default());
	let mut bytes = (0usize, 0usize);
	let mut late = 0u64;
	let mut started = std::time::Instant::now();

	for tick_index in 0..WARMUP + TICKS {
		// Discard the warmup: the first ticks pay for PTX JIT, NVENC's lazy
		// allocation and the pool's first buffers.
		if tick_index == WARMUP {
			upload = Stage::default();
			publish = Stage::default();
			convert = Stage::default();
			resize = Stage::default();
			encode_hd = Stage::default();
			encode_sd = Stage::default();
			complete = Stage::default();
			bytes = (0, 0);
			late = 0;
			started = std::time::Instant::now();
		}
		let deadline = std::time::Instant::now() + tick;
		let timestamp = moq_net::Timestamp::from_micros(tick_index * 33_333).unwrap();

		for (producer, slot, hd, small, signal) in &mut views {
			let (frame, completion) = publish
				.measure(|| {
					slot.take()
						.unwrap()
						.publish(Timeline::new(*signal, *signal + 1).unwrap())
				})
				.unwrap();

			let converted = convert
				.measure(|| converter.reserve().expect("a free buffer").convert(&frame))
				.expect("convert");
			drop(frame);
			let scaled = resize
				.measure(|| converter.reserve().expect("a free buffer").resize(&converted, sd))
				.expect("resize");

			let packets = encode_hd
				.measure(|| hd.encode(&VideoFrame::new(Surface::Cuda(converted), timestamp)))
				.expect("encode HD");
			bytes.0 += packets.iter().map(|p| p.payload.len()).sum::<usize>();
			let packets = encode_sd
				.measure(|| small.encode(&VideoFrame::new(Surface::Cuda(scaled), timestamp)))
				.expect("encode SD");
			bytes.1 += packets.iter().map(|p| p.payload.len()).sum::<usize>();

			let (wall, cpu) = (std::time::Instant::now(), cpu_now());
			let returned = tokio::time::timeout(Duration::from_secs(2), completion.wait())
				.await
				.expect("CUDA completion timed out")
				.expect("completion worker stopped");
			complete.wall.push(wall.elapsed());
			complete.cpu.push(cpu_now() - cpu);

			// The next upload waits on the completion CUDA just queued and
			// signals the value the next publish declares ready.
			*signal += 2;
			let picture = &pictures[(tick_index as usize + 1) % pictures.len()];
			upload.measure(|| producer.upload(picture, Some(*signal - 1), *signal));
			*slot = Some(returned);
		}

		match deadline.checked_duration_since(std::time::Instant::now()) {
			Some(idle) => tokio::time::sleep(idle).await,
			None => late += 1,
		}
	}

	let elapsed = started.elapsed();
	eprintln!("three 1280x720 views at 30 fps, {TICKS} ticks in {elapsed:.2?}");
	let pipeline: Duration = [
		("publish", &publish),
		("convert", &convert),
		("resize", &resize),
		("encode hd", &encode_hd),
		("encode sd", &encode_sd),
		("complete", &complete),
	]
	.iter()
	.map(|(name, stage)| stage.report(name))
	.sum();
	// The producer's staging copy: a stand-in for a renderer writing the image,
	// not part of what the pipeline costs.
	upload.report("upload*");
	eprintln!(
		"pipeline {:.1}ms of the {:.1}ms tick budget ({} ticks over), instrumentation upload excluded",
		pipeline.as_secs_f64() * 1e3 / TICKS as f64,
		tick.as_secs_f64() * 1e3,
		late,
	);
	eprintln!(
		"host bytes out: hd={} sd={} ({:.2} Mbps total), zero pixel readbacks",
		bytes.0,
		bytes.1,
		(bytes.0 + bytes.1) as f64 * 8.0 / elapsed.as_secs_f64() / 1e6,
	);
}
