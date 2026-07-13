import { expect, test } from "bun:test";
import { Reload } from "./reload.ts";

async function settle() {
	await new Promise((resolve) => setTimeout(resolve, 0));
}

test("equivalent URL instances do not restart a pending connection", async () => {
	const original = globalThis.WebTransport;
	let connects = 0;

	class PendingWebTransport {
		ready = new Promise<void>(() => {});
		closed = new Promise<void>(() => {});

		constructor() {
			connects++;
		}

		close() {}
	}

	globalThis.WebTransport = PendingWebTransport as unknown as typeof WebTransport;
	const reload = new Reload({
		enabled: true,
		url: new URL("https://example.com/broadcast"),
		websocket: { enabled: false },
	});

	try {
		await settle();
		expect(connects).toBe(1);

		reload.url.set(new URL("https://example.com/broadcast"));
		await settle();
		expect(connects).toBe(1);

		reload.url.set(new URL("https://example.com/other"));
		await settle();
		expect(connects).toBe(2);
	} finally {
		reload.close();
		globalThis.WebTransport = original;
	}
});
