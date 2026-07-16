import { expect, test } from "bun:test";
import { Broadcast } from "../broadcast.ts";
import { Encoder } from "./index.ts";

// Registration runs inside an effect, which settles over a few microtasks.
const flush = () => new Promise<void>((resolve) => queueMicrotask(resolve));
async function settle(times = 5): Promise<void> {
	for (let i = 0; i < times; i++) await flush();
}

test("an encoder registers its rendition on the broadcast", async () => {
	const broadcast = new Broadcast({ enabled: true });
	const encoder = new Encoder("video/hd", { broadcast });
	await settle();

	// Registered: the name is now taken.
	expect(() => broadcast.video("video/hd")).toThrow();

	encoder.close();
	await settle();

	// Freed on close, so the name can be reused.
	expect(() => broadcast.video("video/hd")).not.toThrow();

	broadcast.close();
});

test("an encoder without a broadcast has nothing to publish", async () => {
	const encoder = new Encoder("video/hd");
	await settle();

	// No broadcast, no subscriber: never active.
	expect(encoder.out.active.peek()).toBe(false);

	encoder.close();
});
