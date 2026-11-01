use super::*;
use crate::subscribe::{SelectArgs, Subscribe, SubscribeArgs, SubscribeFormat};
use tokio::io::{AsyncBufReadExt, BufReader};

const CLIP: &[u8] = include_bytes!("../../../moq-mux/src/container/ts/test_data/bbb5s_cbr.ts");
const HALF: usize = CLIP.len() / 2;

#[derive(serde::Deserialize)]
struct Feedback {
	broadcast: String,
	feedback: hang::echo::Snapshot<ts::stats::Report>,
}

/// Discovery, live feedback, and final TS counters share a paused clock and in-memory origin.
#[tokio::test(start_paused = true)]
async fn export_echo_reports_where_the_import_solicits() {
	let (expected, _) = written(CLIP);
	let origin = moq_tokio::origin::spawn();
	let publication = origin.create_broadcast("demo").unwrap();
	let mut broadcast = publication.broadcast().clone();
	let mut catalog_value = moq_mux::catalog::hang::Catalog::<ts::Ext>::default();
	catalog_value.echo = Some(hang::catalog::Echo::new(
		hang::moq_net::path::Relative::new("viewers").unwrap(),
	));
	let config = moq_mux::catalog::Config::default()
		.with_catalog(catalog_value)
		.with_max_age(Duration::from_secs(1));
	let mut catalog = moq_mux::catalog::Producer::new(&mut broadcast, config).unwrap();
	let mut import = ts::Import::new(broadcast, catalog.reserve());
	import.decode(&CLIP[..HALF]).unwrap();
	publication.announce(Default::default()).unwrap();
	let consumer = origin.consume();
	let resolved = consumer.follow("demo", None).unwrap().available().await.unwrap();

	let args = SubscribeArgs {
		format: SubscribeFormat::Ts,
		max_delay: Duration::from_secs(4),
		follow: Duration::ZERO,
		stitch: false,
		fragment_duration: None,
		mux_rate: None,
		passthrough: false,
		catalog: None,
		select: SelectArgs::default(),
	};
	let subscribe = Subscribe::new(moq_mux::Source::new(consumer, resolved), CatalogFormat::Hang, args);
	let target = EchoTarget {
		origin: origin.clone(),
		name: "alice".parse().unwrap(),
		connection: None,
	};
	let mut echo = Echo::new(target, subscribe.source().clone(), CatalogFormat::Hang);
	let source = subscribe.source().clone();
	let sections = echo.sections();
	let subscribe = subscribe.with_sections(sections.clone());
	let mut exporting = tokio::spawn(async move {
		tokio::select! {
			result = subscribe.write_ts(tokio::io::sink()) => result.unwrap(),
			never = echo.run() => match never {},
		}
		let mut final_report = hang::echo::Snapshot::default();
		final_report.renditions = source.stats();
		final_report.ext = sections.get();
		echo.finish();
		// Compare the serialized form, including its millisecond precision.
		serde_json::to_value(final_report).unwrap()
	});
	let (out, read) = tokio::io::duplex(64 * 1024);
	let sink = tokio::spawn(write_echo(origin.consume(), "viewers".into(), out));
	let mut lines = BufReader::new(read).lines();
	let mut feedback = Vec::<Feedback>::new();

	// A report with media counters is the readiness event for the rest of the feed.
	loop {
		let line = lines.next_line().await.unwrap().expect("feedback mid-export");
		let line: Feedback = serde_json::from_str(&line).unwrap();
		let ready = line.feedback.renditions.values().any(|read| read.frames > 0);
		feedback.push(line);
		if ready {
			break;
		}
	}
	import.decode(&CLIP[HALF..]).unwrap();
	import.finish().unwrap();
	catalog.finish().unwrap();

	let final_report = loop {
		tokio::select! {
			result = &mut exporting => break result.unwrap(),
			line = lines.next_line() => {
				let line = line.unwrap().expect("feedback while exporting");
				feedback.push(serde_json::from_str(&line).unwrap());
			}
		}
	};
	let final_report: hang::echo::Snapshot<ts::stats::Report> = serde_json::from_value(final_report).unwrap();
	// It may have arrived while the export was still running.
	while feedback.last().is_none_or(|line| line.feedback != final_report) {
		let line = kio::clock::timeout(Duration::from_secs(30), lines.next_line())
			.await
			.expect("the final feedback never arrived")
			.unwrap()
			.expect("the echo sink stays up");
		feedback.push(serde_json::from_str(&line).unwrap());
	}
	sink.abort();
	assert!(sink.await.unwrap_err().is_cancelled());

	assert!(
		feedback.iter().all(|line| line.broadcast == "viewers/alice.echo"),
		"the feedback is published at the solicited path"
	);
	let first = &feedback.first().expect("the sink printed feedback").feedback;
	let last = &feedback.last().unwrap().feedback;
	assert!(!last.renditions.is_empty(), "the export reports what it reads");
	for (name, read) in &last.renditions {
		assert!(
			read.frames > 0 && read.frames <= expected[name].frames,
			"{name}: read {} of {} frames",
			read.frames,
			expected[name].frames
		);
		let newest = read.arrival.as_ref().expect("an arrival").timestamp;
		let earliest = first.renditions.get(name).and_then(|read| read.arrival.as_ref());
		assert!(
			earliest.is_none_or(|earliest| earliest.timestamp < newest),
			"{name}: the newest arrival advances over {} lines: {earliest:?} then {:?}",
			feedback.len(),
			read.arrival
		);
	}

	let rows = last.ext.mpegts.as_ref().expect("a TS export reports its rows");
	assert_clean(rows);
	let first = feedback
		.iter()
		.find_map(|line| line.feedback.ext.mpegts.as_ref())
		.unwrap();
	assert_advancing(first, rows);
	let release = last
		.ext
		.release
		.as_ref()
		.expect("a TS export reports its release clock");
	assert!(
		release.margin.is_some(),
		"a second of frames gives a margin: {release:?}"
	);
}

