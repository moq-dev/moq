import { afterEach, describe, expect, spyOn, test } from "bun:test";
import { type Installed, Manual, Milli, Nano } from "@moq/clock";
import { accept, connect, type Established } from "./connection/index.ts";
import * as Ietf from "./ietf/index.ts";
import * as Lite from "./lite/index.ts";
import { createMockTransportPair, type MockTransport } from "./mock.ts";
import { ORDER, Schedule } from "./padding.ts";
import { Writer } from "./stream.ts";
import { wireOf } from "./wire.ts";

const TICK = Nano.fromMilli(Milli(100));
const MBPS = 1_000_000;

let installed: Installed | undefined;
afterEach(() => {
	installed?.close();
	installed = undefined;
});

interface Path {
	/** The rate to probe up to. */
	target: number;
	/** The transport's estimate, given whether the last tick padded. */
	estimate: (padded: boolean) => number;
	/** What the media sends, in bits per second. */
	media: number;
}

// Run a schedule for `seconds` against a transport that sends whatever it is handed, returning
// the padding bytes each tick handed it.
function simulate(path: Path, seconds: number): number[] {
	const clock = new Manual();
	installed = clock.install();
	const schedule = new Schedule();
	let sent = 0;
	let padded = false;
	const pads: number[] = [];
	for (let i = 0; i < seconds * 10; i++) {
		const bytes = schedule.tick({ target: path.target, estimate: path.estimate(padded), sent });
		schedule.padded(bytes);
		pads.push(bytes);
		padded ||= bytes > 0;
		clock.jump(TICK);
		sent += (path.media / 8) * Nano.toSecond(TICK) + bytes;
	}
	return pads;
}

// The ticks each cluster started on.
function starts(pads: number[]): number[] {
	return pads.flatMap((bytes, i) => (bytes > 0 && !(pads[i - 1] > 0) ? [i] : []));
}

// The bytes a tick fills up to `rate` beside `media`.
function fill(rate: number, media: number): number {
	return ((rate - media) / 8) * Nano.toSecond(TICK);
}

describe("Schedule", () => {
	test("pads nothing once the estimate meets the target", () => {
		const pads = simulate({ target: 4 * MBPS, estimate: () => 4 * MBPS, media: 0.5 * MBPS }, 30);
		expect(pads.every((bytes) => bytes === 0)).toBe(true);
	});

	test("pads nothing while media holds the estimate", () => {
		const pads = simulate({ target: 8 * MBPS, estimate: () => 2 * MBPS, media: 1.8 * MBPS }, 30);
		expect(pads.every((bytes) => bytes === 0)).toBe(true);
	});

	test("pads nothing without an estimate or a byte count", () => {
		const clock = new Manual();
		installed = clock.install();
		const schedule = new Schedule();
		for (let i = 0; i < 300; i++) {
			expect(
				schedule.tick({ target: 4 * MBPS, estimate: i % 2 ? undefined : MBPS, sent: i % 2 ? 0 : undefined }),
			).toBe(0);
			clock.jump(TICK);
		}
	});

	test("clusters about every five seconds at twice the estimate, within the overhead budget", () => {
		const seconds = 60;
		const pads = simulate({ target: 10 * MBPS, estimate: () => MBPS, media: 0.5 * MBPS }, seconds);
		const clusters = starts(pads);
		expect(clusters.length).toBeGreaterThanOrEqual(10);
		// The detector needs a moment to see the session app-limited, then waits a period.
		expect(clusters[0]).toBeGreaterThanOrEqual(50);
		for (const [i, start] of clusters.entries()) {
			expect(pads[start]).toBe(Math.floor(fill(2 * MBPS, 0.5 * MBPS)));
			if (i > 0) expect(start - clusters[i - 1]).toBe(50);
		}

		const padding = pads.reduce((sum, bytes) => sum + bytes, 0);
		expect(padding).toBeLessThanOrEqual((MBPS / 8) * 0.1 * seconds);
	});

	test("caps a cluster at the target", () => {
		const pads = simulate({ target: 1.2 * MBPS, estimate: () => MBPS, media: 0.5 * MBPS }, 10);
		const [start] = starts(pads);
		expect(start).toBeDefined();
		expect(pads[start]).toBe(Math.floor(fill(1.2 * MBPS, 0.5 * MBPS)));
	});

	test("a validated cluster follows up before the next period", () => {
		// The path had room: the first cluster lifts the estimate past its validation threshold.
		const pads = simulate(
			{ target: 10 * MBPS, estimate: (padded) => (padded ? 1.6 : 1) * MBPS, media: 0.5 * MBPS },
			15,
		);
		const [first, followUp] = starts(pads);
		expect(followUp - first).toBeLessThan(50);
		expect(pads[followUp]).toBe(Math.floor(fill(3.2 * MBPS, 0.5 * MBPS)));
	});

	test("a falling estimate ends a cluster at once, without a follow-up", () => {
		const pads = simulate(
			{ target: 10 * MBPS, estimate: (padded) => (padded ? 0.9 : 1) * MBPS, media: 0.4 * MBPS },
			15,
		);
		const [first, next] = starts(pads);
		expect(pads[first + 1]).toBe(0);
		expect(next - first).toBe(50);
	});
});

const url = new URL("https://localhost:4443/test");

async function session(protocol: string): Promise<{ client: Established; server: Established; quic: MockTransport }> {
	const pair = createMockTransportPair(protocol, { stats: { estimatedSendRate: MBPS, bytesSent: 0 } });
	const [client, server] = await Promise.all([
		connect({ url, transport: pair.client }),
		accept({ transport: pair.server, url }),
	]);
	return { client, server, quic: pair.client };
}

describe("Padding", () => {
	test.each([Lite.ALPN_07_WIP, ...Object.values(Ietf.ALPN)])("%s pads at the lowest send order", async (protocol) => {
		const clock = new Manual();
		installed = clock.install();
		const { client, server, quic } = await session(protocol);
		const sender = wireOf(client).sender;
		if (!sender) throw new Error("no sender");

		// Padding is the only long run of zeros a session writes.
		let padded = 0;
		const write = Writer.prototype.write;
		const writes = spyOn(Writer.prototype, "write").mockImplementation(function (this: Writer, chunk) {
			if (chunk.byteLength > 64 && chunk.every((byte) => byte === 0)) padded += chunk.byteLength;
			return write.call(this, chunk);
		});

		const before = quic.sendStreams.uni.length;
		sender.setTarget(4 * MBPS);
		await clock.advance(Nano.fromMilli(Milli(8000)));
		writes.mockRestore();

		const padding = quic.sendStreams.uni.slice(before);
		expect(padding.length).toBe(1);
		expect(padding[0].sendOrder).toBe(ORDER);
		expect(padded).toBeGreaterThan(0);

		// Opting out stops padding.
		sender.setTarget(0);
		await clock.advance(Nano.fromMilli(Milli(20_000)));
		expect(quic.sendStreams.uni.length).toBe(before + 1);

		let closed = false;
		void server.closed.then(() => {
			closed = true;
		});
		await clock.advance(TICK);
		expect(closed).toBe(false);

		client.abort();
		server.abort();
	});

	test.each([Lite.ALPN_05, Lite.ALPN_06])("%s has no sender", async (protocol) => {
		const { client, server } = await session(protocol);
		expect(wireOf(client).sender).toBeUndefined();
		client.abort();
		server.abort();
	});
});
