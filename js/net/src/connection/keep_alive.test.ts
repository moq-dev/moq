import { expect, test } from "bun:test";
import { Duration, yieldNow } from "@moq/clock";
import { type KeepAlive, start } from "./keep_alive.ts";

const CONFIG: KeepAlive = { soft: Duration.secs(2), hard: Duration.secs(10) };

/** A qmux session's keep-alive surface, flipped by hand. */
class FakeProbing {
	stale = false;
	config?: KeepAlive;
	#ready = Promise.withResolvers<void>();
	readonly ready = this.#ready.promise;
	#params = false;
	#changed = Promise.withResolvers<void>();
	#closed = Promise.withResolvers<void>();
	readonly closed = this.#closed.promise;

	constructor(ready = true) {
		if (ready) this.params();
	}

	/** The peer's transport parameters arrive, settling `ready`. */
	params() {
		this.#params = true;
		this.#ready.resolve();
	}

	// Like qmux, refused until the peer's parameters arrive.
	keepAlive(config: KeepAlive) {
		if (!this.#params) throw new Error("keep-alive needs the peer's transport parameters; await ready");
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
	const liveness = await start(probing as unknown as WebTransport, CONFIG);
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

test("a session already quiet past soft starts stale", async () => {
	const probing = new FakeProbing();
	// The transport marks a long-quiet session stale as soon as the keep-alive starts.
	probing.keepAlive = (config) => {
		probing.config = config;
		probing.stale = true;
	};
	expect((await start(probing as unknown as WebTransport, CONFIG)).peek()).toBe("stale");
});

test("a keep-alive started before the peer's parameters arrive waits for them", async () => {
	const probing = new FakeProbing(false);
	const started = start(probing as unknown as WebTransport, CONFIG);
	await turn();
	expect(probing.config).toBeUndefined();
	probing.params();
	expect((await started).peek()).toBe("live");
	expect(probing.config).toBe(CONFIG);
});

test("a transport that cannot probe its peer refuses the keep-alive", async () => {
	const webtransport = { closed: new Promise(() => {}) } as unknown as WebTransport;
	await expect(start(webtransport, CONFIG)).rejects.toThrow("WebTransport cannot");
});