/// What a TS import of `clip` writes to each rendition, and its MPEG-TS counters, from an
/// in-process import.
fn written(clip: &[u8]) -> (BTreeMap<String, hang::stats::Rendition>, ts::stats::Snapshot) {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let config =
		moq_mux::catalog::Config::default().with_catalog(moq_mux::catalog::hang::Catalog::<ts::Ext>::default());
	let catalog = moq_mux::catalog::Producer::new(&mut broadcast, config).unwrap();
	let mut import = ts::Import::new(broadcast, catalog.reserve());
	import.decode(clip).unwrap();
	import.finish().unwrap();
	(catalog.stats(), import.stats())
}

/// Each elementary stream's access units, by PID.
fn units(mpegts: &ts::stats::Snapshot) -> BTreeMap<u16, u64> {
	mpegts.streams.iter().map(|(pid, row)| (*pid, row.units)).collect()
}

/// Every TR 101 290 counter, PSI CRC error, and per-PID loss reads zero.
fn assert_clean(mpegts: &ts::stats::Snapshot) {
	let mut stream = mpegts.clone();
	stream.streams.clear();
	assert_eq!(
		stream,
		ts::stats::Snapshot::default(),
		"a stream-wide error was counted"
	);
	for (pid, row) in &mpegts.streams {
		let lost = [
			row.resyncs,
			row.discarded,
			row.unconfirmed,
			row.damaged,
			row.continuity_count_error,
			row.transport_error,
			row.pts_error,
		];
		assert_eq!(lost, [0; 7], "PID {pid} counted a loss: {row:?}");
	}
}

/// Each PID's access units in `first` fell short of `last`'s: the rows advance while it runs.
/// An SI row counts only the sections the program clock could not place, so it has none.
fn assert_advancing(first: &ts::stats::Snapshot, last: &ts::stats::Snapshot) {
	assert!(!last.streams.is_empty(), "no elementary stream reported");
	let first = units(first);
	for (pid, units) in units(last) {
		if last.streams[&pid].track == ".si" {
			continue;
		}
		let before = first.get(&pid).copied().unwrap_or(0);
		assert!(before < units, "PID {pid}: {before} access units, then {units}");
	}
}
