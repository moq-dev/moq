import { expect, test } from "bun:test";
import { leakedPlayerStarted, type Resources, reachesLive } from "./contract";

const playing: Resources = { transports: 1, sockets: 0, audioContexts: 1, workers: 0 };

test("a leaked player is visible when it reuses the pooled transport", () => {
	const leaked: Resources = { transports: 1, sockets: 0, audioContexts: 2, workers: 0 };
	expect(leakedPlayerStarted(playing, leaked)).toBe(true);
});

test("a leaked player is visible when it reuses a pooled websocket fallback", () => {
	const busy: Resources = { transports: 0, sockets: 1, audioContexts: 1, workers: 0 };
	const leaked: Resources = { transports: 0, sockets: 1, audioContexts: 2, workers: 0 };
	expect(leakedPlayerStarted(busy, leaked)).toBe(true);
});

test("unchanged counts are not a leak start", () => {
	expect(leakedPlayerStarted(playing, playing)).toBe(false);
});

const nearLive = {
	previous: 183,
	source: 196,
	bound: 45,
	sample: { frameId: 186, audioContext: "running", audioStalled: false },
};

test("an advancing preview while audio buffers is not live playback", () => {
	expect(reachesLive({ ...nearLive, sample: { ...nearLive.sample, audioStalled: true } })).toBe(false);
});

test("a suspended audio graph cannot establish live playback", () => {
	expect(reachesLive({ ...nearLive, sample: { ...nearLive.sample, audioContext: "suspended" } })).toBe(false);
});

test("live playback requires an advancing readable frame within the existing bound", () => {
	expect(reachesLive(nearLive)).toBe(true);
	expect(reachesLive({ ...nearLive, previous: undefined })).toBe(false);
	expect(reachesLive({ ...nearLive, previous: 186 })).toBe(false);
	expect(reachesLive({ ...nearLive, source: 232 })).toBe(false);
	expect(reachesLive({ ...nearLive, sample: { ...nearLive.sample, frameId: undefined } })).toBe(false);
});
