// The lite-07 range table (`rs/moq-net/tests/range-table.json`), which Rust's
// `rs/moq-net/tests/integration/range_table.rs` passes too: each case's reader gets exactly the
// listed groups direct from the origin, through a relay copy cold, warm, and as a late join, and
// across a route switch mid-read. A warm copy also answers from what it holds exactly when the case
// says it may. Every group arrives whole: a switch stalls the first route partway through, so the
// second supplies the groups the first never delivered and the tail of one it delivered only the
// head of.
import { expect, test } from "bun:test";
import { Duration, Elapsed, timeout } from "@moq/clock";
import { Manual } from "@moq/clock/test";
import type { Consumer as BroadcastConsumer } from "./broadcast.ts";
import * as cache from "./cache.ts";
import { accept as acceptSession, connect as connectSession } from "./connection/index.ts";
import { defined } from "./defined.test-util.ts";
import type { Consumer as GroupConsumer, Producer as GroupProducer } from "./group.ts";
import { WireRoute } from "./hop.ts";
import * as Instance from "./instance.ts";
import { hooks } from "./internal.ts";
import * as Lite from "./lite/index.ts";
import { createMockTransportPair } from "./mock.ts";
import { Producer as OriginProducer } from "./origin.ts";
import * as Path from "./path.ts";
import { Timescale, Timestamp } from "./time.ts";
import type { Range, Subscriber, Subscription } from "./track.ts";
import { wireOf } from "./wire.ts";

type Ask = { ranges?: { start?: number | { live: number }; end?: number }[] };
type Known = { mode: Mode; lang?: "rs" | "js" };
type Case = {
	name: string;
	groups?: number[];
	append?: boolean;
	span?: number;
	open?: number[];
	copy?: Ask[];
	evict?: boolean;
	read: Ask;
	deliver: number[];
	asks?: boolean;
	known?: Known[];
};

const table = (await Bun.file(new URL("../../../rs/moq-net/tests/range-table.json", import.meta.url)).json()) as {
	groups: number[];
	// The frames in each group, one per millisecond from its start.
	frames: number;
	cases: Case[];
};

// Where a case runs, as Rust's `Mode`.
type Mode = "direct" | "cold" | "warm" | "late" | "switch";

// How long a reader goes without a group before it has read everything it will.
const QUIET = Duration.millis(1_000);
// How long a reader waits for each group it still wants.
const ARRIVAL = Duration.millis(20_000);

const url = new URL("https://localhost:4443/test");
const path = Path.from("room");
// The origin's instance, which both routes of a switch carry, so the front resumes across them.
const INSTANCE = Instance.parse("01900000-0000-7000-8000-000000000001");

// Each live join under no max, so only its lag decides what it gets, and history otherwise.
function subscription(ask: Ask): Subscription {
	const ranges: Range[] = (ask.ranges ?? [{}]).map((range) => ({
		start:
			range.start === undefined
				? undefined
				: typeof range.start === "number"
					? BigInt(range.start)
					: { live: { min: Duration.millis(range.start.live), max: null } },
		end: range.end === undefined ? undefined : BigInt(range.end),
	}));
	return { order: "ascending", ranges };
}

// The origin, holding every group it writes for good, and its empty track.
function source() {
	const origin = new OriginProducer();
	const route = origin.route(path);
	const track = defined(route.broadcast.createTrack("data", { timescale: Timescale.MILLI, maxAge: null }));
	hooks.poolTrack(track, new cache.Pool());
	const head = Math.ceil(table.frames / 2);
	const frames = (group: GroupProducer, start: number, from: number, to: number) => {
		for (let frame = from; frame < to; frame++) {
			group.writeFrame({ payload: new Uint8Array([0x78]), timestamp: Timestamp.fromMillis(start + frame) });
		}
	};
	// Write the tail of a group `writeHead` left open, and finish it, unless the case leaves it open.
	const writeTail = (c: Case, live: { start: number; group: GroupProducer }) => {
		frames(live.group, live.start, head, table.frames);
		if (c.open?.includes(live.start)) return;
		if (c.span === undefined) live.group.finish();
		else live.group.finishAt(BigInt(live.start + c.span));
	};
	// Write every group but the tail of the last, returning it still open: the live edge
	// mid-group, which a reader may have only the head of when its route changes.
	const writeHead = (c: Case) => {
		let live: { start: number; group: GroupProducer } | undefined;
		for (const start of c.groups ?? table.groups) {
			if (live) writeTail(c, live);
			const time = Timestamp.fromMillis(start);
			const group = c.append === false ? track.createGroup(BigInt(start)) : track.appendGroup(time);
			frames(group, start, 0, head);
			live = { start, group };
		}
		return live;
	};
	return {
		origin,
		track,
		writeHead,
		writeTail,
		write(c: Case) {
			const live = writeHead(c);
			if (live) writeTail(c, live);
		},
		close() {
			track.close();
			route.broadcast.close();
			origin.close();
		},
	};
}

