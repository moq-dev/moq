/** Compare the strict frame parse against bare `JSON.parse`, over catalog and stats frames. */
import { parse } from "../src/parse.ts";

// A catalog with a few renditions, nested a couple of levels deep.
const catalog = {
	video: {
		renditions: Object.fromEntries(
			["1080p", "720p", "480p", "360p"].map((name, i) => [
				name,
				{
					codec: "avc1.64001f",
					description: "0164001fffe100196764001fac2484014016ec0440000003004000000c23c60c92",
					codedWidth: 1920 >> i,
					codedHeight: 1080 >> i,
					framerate: 30,
					bitrate: 6_000_000 >> i,
					container: { kind: "legacy" },
				},
			]),
		),
	},
	audio: {
		renditions: { opus: { codec: "opus", sampleRate: 48000, numberOfChannels: 2, container: { kind: "legacy" } } },
	},
	stats: { track: "stats.json" },
};

// A moq-stats traffic table: one row of flat counters per path.
function stats(rows: number) {
	return Object.fromEntries(
		Array.from({ length: rows }, (_, i) => [
			`anon/live-stream-${i}`,
			{
				announces_started: 1,
				broadcasts_started: 3 + (i % 7),
				subscriptions_started: 9 + (i % 21),
				bytes: 62_500 * (1_000 + i * 37),
				frames: 80 * (1_000 + i * 37),
				groups: 2 * (1_000 + i * 37),
			},
		]),
	);
}

const decoder = new TextDecoder();
const cases: [string, Uint8Array][] = [
	["catalog", new TextEncoder().encode(JSON.stringify(catalog))],
	...[16, 1024, 65536].map((length): [string, Uint8Array] => [
		`unicode-${length}`,
		new TextEncoder().encode(JSON.stringify({ ["é🌍".repeat(length)]: "é🌍".repeat(length) })),
	]),
	...[10, 100, 1000].map((rows): [string, Uint8Array] => [
		`stats-${rows}`,
		new TextEncoder().encode(JSON.stringify(stats(rows))),
	]),
];

function time(bytes: Uint8Array, decode: (bytes: Uint8Array) => unknown): number {
	const iterations = Math.max(20, Math.floor(2_000_000 / bytes.length));
	const samples: number[] = [];
	for (let sample = 0; sample < 5; sample++) {
		const start = performance.now();
		for (let n = 0; n < iterations; n++) decode(bytes);
		samples.push(((performance.now() - start) / iterations) * 1000);
	}
	samples.sort((a, b) => a - b);
	return samples[2] ?? 0;
}

console.log("frame,bytes,json_parse_us,parse_us");
for (const [name, bytes] of cases) {
	const baseline = time(bytes, (bytes) => JSON.parse(decoder.decode(bytes)));
	const strict = time(bytes, parse);
	console.log(`${name},${bytes.length},${baseline.toFixed(2)},${strict.toFixed(2)}`);
}
