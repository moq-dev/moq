/** Reduce push-to-talk evidence without substituting missing observations with zero. @module */
import { join } from "node:path";
import type { measurement } from "./src/ptt";

// The driver's final write adds the page's state to the result.
type Result = ReturnType<typeof measurement.result> & { state?: ReturnType<typeof measurement.state> };
const [directory, ...rest] = process.argv.slice(2);
if (!directory || rest.length) throw new Error("usage: ptt-analyze.ts RUN_DIR");
const result: Result = await Bun.file(join(directory, "presses.json")).json();
if (result.presses.length !== 44 || result.presses.some((press, i) => press.on !== (i % 2 === 0))) {
	throw new Error("expected 22 complete presses");
}
if (result.captureError || result.errors.length) throw new Error("invalid capture");
const encoded = new Map(result.encoded.map((frame) => [frame.timestamp, frame]));
const sent = new Map(result.sent.map((frame) => [frame.timestamp, frame]));
const arrivals = new Map(result.arrivals.map((frame) => [Math.round(frame[1] * 1000), frame]));
// The first decode of each timestamp; a later one would be a re-decode, not the frame's first.
const decoded = new Map<number, (typeof result.decoded)[number]>();
for (const frame of result.decoded) if (!decoded.has(frame.timestamp)) decoded.set(frame.timestamp, frame);
const presses = result.presses
	.filter((press) => press.on)
	.map((press, i) => {
		const release = result.presses[i * 2 + 1];
		if (!release || release.on) throw new Error(`press ${i + 1} has no release`);
		const input = result.submitted.filter((frame) => frame.at >= press.at && frame.at < release.at);
		const output = input.flatMap((frame) => encoded.get(frame.timestamp) ?? []);
		const published = output.flatMap((frame) => sent.get(frame.timestamp) ?? []);
		const received = published.flatMap((frame) => {
			const received = arrivals.get(frame.timestamp);
			return received ? [received] : [];
		});
		const earliest = received.toSorted((a, b) => a[0] - b[0])[0];
		// An onset during a zero-gap press may belong to its still-playing predecessor.
		// A constant tone has no utterance ID, so only separated presses have identifiable onsets.
		const separated = i === 0 || i >= 8;
		const audible = separated
			? result.audible.find(
					(frame) => frame.at >= press.at && frame.at < (result.presses[(i + 1) * 2]?.at ?? Infinity),
				)
			: undefined;
		// One-way delivery is the slowest frame the start waited on, measured from its first sample's
		// capture: it covers packetization, transport, queueing and retransmission. What remains of the
		// start is the player's own added latency; projected device output is reported apart from it.
		const head = input[0]?.timestamp;
		const waited = input.flatMap((frame) => {
			const arrival = arrivals.get(frame.timestamp);
			if (!audible || head === undefined || !arrival || arrival[0] > audible.rendered) return [];
			return [arrival[0] - press.at - (frame.timestamp - head) / 1000];
		});
		const delivery = waited.length ? Math.max(...waited) : null;
		// The head frame's path from the press to projected output, one boundary after another, so
		// the parts sum to `audible`. Capture and packetization split at the head frame's end on the
		// media clock, placed against the newest captured chunk delivered before the press.
		const first = input[0];
		const path = (() => {
			if (!audible || !first || !press.capture) return null;
			const write = sent.get(first.timestamp);
			const arrival = arrivals.get(first.timestamp);
			const output = decoded.get(first.timestamp);
			// The first PCM that reached the delay from the head: the ring leaves its stall on it, and a
			// later frame decoded before the onset only adds to a ring already playing.
			const delay = result.state?.delay;
			const filled =
				delay === undefined
					? undefined
					: input
							.flatMap((frame) => decoded.get(frame.timestamp) ?? [])
							.toSorted((a, b) => a.at - b.at)
							.find((frame) => frame.timestamp + (frame.duration ?? 0) - first.timestamp >= delay * 1000);
			if (!write || !arrival || !output || !filled) return null;
			const pressed = press.capture.end / 1000 + (press.at - press.capture.at);
			const packetization = (first.timestamp + (first.duration ?? 0)) / 1000 - pressed;
			return {
				capture: first.at - press.at - packetization,
				packetization,
				encode: write.at - first.at,
				network: arrival[0] - write.at,
				decode: output.at - arrival[0],
				jitter_buffer: filled.at - output.at,
				render: audible.rendered - filled.at,
				device: audible.at - audible.rendered,
			};
		})();
		return {
			press: i + 1,
			gap: result.presses[i * 2 - 1] ? press.at - result.presses[i * 2 - 1].at : null,
			duration: release.at - press.at,
			submitted: input.length,
			encoded: output.length,
			sent: published.length,
			received: received.length,
			firstEncoded: input[0] ? encoded.has(input[0].timestamp) : false,
			firstReceived: input[0] ? arrivals.has(input[0].timestamp) : false,
			firstDecoded: input[0] ? decoded.has(input[0].timestamp) : false,
			arrival: earliest ? earliest[0] - press.at : null,
			audible: audible ? audible.at - press.at : null,
			delivery,
			playout: audible && delivery !== null ? audible.rendered - press.at - delivery : null,
			output: audible ? audible.at - audible.rendered : null,
			path,
		};
	});
