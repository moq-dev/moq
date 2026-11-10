import { expect, test } from "bun:test";
import { Effect, Signal } from "@moq/signals";
import * as Announce from "../../announce";
import type MoqPublish from "../../element";
import { statusBadge } from "./status-badge";

// Replays LiveMeansWatchable in `verify/Publish.tla`: the badge reads the connection and the
// captured sources, never the announce gate, so it says LIVE while `source` mode still holds the
// announce for the encoders to settle and no viewer can find the broadcast.
test("residual: the badge says LIVE before the broadcast is announced", async () => {
	// The only DOM the component touches.
	const made: { className: string; textContent: string; dataset: Record<string, string> }[] = [];
	const original = globalThis.document;
	globalThis.document = {
		createElement: () => {
			const element = { className: "", textContent: "", dataset: {}, append: () => {} };
			made.push(element);
			return element;
		},
	} as unknown as Document;

	const video = {};
	const audio = {};
	const capture = (source: object) => new Signal({ in: { source: new Signal(source) } });
	const publish = {
		connection: { url: new Signal(new URL("https://relay.example/")), status: new Signal("connected") },
		video: { in: { capture: capture(video) } },
		audio: { in: { capture: capture(audio) } },
		controls: { muted: new Signal(false), invisible: new Signal(false) },
		sources: { video: new Signal(undefined), audio: new Signal(undefined) },
	} as unknown as MoqPublish;

	const effect = new Effect();
	try {
		// You picked the camera, the connection opened, and you allowed the camera; the encoders
		// have not settled yet.
		const announcing = new Signal(false);
		const track = (source: object) => ({
			enabled: new Signal(true),
			source: new Signal(source),
			settled: new Signal(false),
		});
		Announce.run(effect, announcing, {
			mode: new Signal("source" as const),
			source: new Signal("camera" as const),
			video: track(video),
			audio: track(audio),
		});
		statusBadge(effect, publish);
		for (let i = 0; i < 5; i++) await Promise.resolve();

		const text = made.find((element) => element.className === "badge-text");
		expect(text?.textContent).toBe("LIVE");
		expect(announcing.peek()).toBe(false);
	} finally {
		effect.close();
		globalThis.document = original;
	}
});
