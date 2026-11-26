import { expect, test } from "bun:test";
import { Duration, yieldNow } from "@moq/clock";
import { type KeepAlive, start } from "./keep_alive.ts";

const CONFIG: KeepAlive = { soft: Duration.secs(2), hard: Duration.secs(10) };

/** A qmux session's keep-alive surface, flipped by hand. */
class FakeProbing {
	stale = false;
	config?: KeepAlive;
	#changed = Promise.withResolvers<void>();
	#closed = Promise.withResolvers<void>();
	readonly closed = this.#closed.promise;

	keepAlive(config: KeepAlive) {
		this.config = config;
	}

	staleChanged(): Promise<void> {
		return this.#changed.promise;
	}

	flip(stale: boolean) {
		this.stale = stale;
		this.#notify();
	}

	close() {
		this.#closed.resolve();
		this.#notify();
	}

	#notify() {
		const changed = this.#changed;
		this.#changed = Promise.withResolvers();
		changed.resolve();
	}
}

const turn = yieldNow;

test("liveness follows the transport's stale signal until it closes", async () => {
	const probing = new FakeProbing();
	const liveness = start(probing as unknown as WebTransport, CONFIG);
	expect(probing.config).toBe(CONFIG);
	expect(liveness.peek()).toBe("live");

	probing.flip(true);
	await turn();
	expect(liveness.peek()).toBe("stale");
	probing.flip(false);
	await turn();
	expect(liveness.peek()).toBe("live");

	probing.close();
	await turn();
	probing.flip(true);
	await turn();
	expect(liveness.peek()).toBe("live");
});

test("a transport that cannot probe its peer refuses the keep-alive", () => {
	const webtransport = { closed: new Promise(() => {}) } as unknown as WebTransport;
	expect(() => start(webtransport, CONFIG)).toThrow("WebTransport cannot");
});
