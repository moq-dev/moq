import { expect, test } from "bun:test";
import { Effect, Signal } from "@moq/signals";
import type MoqWatch from "../../element";
import { centerPlay } from "./center-play";

test("hides the play button while the catalog needs a newer player", async () => {
	// Effects run asynchronously, so each write to `style.display` resolves the pending wait.
	let wake = () => {};
	const written = () => new Promise<void>((resolve) => (wake = resolve));
	let display: string | undefined;
	const style = {
		get display() {
			return display;
		},
		set display(value) {
			display = value;
			wake();
		},
	};

	// The only DOM the component touches: a button holding an icon span.
	const original = globalThis.document;
	globalThis.document = {
		createElement: () => ({
			style,
			setAttribute: () => {},
			replaceChildren: () => {},
			addEventListener: () => {},
			removeEventListener: () => {},
		}),
	} as unknown as Document;

	const status = new Signal<"offline" | "loading" | "live" | "error" | "unsupported">("live");
	const watch = {
		controls: { paused: new Signal(true) },
		video: { source: { out: { error: new Signal(undefined) } } },
		broadcast: { out: { error: new Signal(undefined), status } },
	} as unknown as MoqWatch;

	const effect = new Effect();
	try {
		let next = written();
		centerPlay(effect, watch);
		await next;
		expect(display).toBe("");

		next = written();
		status.set("unsupported");
		await next;
		expect(display).toBe("none");

		next = written();
		status.set("live");
		await next;
		expect(display).toBe("");
	} finally {
		effect.close();
		globalThis.document = original;
	}
});
