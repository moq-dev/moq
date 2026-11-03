import { expect, test } from "bun:test";
import { Manual, Milli, Nano, now, Wall, wall } from "@moq/clock";
import * as Catalog from "@moq/hang/catalog";
import { Path } from "@moq/net";
import { arrivedAt, Tally } from "./feedback";

const live = { path: Path.from("room/live.hang"), id: "video" };

test("counts per rendition of each catalog broadcast, omitting zeros", () => {
	const tally = new Tally();
	tally.count(live, { frames: 1, bytes: 10, late: 0 });
	tally.count(live, { frames: 1, bytes: 5 });
	tally.count({ ...live, id: "audio" }, { underruns: 2 });
	tally.count({ path: Path.from("other.hang"), id: "video" }, { frames: 7 });
	expect(tally.report(live.path)).toEqual({ video: { frames: 2, bytes: 15 }, audio: { underruns: 2 } });
	expect(tally.report(Path.from("unknown.hang"))).toEqual({});
});

test("a rendition named __proto__ is reported as its own ID", () => {
	const tally = new Tally();
	tally.count({ ...live, id: "__proto__" }, { frames: 1 });
	tally.count(live, { frames: 2 });
	const report = tally.report(live.path);
	expect(Object.keys(report).sort()).toEqual(["__proto__", "video"]);
	expect(Object.hasOwn(report, "__proto__")).toBe(true);
	expect(JSON.parse(JSON.stringify(report)).__proto__).toEqual({ frames: 1 });
});

test("a stall reports its time so far and adds its duration once it ends", async () => {
	const clock = new Manual();
	const installed = clock.install();
	try {
		const tally = new Tally();
		const end = tally.stall(live);
		await clock.advance(Nano.fromMilli(Milli(250.6)));
		expect(tally.report(live.path).video).toEqual({ stalls: 1, stalled: 250 });
		await clock.advance(Nano.fromMilli(Milli(100)));
		expect(end()).toBeCloseTo(350.6);
		await clock.advance(Nano.fromMilli(Milli(1000)));
		expect(tally.report(live.path).video).toEqual({ stalls: 1, stalled: 350 });
	} finally {
		installed.close();
	}
});

test("an arrival pairs the media timestamp with milliseconds since the MoQ epoch", () => {
	const tally = new Tally();
	tally.arrive(live, Milli(1234.9), Wall(Catalog.MOQ_EPOCH_UNIX_MILLIS + 5000.7));
	expect(tally.report(live.path).video.arrival as unknown).toEqual({ timestamp: 1234, wall: 5000 });
});

test("an arrival's wall time is when the frame came off the transport, not when it was read", async () => {
	const clock = new Manual();
	const installed = clock.install();
	try {
		await clock.advance(Nano.fromMilli(Milli(1000)));
		const arrival = Nano.toMilli(Nano(now()));
		const before = wall();
		await clock.advance(Nano.fromMilli(Milli(300)));
		expect(arrivedAt(arrival)).toBe(before);
	} finally {
		installed.close();
	}
});

test("keeps the most recent few catalog broadcasts", () => {
	const tally = new Tally();
	for (let n = 0; n < 6; n++) tally.count({ path: Path.from(`b${n}.hang`), id: "video" }, { frames: 1 });
	expect(tally.report(Path.from("b0.hang"))).toEqual({});
	expect(tally.report(Path.from("b5.hang"))).toEqual({ video: { frames: 1 } });
});

test("keeps a bounded number of rendition IDs per catalog broadcast, dropping the oldest", () => {
	const tally = new Tally();
	for (let n = 0; n <= 1024; n++) tally.count({ ...live, id: `r${n}` }, { frames: 1 });
	const report = tally.report(live.path);
	expect(Object.keys(report)).toHaveLength(1024);
	expect(report.r0).toBeUndefined();
	expect(report.r1024).toEqual({ frames: 1 });
});
