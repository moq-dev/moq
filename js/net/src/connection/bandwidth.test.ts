import { afterEach, expect, spyOn, test } from "bun:test";
import { Duration, now, yieldNow } from "@moq/clock";
import { Manual } from "@moq/clock/test";
import * as Lite from "../lite/index.ts";
import { createMockTransportPair, type MockTransport } from "../mock.ts";
import { Producer as TrackProducer } from "../track.ts";
import { type AcceptProps, accept as acceptSession } from "./index.ts";
import { Connection, resetShared } from "./pool.ts";

const url = new URL("https://example.com/bandwidth");

function accept(transport: WebTransport, url: URL, props: Omit<AcceptProps, "transport" | "url"> = {}) {
	return acceptSession({ transport, url, ...props });
}

async function settle() {
	await yieldNow();
}

async function waitUntil(pred: () => boolean, ms = 1000): Promise<void> {
	const deadline = now().saturatingAdd(Duration.millis(ms));
	for (;;) {
		if (pred()) return;
		if (now().gt(deadline)) throw new Error("timed out waiting for condition");
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
		const pair = createMockTransportPair(Lite.ALPN_06, { stats });
		void accept(pair.server, url);
		return pair.client;
	};
	globalThis.WebTransport = stub as unknown as typeof WebTransport;
}

test("a connection samples the send rate onto the allocator", async () => {
	stubTransport({ estimatedSendRate: 2_000_000 });

	const connection = new Connection({ url });
	try {
		await waitUntil(() => connection.status.peek() === "connected");
		const allocator = connection.allocator.peek();
		if (!allocator) throw new Error("missing allocator");

		const track = new TrackProducer("video").accept({ priority: 60 });
		track.subscribe();
		const reserved = allocator.reserve(track.demand(), 4_000_000);

		await waitUntil(() => reserved.peek() === 2_000_000);
	} finally {
		connection.close();
	}
});

test("two publishers on one connection split the estimate by priority", async () => {
	stubTransport({ estimatedSendRate: 2_000_000 });

	const first = new Connection({ url });
	const second = new Connection({ url });
	try {
		await waitUntil(() => first.status.peek() === "connected");
		await waitUntil(() => second.status.peek() === "connected");

		const allocator = first.allocator.peek();
		expect(allocator).toBe(second.allocator.peek());
		if (!allocator) throw new Error("missing allocator");

		const audio = new TrackProducer("audio").accept({ priority: 80 });
		audio.subscribe();
		const audioShare = allocator.reserve(audio.demand(), 128_000);

		const video = new TrackProducer("video").accept({ priority: 60 });
		video.subscribe();
		const videoShare = allocator.reserve(video.demand(), 4_000_000);

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

	const handle = new Connection({ url });
	try {
		await waitUntil(() => handle.status.peek() === "connected");
		const allocator = handle.allocator.peek();
		if (!allocator) throw new Error("missing allocator");

		const watched = new TrackProducer("watched").accept({ priority: 60 });
		watched.subscribe();
		const watchedShare = allocator.reserve(watched.demand(), 4_000_000);

		const idle = new TrackProducer("idle").accept({ priority: 60 });
		const idleShare = allocator.reserve(idle.demand(), 4_000_000);

		await waitUntil(() => watchedShare.peek() === 2_000_000);
		// Claims nothing from the watched track, but reads what it would get back.
		expect(idleShare.peek()).toBe(1_000_000);
	} finally {
		handle.close();
	}
});

// The WebSocket fallback is covered where sessions detect their transport (stats.test.ts):
// a dial that reads as one would mark the URL as WebSocket-preferred for later tests.
const silent: { name: string; shape: (quic: MockTransport) => void }[] = [
	{
		name: "WebTransport without getStats",
		shape: (quic) => {
			(quic as unknown as { getStats?: unknown }).getStats = undefined;
		},
	},
	{ name: "WebRTC", shape: (quic) => Object.assign(quic, { transport: "webrtc" }) },
];

for (const kind of silent) {
	test(`a reservation over ${kind.name} starts no sampling`, async () => {
		const clock = new Manual();
		const installed = clock.install();
		const stub = function StubWebTransport() {
			const pair = createMockTransportPair(Lite.ALPN_06, { stats: { estimatedSendRate: 2_000_000 } });
			kind.shape(pair.client);
			void accept(pair.server, url);
			return pair.client;
		};
		globalThis.WebTransport = stub as unknown as typeof WebTransport;
		const snapshots = spyOn(Lite.Connection.prototype, "stats");

		const connection = new Connection({ url });
		try {
			await clock.advance(Duration.ZERO);
			expect(connection.status.peek()).toBe("connected");
			const allocator = connection.allocator.peek();
			if (!allocator) throw new Error("missing allocator");

			const track = new TrackProducer("video").accept({ priority: 60 });
			track.subscribe();
			const reservation = allocator.reserve(track.demand(), 4_000_000);
			await clock.advance(Duration.millis(350));
			expect(snapshots).toHaveBeenCalledTimes(0);
			// The estimate stays unknown, so the sender holds its rate.
			expect(reservation.peek()).toBeUndefined();
			reservation.close();
			track.close();
		} finally {
			connection.close();
			snapshots.mockRestore();
			installed.close();
		}
	});
}

test("a private connection still exposes an allocator", async () => {
	stubTransport({ estimatedSendRate: 2_000_000 });

	const connection = new Connection({
		url,
		share: false,
		websocket: { enabled: false },
	});
	try {
		await waitUntil(() => connection.status.peek() === "connected");
		const allocator = connection.allocator.peek();
		if (!allocator) throw new Error("missing allocator");

		const track = new TrackProducer("video").accept({ priority: 60 });
		track.subscribe();
		const reserved = allocator.reserve(track.demand(), 4_000_000);

		await waitUntil(() => reserved.peek() === 2_000_000);
	} finally {
		connection.close();
	}
});
