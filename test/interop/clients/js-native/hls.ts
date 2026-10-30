/**
 * HLS record interop publisher: publish the schedule `interop-hls publish` does through
 * `@moq/hang`'s recorder, finish every track, and stay up until the harness acknowledges. The
 * harness reads both publishers' records with `interop-hls read` and requires them identical.
 *
 *     node --import tsx hls.ts URL BROADCAST
 *
 * Keep the schedule in step with `test/interop/clients/rust/hls.rs`.
 *
 * @module
 */
import { strict as assert } from "node:assert";
import { createInterface } from "node:readline";
import { Micro } from "@moq/clock";
import * as Catalog from "@moq/hang/catalog";
import * as Container from "@moq/hang/container";
import * as Hls from "@moq/hang/hls";
import * as Json from "@moq/json";
import * as Moq from "@moq/net";
import { install } from "@moq/web-transport";

install();
console.debug = console.error;
console.info = console.error;
const [url, path] = process.argv.slice(2);
if (!url || !path) throw new Error("usage: hls.ts URL BROADCAST");

/** One frame: its pts in milliseconds and whether it opens a group. */
type Frame = { ms: number; key: boolean };

/** Frames every `step` ms over `[start, end)`, opening a group every `gop` ms. */
function frames(start: number, end: number, step: number, gop: number): Frame[] {
	const out: Frame[] = [];
	for (let ms = start; ms < end; ms += step) out.push({ ms, key: (ms - start) % gop === 0 });
	return out;
}

// Video: 2s GOPs, a pause, then 3s GOPs off the grid. Audio: 100ms groups of 20ms frames, a
// pause, then more. Each part ends in a discontinuity but the last, which the finish ends.
const VIDEO = [frames(0, 6000, 100, 2000), frames(20000, 30000, 100, 3000)];
const AUDIO = [frames(0, 9000, 20, 100), frames(15000, 17000, 20, 100)];

const origin = new Moq.Origin.Producer();
const connection = await Moq.Connection.connect({
	url: new URL(url),
	websocket: { enabled: false },
	publish: origin.consume(),
});
try {
	const publication = origin.createBroadcast(Moq.Path.from(path));
	const broadcast = publication.broadcast;
	const renditions: { video: Record<string, Catalog.VideoConfig>; audio: Record<string, Catalog.AudioConfig> } = {
		video: {},
		audio: {},
	};
	const publish = (kind: "video" | "audio", parts: Frame[][]) => {
		const media = broadcast.createTrack(kind, Container.trackInfo({ priority: Catalog.PRIORITY[kind] }));
		const recorder = new Hls.Recorder(broadcast.createTrack(Hls.defaultName(kind), Hls.Recorder.INFO));
		const producer = new Container.Legacy.Producer(media, new Container.Legacy.Format(kind), recorder);
		for (const [index, part] of parts.entries()) {
			if (index > 0) producer.discontinuity();
			for (const { ms, key } of part) producer.encode(new Uint8Array([1]), Micro(ms * 1000), key);
		}
		producer.close();
		const hls = recorder.hls.peek();
		assert(hls, `${kind} declared no HLS duration`);
		if (kind === "video") renditions.video[kind] = { codec: "avc1.64001f", container: { kind: "legacy" }, hls };
		else
			renditions.audio[kind] = {
				codec: "opus",
				sampleRate: Catalog.u53(48000),
				numberOfChannels: Catalog.u53(2),
				container: { kind: "legacy" },
				hls,
			};
	};
	publish("video", VIDEO);
	publish("audio", AUDIO);

	const track = broadcast.createTrack(Catalog.TRACK, {
		timescale: Moq.Time.Timescale.MILLI,
		priority: Catalog.PRIORITY.catalog,
		compress: true,
	});
	const catalog = new Json.Snapshot.Producer<Catalog.Root>({ track, deltas: false });
	catalog.update({
		value: Catalog.encode({
			video: { renditions: renditions.video },
			audio: { renditions: renditions.audio },
		}),
		at: Moq.Time.Timestamp.now(),
	});
	catalog.finish();
	publication.announce();
	console.log("hls ready");

	const lines = createInterface({ input: process.stdin });
	try {
		const ack = await lines[Symbol.asyncIterator]().next();
		assert.equal(ack.value, "clean end", "reader did not acknowledge");
	} finally {
		lines.close();
	}
	broadcast.close();
	console.log("hls acknowledged");
} finally {
	connection.close();
	origin.close();
}
process.exit(0);
