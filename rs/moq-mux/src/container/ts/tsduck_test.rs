//! TSDuck validation of exported MPEG-TS.
//!
//! The tests in `export_test` re-parse our output with the `mpeg2ts` crate,
//! which can share blind spots with the muxer (both encode our reading of the
//! spec). These smoke tests hand the exported bytes to TSDuck's `tsanalyze`,
//! an independent reference analyzer, and assert every error counter in its
//! report is zero: sync bytes, transport errors, continuity discontinuities,
//! PCR/PTS/DTS leaps, PES start prefixes, and unreferenced PIDs.
//!
//! They skip when `tsanalyze` is not on `$PATH`; the nix dev shell provides
//! TSDuck, so `just ci` always runs them.

use std::process::Command;

use bytes::{Bytes, BytesMut};

use crate::catalog::hang::Container as HangContainer;
use crate::container::Timestamp;
use crate::container::ts::{Export, Import, catalog as tscat};
use crate::container::{Frame, Producer};

// libklvanc public-sample SCTE-35 cue: splice_info_section, table_id 0xFC, 30 bytes.
const CUE: &[u8] = &[
	0xfc, 0x30, 0x1b, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0xf0, 0x0a, 0x05, 0x00, 0x00, 0x2b, 0xb4, 0x7f,
	0xdf, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0xad, 0x25, 0xe8, 0x39,
];

/// True when TSDuck's `tsanalyze` is runnable; otherwise the test skips.
fn tsduck_available() -> bool {
	if Command::new("tsanalyze").arg("--version").output().is_ok() {
		return true;
	}
	eprintln!("skipping: tsanalyze (TSDuck) not on $PATH; `nix develop` provides it");
	false
}

/// Drive an exporter until it stops producing output, concatenating every
/// chunk. Same shape as `export_test::drain_with`: the producers stay alive so
/// the exporter can subscribe to the finished, retained tracks, meaning it
/// never reaches a hard end-of-stream; we pull until a `next()` blocks
/// (`Pending`, surfaced as a timeout under paused time).
async fn drain<E: crate::catalog::hang::CatalogExt>(mut exporter: Export<E>) -> BytesMut {
	let mut out = BytesMut::new();
	while let Ok(res) = tokio::time::timeout(std::time::Duration::from_secs(1), exporter.next()).await {
		let Some(frame) = res.expect("exporter error") else {
			break;
		};
		out.extend_from_slice(&frame.payload);
	}
	out
}

/// Import a TS fixture into a broadcast and export it back to TS.
async fn reexport(data: &[u8]) -> BytesMut {
	let mut broadcast = moq_net::Broadcast::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
	let mut import = Import::new(broadcast, catalog.clone());
	import.decode(&BytesMut::from(data)).unwrap();
	import.finish().unwrap();

	// `import` and `catalog` stay alive: retained tracks the exporter subscribes to.
	drain(Export::new(consumer).unwrap()).await
}

