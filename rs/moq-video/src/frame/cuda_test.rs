use std::num::NonZeroUsize;

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
	let size = Size::new(256, 128);
	let sd = Size::new(128, 64);
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

	let converted = converter.convert(&frame).expect("convert RGBA");
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

	let scaled = converted.resize(sd).expect("GPU resize");
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

	// Three buffers live fills the pool; a fourth is refused, not allocated.
	let third = converter.convert(&frame).expect("third buffer");
	let refused = converter.convert(&frame).expect_err("a full pool must refuse");
	assert!(matches!(refused, Error::Unsupported(_)), "{refused}");
	assert!(matches!(scaled.resize(sd), Err(Error::Unsupported(_))));
	drop(third);
	drop(converter.convert(&frame).expect("a returned buffer is reusable"));
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

		let converted = converter.convert(&frame).expect("convert BGRA");
		if i == 0 {
			let rgba = gradient(size, Channels::Rgba, 0);
			let actual = converted.download_i420().unwrap();
			let reference = reference(&rgba, size, color);
			assert_eq!(actual.y(), reference.y(), "BGRA luma differs from RGBA");
			assert!(max_diff(actual.u(), reference.u()) <= 1);
			assert!(max_diff(actual.v(), reference.v()) <= 1);
		}
		let scaled = converted.resize(sd).expect("GPU resize");
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
