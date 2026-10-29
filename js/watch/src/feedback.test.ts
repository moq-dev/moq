import { expect, test } from "bun:test";
import { Manual, Milli, Nano } from "@moq/clock";
import * as Catalog from "@moq/hang/catalog";
import { Path } from "@moq/net";
import { Tally } from "./feedback";

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
	tally.arrive(live, Milli(1234.9));
	const arrival = tally.report(live.path).video.arrival;
	expect(arrival?.timestamp).toBe(1234);
	expect(arrival?.wall).toBeCloseTo(Date.now() - Catalog.MOQ_EPOCH_UNIX_MILLIS, -3);
});

test("keeps the most recent few catalog broadcasts", () => {
	const tally = new Tally();
	for (let n = 0; n < 6; n++) tally.count({ path: Path.from(`b${n}.hang`), id: "video" }, { frames: 1 });
	expect(tally.report(Path.from("b0.hang"))).toEqual({});
	expect(tally.report(Path.from("b5.hang"))).toEqual({ video: { frames: 1 } });
});