/// Run `tsanalyze --json` on the exported bytes and assert every error counter
/// is zero. Returns the parsed report for scenario-specific checks, or `None`
/// when TSDuck is unavailable (the caller skips). On failure the TS file is
/// left in the temp dir for inspection (the panic message carries its path);
/// it is removed once the shared checks pass.
fn validate(name: &str, ts: &[u8]) -> Option<serde_json::Value> {
	assert!(!ts.is_empty(), "{name}: no TS output");
	if !tsduck_available() {
		return None;
	}

	let path = std::env::temp_dir().join(format!("moq-tsduck-{name}-{}.ts", std::process::id()));
	std::fs::write(&path, ts).unwrap();
	let out = Command::new("tsanalyze")
		.args(["--json", "--deterministic"])
		.arg(&path)
		.output()
		.expect("running tsanalyze");
	let file = path.display();
	assert!(
		out.status.success(),
		"{name}: tsanalyze failed on {file}: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let report: serde_json::Value = serde_json::from_slice(&out.stdout)
		.unwrap_or_else(|e| panic!("{name}: tsanalyze produced invalid JSON for {file}: {e}"));

	// A missing counter compares as Null != 0, so a report-schema change fails
	// loudly instead of silently passing.
	let packets = &report["ts"]["packets"];
	assert!(
		packets["total"].as_u64().unwrap_or(0) > 0,
		"{name}: tsanalyze saw no packets ({file})"
	);
	for counter in ["invalid-syncs", "transport-errors", "suspect-ignored"] {
		assert_eq!(packets[counter], 0, "{name}: ts.packets.{counter} ({file})");
	}
	assert_eq!(
		report["ts"]["pids"]["unreferenced"], 0,
		"{name}: unreferenced PIDs ({file})"
	);
	assert_eq!(report["ts"]["pids"]["scrambled"], 0, "{name}: scrambled PIDs ({file})");
	assert_eq!(report["ts"]["pids"]["pcr"], 1, "{name}: exactly one PCR PID ({file})");
	assert_eq!(report["ts"]["services"]["total"], 1, "{name}: single program ({file})");

	for pid in report["pids"].as_array().expect("pids array") {
		let id = &pid["id"];
		assert_eq!(pid["unreferenced"], false, "{name}: PID {id} unreferenced ({file})");
		for counter in [
			"discontinuities",
			"invalid-scrambling",
			"pcr-leap",
			"pts-leap",
			"dts-leap",
		] {
			assert_eq!(
				pid["packets"][counter], 0,
				"{name}: PID {id} packets.{counter} ({file})"
			);
		}
		// Only PES-carrying PIDs report this one.
		if !pid["invalid-pes-prefix"].is_null() {
			assert_eq!(
				pid["invalid-pes-prefix"], 0,
				"{name}: PID {id} invalid PES prefix ({file})"
			);
		}
	}

	std::fs::remove_file(&path).unwrap();
	Some(report)
}

/// Count the PIDs tsanalyze flagged as carrying the given kind ("video"/"audio").
fn pids_carrying(report: &serde_json::Value, kind: &str) -> usize {
	report["pids"]
		.as_array()
		.expect("pids array")
		.iter()
		.filter(|p| p[kind] == true)
		.count()
}

/// The baseline A/V program: real H.264 + AAC re-exported.
#[tokio::test(start_paused = true)]
async fn av_export() {
	let ts = reexport(include_bytes!("test_data/bbb.ts")).await;
	let Some(report) = validate("bbb", &ts) else { return };
	assert_eq!(pids_carrying(&report, "video"), 1, "one H.264 PID");
	assert_eq!(pids_carrying(&report, "audio"), 1, "one AAC PID");
}

/// A real contribution feed (H.264 1080i with B-frames + two MP2 programs):
/// the authored decode timeline must reach the wire as valid DTS.
#[tokio::test(start_paused = true)]
async fn bframe_export() {
	let ts = reexport(include_bytes!("test_data/scte35/kyrion_dirtystart.ts")).await;
	let Some(report) = validate("kyrion", &ts) else { return };
	assert_eq!(pids_carrying(&report, "video"), 1, "one H.264 PID");
	assert_eq!(pids_carrying(&report, "audio"), 2, "both MP2 PIDs");
	let dts: u64 = report["pids"]
		.as_array()
		.expect("pids array")
		.iter()
		.filter(|p| p["video"] == true)
		.map(|p| p["packets"]["dts"].as_u64().expect("dts count"))
		.sum();
	assert!(dts > 0, "B-frame video must carry DTS");
}

/// An audio-only program (AC-3): the PCR falls to the audio PID.
#[tokio::test(start_paused = true)]
async fn audio_only_export() {
	let ts = reexport(include_bytes!("test_data/ac3.ts")).await;
	let Some(report) = validate("ac3", &ts) else { return };
	assert_eq!(pids_carrying(&report, "video"), 0, "no video PID");
	assert_eq!(pids_carrying(&report, "audio"), 1, "one AC-3 PID");
}

/// A program carrying a SCTE-35 cue track alongside real A/V: the section PID
/// must be PMT-referenced and clean like every other PID.
#[tokio::test(start_paused = true)]
async fn scte35_export() {
	const CUE_PID: u16 = 0x102;

	let mut broadcast = moq_net::Broadcast::new().produce();
	let consumer = broadcast.consume();
	let mut catalog =
		crate::catalog::Producer::with_catalog(&mut broadcast, crate::catalog::hang::Catalog::<tscat::Ext>::default())
			.unwrap();

	// Create and write the cue track BEFORE moving `broadcast` into `Import`;
	// the producer stays alive so the exporter can subscribe to the retained track.
	let scte = broadcast.unique_track(".scte35").unwrap();
	let scte_name = scte.name().to_string();
	{
		let track = tscat::Track {
			pid: CUE_PID,
			descriptors: Vec::new(),
			verbatim: Some(tscat::Verbatim::new(0x86, tscat::Framing::Section)),
		};
		catalog.lock().mpegts.tracks.insert(scte_name, track);
	}
	let mut scte_producer = Producer::new(scte, HangContainer::Legacy);
	// bbb's first video keyframe is at 1.4 s; stamp the cue just after it so it
	// survives the tune-in alignment.
	scte_producer
		.write(Frame {
			timestamp: Timestamp::from_millis(1410).unwrap(),
			duration: None,
			payload: Bytes::from_static(CUE),
			keyframe: true,
		})
		.unwrap();
	scte_producer.finish_group().unwrap();
	scte_producer.finish().unwrap();

	let data = include_bytes!("test_data/bbb.ts");
	let mut import = Import::new(broadcast, catalog.clone());
	import.decode(&BytesMut::from(&data[..])).unwrap();
	import.finish().unwrap();

	// `import`, `catalog`, and `scte_producer` stay alive: retained tracks. The
	// exporter must carry the extension to see the mpegts section.
	let ts = drain(Export::with_ts(consumer, crate::catalog::CatalogFormat::Hang).unwrap()).await;
	let Some(report) = validate("scte35", &ts) else { return };
	assert!(
		report["pids"]
			.as_array()
			.expect("pids array")
			.iter()
			.any(|p| p["id"] == CUE_PID),
		"cue PID missing from the analysis"
	);
}
