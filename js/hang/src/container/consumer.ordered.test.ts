// A consumer reading an ordered reader: the reader decides every hole, and the arrival subscription
// only feeds the estimate.
import { expect, test } from "bun:test";
import { Duration } from "@moq/clock";
import { Manual } from "@moq/clock/test";
import { Time, Track } from "@moq/net";
import { u53 } from "../catalog/integers.ts";
import { Consumer, type Read } from "./consumer.ts";
import { encodeFrame, Format } from "./legacy.ts";
import { defined } from "../defined.test-util.ts";

const flush = () => new Promise<void>((resolve) => setImmediate(resolve));

// One 20 ms CELT packet: its TOC byte states the duration, which an Opus format reads.
const OPUS = new Uint8Array([0xf8]);

// An audio track whose missing groups an unserved fill owes, as a session's feed would, read by a
// consumer whose ordered reader waits `wait` for a hole and whose arrival subscription reaches 2 s.
// Frames state no duration unless `opus`.
function setup(wait: number, opus = false) {
	const clock = new Manual();
	const installed = clock.install();
	const track = new Track.Producer("audio");
	const requests = track.requests();
	const ordered = track.subscribe({ delay: { min: Duration.ZERO, max: Duration.millis(wait) } }).ordered();
	const consumer = new Consumer(track.subscribe({ delay: { max: Duration.secs(2) } }), {
		format: opus
			? new Format({
					codec: "opus",
					sampleRate: u53(48_000),
					numberOfChannels: u53(2),
					container: { kind: "legacy" },
				})
			: new Format("audio"),
		delay: { min: Duration.ZERO, max: Duration.secs(2) },
		ordered,
	});
	const reads: Read[] = [];
	const reading = (async () => {
		for (;;) {
			const read = await consumer.next();
			if (!read) return;
			reads.push(read);
		}
	})();
	// One finished group holding a 20 ms frame at `ms`.
	const write = (sequence: bigint, ms: number) => {
		const group = defined(track.createGroup(sequence));
		group.writeFrame({
			payload: encodeFrame(opus ? OPUS : new Uint8Array([1]), Duration.millis(ms)),
			timestamp: Time.Timestamp.fromMillis(ms),
		});
		group.finish();
	};
	return {
		clock,
		track,
		consumer,
		reads,
		write,
		// What was read, as `frame 20` or `gap 20+20`, in ms.
		read: () =>
			reads.map((read) =>
				read.kind === "gap"
					? `gap ${read.timestamp.asMillis()}+${read.duration.asMillis()}`
					: read.kind === "frame"
						? `frame ${read.frame.timestamp.asMillis()}`
						: `end ${read.end.asMillis()}`,
			),
		async close() {
			consumer.close();
			requests.close();
			track.finish();
			track.close();
			try {
				await reading;
			} finally {
				installed.close();
			}
		},
	};
}

test("a hole is waited for as long as the ordered reader waits, then read as a gap", async () => {
	const t = setup(100);
	try {
		t.write(0n, 0);
		t.write(2n, 40);
		await flush();
		expect(t.read()).toEqual(["frame 0"]);
		// The consumer keeps no wait of its own: the reader's budget alone holds the group past the hole.
		await t.clock.advance(Duration.millis(99));
		expect(t.read()).toEqual(["frame 0"]);
		await t.clock.advance(Duration.millis(1));
		await flush();
		expect(t.read()).toEqual(["frame 0", "gap 0+40", "frame 40"]);
	} finally {
		await t.close();
	}
});

test("a gap the reader reports before an empty group still reads as a gap before the next frame", async () => {
	const t = setup(100);
	try {
		t.write(0n, 0);
		// Group 2 holds nothing. The hole at 1 ages from group 2's arrival, while group 2 ages from
		// group 3's later one, so the reader hands out the empty group 2 with the gap on it.
		defined(t.track.createGroup(2n)).finish();
		await t.clock.advance(Duration.millis(90));
		t.write(3n, 60);
		await t.clock.advance(Duration.millis(10));
		await flush();
		expect(t.read()).toEqual(["frame 0", "gap 0+60", "frame 60"]);
	} finally {
		await t.close();
	}
});

test("a group that lands within the reader's wait fills its hole", async () => {
	const t = setup(100);
	try {
		t.write(0n, 0);
		t.write(2n, 40);
		await t.clock.advance(Duration.millis(50));
		t.write(1n, 20);
		await flush();
		expect(t.read()).toEqual(["frame 0", "frame 20", "frame 40"]);
	} finally {
		await t.close();
	}
});

test("the estimate observes late groups the ordered reader gave up on", async () => {
	// The spread after 5 s of groups 20 ms apart, every tenth landing `late` ms past its turn.
	const spread = async (late: number) => {
		const t = setup(50);
		try {
			const arrivals = Array.from({ length: 250 }, (_, i) => ({ i, at: i * 20 + (i % 10 === 5 ? late : 0) }));
			arrivals.sort((a, b) => a.at - b.at);
			let at = 0;
			for (const arrival of arrivals) {
				await t.clock.advance(Duration.millis(arrival.at - at));
				at = arrival.at;
				t.write(BigInt(arrival.i), arrival.i * 20);
			}
			await flush();
			// The reader passed each late group long before it landed, so it plays as a gap.
			if (late > 0) expect(t.read()).not.toContain("frame 100");
			return t.consumer.spread.peek();
		} finally {
			await t.close();
		}
	};
	expect((await spread(300)).gt(await spread(0))).toBe(true);
});

test("a gap over keys that held no media is no loss", async () => {
	const t = setup(100, true);
	try {
		// Keyed by timestamp in microseconds, with the keys between never declared skipped. The group
		// past them opens before its frame lands, and the reader gives the keys up while it waits.
		t.write(0n, 0);
		const group = defined(t.track.createGroup(20_000n));
		await t.clock.advance(Duration.millis(100));
		// A later group the reader hands out at once wakes the consumer before the frame says nothing
		// was missing.
		t.track.skipGroups(20_001n, 40_000n);
		t.write(40_000n, 40);
		await flush();
		group.writeFrame({ payload: encodeFrame(OPUS, Duration.millis(20)), timestamp: Time.Timestamp.fromMillis(20) });
		group.finish();
		await flush();
		expect(t.read()).toEqual(["frame 0", "frame 20", "frame 40"]);
	} finally {
		await t.close();
	}
});

test("a gap over keys that held media is still a loss", async () => {
	const t = setup(100, true);
	try {
		t.write(0n, 0);
		t.write(40_000n, 40);
		await t.clock.advance(Duration.millis(100));
		await flush();
		expect(t.read()).toEqual(["frame 0", "gap 20+20", "frame 40"]);
	} finally {
		await t.close();
	}
});

test("an open group that never delivers a frame is passed once the reader's wait ages it", async () => {
	const t = setup(100, true);
	try {
		t.write(0n, 0);
		// Opened, never written or finished.
		defined(t.track.createGroup(1n));
		// Group 2 lands at 20 ms. The reader expires the open group a wait after its successor landed.
		for (let i = 2; i <= 6; i++) {
			await t.clock.advance(Duration.millis(20));
			t.write(BigInt(i), i * 20);
		}
		await t.clock.advance(Duration.millis(19));
		await flush();
		expect(t.read()).toEqual(["frame 0"]);
		await t.clock.advance(Duration.millis(1));
		await flush();
		expect(t.read()).toEqual([
			"frame 0",
			"gap 20+20",
			"frame 40",
			"frame 60",
			"frame 80",
			"frame 100",
			"frame 120",
		]);
	} finally {
		await t.close();
	}
});