const spread = (values: (number | null)[]) => {
	const sorted = values.flatMap((value) => (value === null ? [] : [value])).toSorted((a, b) => a - b);
	if (!sorted.length) return null;
	const middle = sorted.length / 2;
	const median = sorted.length % 2 ? sorted[Math.floor(middle)] : (sorted[middle - 1] + sorted[middle]) / 2;
	return { min: sorted[0], median, max: sorted[sorted.length - 1] };
};
const parts = ["capture", "packetization", "encode", "network", "decode", "jitter_buffer", "render", "device"] as const;
const summary = {
	// The goal: at most 300 ms of playout added on top of one-way delivery.
	latency: {
		delivery: spread(presses.map((press) => press.delivery)),
		playout: spread(presses.map((press) => press.playout)),
		output: spread(presses.map((press) => press.output)),
	},
	path: Object.fromEntries(parts.map((part) => [part, spread(presses.map((press) => press.path?.[part] ?? null))])),
	delay: result.state?.delay,
	presses,
	captures: result.captures,
	encoders: result.encoders,
	subscriptions: result.subscriptions,
	unencoded: result.submitted.filter((frame) => !encoded.has(frame.timestamp)).length,
	unsent: result.encoded.filter((frame) => !sent.has(frame.timestamp)).length,
	// An ideal path loses nothing, so every written frame must reach the listener.
	unreceived: result.sent.filter((frame) => !arrivals.has(frame.timestamp)).length,
};
await Bun.write(join(directory, "summary.json"), JSON.stringify(summary, null, 2));
// The report: the latency and its parts, and the completeness the run gates on. Per-press detail
// stays in summary.json.
const { latency, path, delay, unencoded, unsent, unreceived } = summary;
console.log(JSON.stringify({ latency, path, delay, unencoded, unsent, unreceived }, null, 2));

const failures: string[] = [];
if (presses.some((press, i) => (i === 0 || i >= 8) && press.audible === null)) {
	failures.push("a separated utterance never reached the output");
}
if (result.captures.length) failures.push("capture reopened");
if (result.encoders.length) failures.push("an encoder was created again");
if (result.subscriptions.length) failures.push("subscription reopened");
if (presses.some((press) => !press.submitted || !press.firstEncoded)) failures.push("utterance head was not encoded");
if (summary.unencoded || summary.unsent) failures.push("submitted audio was not encoded and sent completely");
if (presses.some((press) => !press.firstReceived)) failures.push("utterance head was not received");
if (presses.some((press) => press.firstReceived && !press.firstDecoded))
	failures.push("utterance head was not decoded");
if (summary.unreceived) failures.push("written audio did not all reach the listener");
if (failures.length) {
	console.error(failures.join("; "));
	process.exitCode = 1;
}