// A lite-07 session pulling from `origin`: the client's transport and its copy of the broadcast.
async function copyOf(origin: OriginProducer) {
	const pair = createMockTransportPair(Lite.ALPN_07_WIP);
	const [client, server] = await Promise.all([
		connectSession({ url, transport: pair.client }),
		acceptSession({ transport: pair.server, url, publish: origin.consume() }),
	]);
	return {
		transport: pair.client,
		// The origin's end, which opens the Group Streams.
		server: pair.server,
		consume: () => wireOf(client).consume(path),
		close() {
			client.abort();
			server.abort();
		},
	};
}

// What a reader got: each group as it arrived, then whether it ended or failed.
class Got {
	groups: GroupConsumer[] = [];
	ended = false;
	error?: string;
	// A receive that outlived a quiet wait, kept for the next read so no group is lost.
	#pending?: Promise<GroupConsumer | undefined>;

	// Receive until `want` groups arrived, each within ARRIVAL, then until none arrives for QUIET.
	async read(clock: Manual, reader: Subscriber, want: number): Promise<void> {
		while (!this.ended && this.error === undefined) {
			const wait = this.groups.length < want ? ARRIVAL : QUIET;
			this.#pending ??= reader.recvGroup();
			try {
				const group = await clock.drive(timeout(wait, this.#pending));
				this.#pending = undefined;
				if (!group) this.ended = true;
				else this.groups.push(group);
			} catch (err) {
				if (err instanceof Elapsed) return;
				this.#pending = undefined;
				this.error = String(err);
			}
		}
	}

	// Each group's start, as it arrived.
	starts(): bigint[] {
		return this.groups.map((group) => group.start);
	}

	// The groups in order, failing a group received twice, one missing any of its frames, or an
	// error. A group in `open` never ends, so it is read only through its frames.
	async delivered(clock: Manual, open: number[] = []): Promise<number[]> {
		const starts = this.starts();
		if (this.error !== undefined) throw new Error(`failed after ${starts}: ${this.error}`);
		for (const group of this.groups) {
			const start = Number(group.start);
			const got: number[] = [];
			while (!(open.includes(start) && got.length === table.frames)) {
				const frame = await clock.drive(timeout(ARRIVAL, group.readFrame()));
				if (!frame) break;
				got.push(frame.timestamp.value);
			}
			const want = Array.from({ length: table.frames }, (_, frame) => start + frame);
			expect(got, `group ${start}: frames`).toEqual(want);
		}
		const sorted = [...new Set(starts)].sort((a, b) => (a < b ? -1 : a > b ? 1 : 0));
		if (sorted.length !== starts.length) throw new Error(`delivered a group twice: ${starts}`);
		return sorted.map(Number);
	}

	close(): void {
		for (const group of this.groups) group.close();
	}
}

// Subscribe the copy's readers and read each out: what the copy then holds.
async function warm(clock: Manual, broadcast: BroadcastConsumer, c: Case): Promise<Subscriber[]> {
	const readers: Subscriber[] = [];
	for (const ask of c.copy ?? []) {
		const reader = broadcast.track("data").subscribe(subscription(ask));
		const got = new Got();
		await got.read(clock, reader, 0);
		got.close();
		readers.push(reader);
	}
	return readers;
}

// Serve `copy` to `front` as one route of the broadcast, at `cost`, until the returned close.
function serve(front: OriginProducer, copy: () => BroadcastConsumer, cost: bigint): () => void {
	const handle = wireOf(front).receive(path, WireRoute.normalize({ instance: INSTANCE, cost }));
	void (async () => {
		for await (const request of handle.requested()) request.serve(copy());
	})();
	return () => handle.close();
}

// Run `c` in `mode`: whether each reader ended, or throwing how it went wrong.
async function run(c: Case, mode: Mode): Promise<boolean> {
	const clock = new Manual();
	const installed = clock.install();
	const origin = source();
	const want = c.deliver.length;
	const got = new Got();
	// A reader joining once the switch settled, which must get the same.
	let after: Got | undefined;
	const cleanup: (() => void)[] = [() => got.close(), () => after?.close()];
	let cached: number | undefined;
	// Whether the warm read ended before upstream could answer: an empty delivery the copy alone gave.
	let endedWarm = false;
	try {
		switch (mode) {
			case "direct": {
				origin.write(c);
				const reader = origin.track.subscribe(subscription(c.read));
				cleanup.push(() => reader.close());
				await got.read(clock, reader, want);
				break;
			}
			case "cold":
			case "warm":
			case "late": {
				const upstream = await copyOf(origin.origin);
				cleanup.push(() => upstream.close());
				const remote = upstream.consume();
				cleanup.push(() => remote.close());
				if (mode === "late") {
					const readers = (c.copy ?? []).map((ask) => remote.track("data").subscribe(subscription(ask)));
					for (const reader of readers) cleanup.push(() => reader.close());
					await clock.advance(Duration.millis(500));
					origin.write(c);
					for (const reader of readers) {
						const got = new Got();
						await got.read(clock, reader, 0);
						got.close();
					}
				} else {
					origin.write(c);
					if (mode === "warm") {
						for (const reader of await warm(clock, remote, c)) cleanup.push(() => reader.close());
					}
				}
				if (c.evict) {
					// Past the idle window and the sweep trailing it, so the copy keeps only the live edge.
					await clock.advance(cache.DEFAULT_EXPIRY.mul(2).add(Duration.millis(2_000)));
				}

				// Show what a warm copy answers alone: nothing it asks upstream is delivered while held. A
				// late join's copy received every group live from where its readers joined, so it holds
				// more than the case's `asks` assumes.
				const held = mode === "warm";
				if (held) upstream.transport.hold();
				const reader = remote.track("data").subscribe(subscription(c.read));
				cleanup.push(() => reader.close());
				if (held) {
					await got.read(clock, reader, 0);
					cached = got.groups.length;
					endedWarm = got.ended;
					upstream.transport.release();
				}
				await got.read(clock, reader, want);
				break;
			}
			case "switch": {
				// The live edge stays mid-group until after the switch.
				const live = origin.writeHead(c);
				const a = await copyOf(origin.origin);
				const b = await copyOf(origin.origin);
				cleanup.push(
					() => a.close(),
					() => b.close(),
				);
				const warmed = b.consume();
				cleanup.push(() => warmed.close());
				for (const reader of await warm(clock, warmed, c)) cleanup.push(() => reader.close());

				const front = new OriginProducer();
				cleanup.push(() => front.close());
				const closeA = serve(front, a.consume, 5n);
				const request = front.consume().request(path);
				cleanup.push(() => request.close());
				await clock.advance(Duration.millis(500));
				const active = defined(request.active.peek());
				// The first route delivers the oldest half of the range and the head of its newest
				// group, then stalls: one Group Stream each.
				a.server.holdUnis();
				const reader = active.track("data").subscribe(subscription(c.read));
				cleanup.push(() => reader.close());
				await clock.advance(Duration.millis(500));
				const held = a.server.heldUnis();
				// Every group stalled, so the switch leaves the new route what the old never released.
				expect(held, "Group Streams the first route opened").toBe(want);
				const oldest = Math.min(Math.floor(want / 2), Math.max(held - 1, 0));
				for (let i = 0; i < oldest; i++) a.server.releaseOldestUni();
				if (held > oldest) a.server.releaseNewestUni();
				await got.read(clock, reader, Math.min(held, oldest + 1));
				// A standby route, priced to lose while the first stands, then the first goes away and
				// the live group goes on.
				cleanup.push(serve(front, b.consume, 1_000n));
				await clock.advance(Duration.millis(500));
				closeA();
				a.close();
				if (live) origin.writeTail(c, live);
				await got.read(clock, reader, want);

				const late = defined(request.active.peek()).track("data").subscribe(subscription(c.read));
				cleanup.push(() => late.close());
				after = new Got();
				await after.read(clock, late, want);
				break;
			}
		}

		const delivered = await got.delivered(clock, c.open);
		expect(delivered, `${mode}: delivered`).toEqual(c.deliver);
		if (after)
			expect(await after.delivered(clock, c.open), `${mode}: delivered after the switch`).toEqual(c.deliver);
		if (c.asks !== undefined && cached !== undefined) {
			// Delivering nothing is answered alone only if the reader ended before upstream could answer.
			const answered = cached === want && (want > 0 || endedWarm);
			if (answered === c.asks) {
				throw new Error(
					c.asks
						? `answered ${got.starts()} from the copy alone, which must ask`
						: `asked upstream, answering only ${cached} groups from the copy`,
				);
			}
		}
		return got.ended && (after?.ended ?? true);
	} finally {
		for (const close of cleanup.reverse()) close();
		origin.close();
		installed.close();
	}
}

for (const mode of ["direct", "cold", "warm", "late", "switch"] as const) {
	for (const c of table.cases) {
		if ((mode === "warm" || mode === "late") && !c.copy?.length) continue;
		// A known divergence is skipped: some reproduce only on some orderings, so one passing proves nothing.
		const known = c.known?.some((known) => known.mode === mode && (known.lang ?? "js") === "js");
		(known ? test.skip : test)(`range table ${mode}: ${c.name}`, async () => {
			const ended = await run(c, mode);
			// A switch never leaves a read open that ends direct from the origin.
			if (mode === "switch" && !ended && (await run(c, "direct")))
				throw new Error("never ended, as it does direct");
		});
	}
}
