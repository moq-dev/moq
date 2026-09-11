import { afterEach, expect, test } from "bun:test";
import * as Lite from "../lite/index.ts";
import { createMockTransportPair } from "../mock.ts";
import { Producer as TrackProducer } from "../track.ts";
import { accept } from "./index.ts";
import { Connection, resetShared } from "./pool.ts";

const url = new URL("https://example.com/bandwidth");

async function settle() {
	await new Promise((resolve) => setTimeout(resolve, 0));
}

async function waitUntil(pred: () => boolean, ms = 1000): Promise<void> {
	const deadline = Date.now() + ms;
	for (;;) {
		if (pred()) return;
		if (Date.now() > deadline) throw new Error("timed out waiting for condition");
		await settle();
	}
}

const original = globalThis.WebTransport;

afterEach(() => {
	resetShared();
	globalThis.WebTransport = original;
});

function stubTransport(stats: { estimatedSendRate?: number }): void {
	const stub = function StubWebTransport() {
		const pair = createMockTransportPair(Lite.ALPN_06_WIP, { stats });
		void accept(pair.server, url);
		return pair.client;
	};
	globalThis.WebTransport = stub as unknown as typeof WebTransport;
}

test("a connection samples the send rate onto the allocator", async () => {
	stubTransport({ estimatedSendRate: 2_000_000 });

	const connection = new Connection({ url, linger: 20 });
	try {
		await waitUntil(() => connection.status.peek() === "connected");
		const allocator = connection.bandwidth.peek();
		if (!allocator) throw new Error("missing allocator");

		const track = new TrackProducer("video").accept({ priority: 60 });
		track.subscribe();
		const reserved = allocator.reserve(track, 4_000_000);

		await waitUntil(() => reserved.peek() === 2_000_000);
	} finally {
		connection.close();
	}
});

test("two publishers on one connection split the estimate by priority", async () => {
	stubTransport({ estimatedSendRate: 2_000_000 });

	const first = new Connection({ url, linger: 20 });
	const second = new Connection({ url });
	try {
		await waitUntil(() => first.status.peek() === "connected");
		await waitUntil(() => second.status.peek() === "connected");

		const allocator = first.bandwidth.peek();
		expect(allocator).toBe(second.bandwidth.peek());
		if (!allocator) throw new Error("missing allocator");

		const audio = new TrackProducer("audio").accept({ priority: 80 });
		audio.subscribe();
		const audioShare = allocator.reserve(audio, 128_000);

		const video = new TrackProducer("video").accept({ priority: 60 });
		video.subscribe();
		const videoShare = allocator.reserve(video, 4_000_000);

		await waitUntil(() => audioShare.peek() !== undefined && videoShare.peek() !== undefined);

		expect(audioShare.peek()).toBe(128_000);
		expect(videoShare.peek()).toBe(1_872_000);
		expect((audioShare.peek() ?? 0) + (videoShare.peek() ?? 0)).toBeLessThanOrEqual(2_000_000);
	} finally {
		first.close();
		second.close();
	}
});

test("an idle track on a live connection claims nothing", async () => {
	stubTransport({ estimatedSendRate: 2_000_000 });

	const handle = new Connection({ url, linger: 20 });
	try {
		await waitUntil(() => handle.status.peek() === "connected");
		const allocator = handle.bandwidth.peek();
		if (!allocator) throw new Error("missing allocator");

		const watched = new TrackProducer("watched").accept({ priority: 60 });
		watched.subscribe();
		const watchedShare = allocator.reserve(watched, 4_000_000);

		const idle = new TrackProducer("idle").accept({ priority: 60 });
		const idleShare = allocator.reserve(idle, 4_000_000);

		await waitUntil(() => watchedShare.peek() === 2_000_000);
		expect(idleShare.peek()).toBeUndefined();
	} finally {
		handle.close();
	}
});

test("a private connection still exposes an allocator", async () => {
	stubTransport({ estimatedSendRate: 2_000_000 });

	const connection = new Connection({
		url,
		share: false,
		websocket: { enabled: false },
	});
	try {
		await waitUntil(() => connection.status.peek() === "connected");
		const allocator = connection.bandwidth.peek();
		if (!allocator) throw new Error("missing allocator");

		const track = new TrackProducer("video").accept({ priority: 60 });
		track.subscribe();
		const reserved = allocator.reserve(track, 4_000_000);

		await waitUntil(() => reserved.peek() === 2_000_000);
	} finally {
		connection.close();
	}
});
