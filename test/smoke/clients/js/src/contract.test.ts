import { expect, test } from "bun:test";
import { leakedPlayerStarted, type Resources } from "./contract";

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
