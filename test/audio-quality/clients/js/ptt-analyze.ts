/** Reduce push-to-talk evidence without substituting missing observations with zero. @module */
import { join } from "node:path";
import type { measurement } from "./src/ptt";

type Result = ReturnType<typeof measurement.result>;
const [directory, diagnostic] = process.argv.slice(2);
if (!directory || (diagnostic !== undefined && diagnostic !== "--diagnostic"))
	throw new Error("usage: ptt-analyze.ts RUN_DIR [--diagnostic]");
const result: Result = await Bun.file(join(directory, "presses.json")).json();
if (result.presses.length !== 44 || result.presses.some((press, i) => press.on !== (i % 2 === 0))) {
	throw new Error("expected 22 complete presses");
}
if (result.captureError || result.errors.length) throw new Error("invalid capture");
const encoded = new Map(result.encoded.map((frame) => [frame.timestamp, frame]));
const sent = new Map(result.sent.map((frame) => [frame.timestamp, frame]));
const arrivals = new Map(result.arrivals.map((frame) => [Math.round(frame[1] * 1000), frame]));
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
		const first = received.toSorted((a, b) => a[0] - b[0])[0];
		// An onset during a zero-gap press may belong to its still-playing predecessor.
		// A constant tone has no utterance ID, so only separated presses have identifiable onsets.
		const separated = i === 0 || i >= 8;
		const audible = separated
			? result.audible.find(
					(frame) => frame.at >= press.at && frame.at < (result.presses[(i + 1) * 2]?.at ?? Infinity),
				)
			: undefined;
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
			arrival: first ? first[0] - press.at : null,
			audible: audible ? audible.at - press.at : null,
		};
	});
const activeDuration = presses.reduce((total, press) => total + press.duration, 0);
const offered = result.sent.reduce((total, frame) => total + (frame.bytes ?? 0) * 8, 0) / (activeDuration / 1000);
const summary = {
	offered,
	presses,
	captures: result.captures,
	probes: result.probes,
	subscriptions: result.subscriptions,
	unencoded: result.submitted.filter((frame) => !encoded.has(frame.timestamp)).length,
	unsent: result.encoded.filter((frame) => !sent.has(frame.timestamp)).length,
};
await Bun.write(join(directory, "summary.json"), JSON.stringify(summary, null, 2));
console.log(JSON.stringify(summary, null, 2));

const failures: string[] = [];
if (presses.some((press, i) => (i === 0 || i >= 8) && press.audible === null)) {
	failures.push("a separated utterance never reached the output");
}
if (result.captures.length) failures.push("capture reopened");
if (result.probes.length) failures.push("encoder probed again");
if (result.subscriptions.length) failures.push("subscription reopened");
if (presses.some((press) => !press.submitted || !press.firstEncoded)) failures.push("utterance head was not encoded");
if (summary.unencoded || summary.unsent) failures.push("submitted audio was not encoded and sent completely");
const shaper = await Bun.file(join(directory, "shaper.json")).json();
if (!diagnostic && offered <= 64_000) failures.push("the bottleneck was not below the media bitrate");
if (!diagnostic && (!shaper.down.throttled || !shaper.down.overflowed || shaper.down.lost)) {
	failures.push("bufferbloat did not act with queue overflow and no random loss");
}
if (failures.length) {
	console.error(failures.join("; "));
	if (!diagnostic) process.exitCode = 1;
}
