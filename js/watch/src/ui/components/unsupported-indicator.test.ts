import { expect, test } from "bun:test";
import { Effect, Signal } from "@moq/signals";
import type MoqWatch from "../../element";
import { unsupportedIndicator } from "./unsupported-indicator";

const unsupported = () => new DOMException("no supported renditions", "NotSupportedError");

test("reports unsupported audio only when the broadcast has no video", async () => {
	// The only DOM the component touches: a container holding a text span.
	const container = { style: { display: "" }, setAttribute: () => {}, appendChild: () => {} };
	const text = { textContent: "" };
	const original = globalThis.document;
	let created = 0;
	globalThis.document = {
		createElement: () => (created++ === 0 ? container : text),
	} as unknown as Document;

	const video = {
		catalog: new Signal<{ renditions: Record<string, { codec: string }> } | undefined>(undefined),
		error: new Signal<Error | undefined>(undefined),
	};
	const audio = {
		catalog: new Signal({ renditions: { mic: { codec: "opus" } } }),
		error: new Signal<Error | undefined>(undefined),
	};
	const watch = {
		broadcast: { out: { status: new Signal("live") } },
		video: { source: { out: video } },
		audio: { source: { out: audio } },
	} as unknown as MoqWatch;

	const effect = new Effect();
	const tick = () => new Promise((resolve) => setTimeout(resolve, 0));
	try {
		unsupportedIndicator(effect, watch);
		await tick();
		expect(container.style.display).toBe("none");

		audio.error.set(unsupported());
		await tick();
		expect(container.style.display).toBe("");
		expect(text.textContent).toBe("This audio codec is not supported by your browser: opus");

		// Playable video carries the broadcast, so unsupported audio alone is not reported.
		video.catalog.set({ renditions: { hd: { codec: "avc1.640028" } } });
		await tick();
		expect(container.style.display).toBe("none");

		video.error.set(unsupported());
		await tick();
		expect(container.style.display).toBe("");
		expect(text.textContent).toBe("This video codec is not supported by your browser: avc1.640028");
	} finally {
		effect.close();
		globalThis.document = original;
	}
});
