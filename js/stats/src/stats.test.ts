import { expect, test } from "bun:test";
import { yieldNow } from "@moq/clock";
import * as Json from "@moq/json";
import { type Broadcast, Origin, Path, Time } from "@moq/net";
import { defined } from "./defined.test-util.ts";
import {
	Consumer,
	Nodes,
	PresenceSchema,
	parseBucketTrack,
	parseNodePath,
	prefixTrack,
	presenceTrack,
	TrafficSchema,
} from "./index.ts";

// A scale-less snapshot write names the group it opens; keys only have to increase.
let keys = 0n;
const key = () => Time.Timestamp.unscaled(keys++);

function publish(broadcast: Broadcast.Producer, name: string) {
	return new Json.Snapshot.Producer<unknown>({ track: defined(broadcast.createTrack(name)) });
}

test("discovers each node under the prefix", async () => {
	const origin = new Origin.Producer();
	const nodes = new Nodes(origin.consume());
	const a = origin.route(Path.from(".stats/node/a"));
	origin.route(Path.from(".stats/node/b/1"));
	origin.route(Path.from(".stats/cluster/c"));

	const seen = [await nodes.next(), await nodes.next()].map((e) => [String(e?.node), e?.active]);
	expect(seen.sort()).toEqual([
		["a", true],
		["b/1", true],
	]);

	a.close();
	expect(await nodes.next()).toMatchObject({ node: "a", active: false });
	nodes.close();
	origin.close();
});

test("a bucket follows the listing to each new run", async () => {
	const origin = new Origin.Producer();
	const broadcast = origin.route(Path.from(".stats/node/a")).broadcast;
	const bucket = new Consumer(broadcast.consume()).bucket("");

	const first = publish(broadcast, "bucket.e1.json");
	first.update({ epoch: "e1", tiers: { "": { paths: { cam: { publisher: { bytes: 5 } } } } } }, key());
	const update = await bucket.next();
	if (update?.kind !== "frame") throw new Error("expected a frame");
	expect(update.frame.epoch).toBe("e1");
	// Missing counters read as zero.
	expect(update.frame.tiers[""]?.paths.cam?.publisher).toMatchObject({ bytes: 5, frames: 0, stale: { bytes: 0 } });

	// A merge-patch delta lands on the snapshot.
	first.update({ epoch: "e1", tiers: { "": { paths: { cam: { publisher: { bytes: 9 } } } } } }, key());
	expect(await bucket.next()).toMatchObject({
		kind: "frame",
		frame: { tiers: { "": { paths: { cam: { publisher: { bytes: 9 } } } } } },
	});

	// A new run replaces the old one, whose counters start over.
	const second = publish(broadcast, "bucket.e2.json");
	broadcast.removeTrack("bucket.e1.json");
	second.update({ epoch: "e2", tiers: {} }, key());
	expect(await bucket.next()).toMatchObject({ kind: "frame", frame: { epoch: "e2", tiers: {} } });

	// Another bucket's run is not this one's.
	publish(broadcast, "acme/bucket.e3.json").update({ epoch: "e3" }, key());
	broadcast.removeTrack("bucket.e2.json");
	expect(await bucket.next()).toEqual({ kind: "ended" });

	bucket.close();
	expect(await bucket.next()).toBeUndefined();
	origin.close();
});

test("totals, presence, and prefix tracks decode with defaults", async () => {
	const origin = new Origin.Producer();
	const broadcast = origin.route(Path.from(".stats/node")).broadcast;
	const consumer = new Consumer(broadcast.consume());

	publish(broadcast, "totals.json").update({ "": { sessions: { sessions_started: 2 } } }, key());
	publish(broadcast, presenceTrack("acme")).update({ gold: {} }, key());
	publish(broadcast, prefixTrack("acme/live")).update({ "": { self: { publisher: { bytes: 3 } } } }, key());

	expect((await consumer.totals().next())?.[""]?.sessions.sessions_started).toBe(2);
	expect((await consumer.presence("acme").next())?.gold?.sessions_failed.transport).toBe(0);
	const prefix = (await consumer.prefix("acme/live").next())?.[""];
	expect(prefix?.self.publisher.bytes).toBe(3);
	expect(prefix?.children).toEqual({});
	origin.close();
});

test("concurrent bucket reads each get a frame", async () => {
	const origin = new Origin.Producer();
	const broadcast = origin.route(Path.from(".stats/node/a")).broadcast;
	const bucket = new Consumer(broadcast.consume()).bucket("");
	const run = publish(broadcast, "bucket.e1.json");
	run.update({ epoch: "e1" }, key());

	const first = bucket.next();
	const second = bucket.next();
	expect((await first)?.kind).toBe("frame");
	run.update({ epoch: "e1", tiers: { "": {} } }, key());
	expect(await second).toMatchObject({ kind: "frame", frame: { tiers: { "": {} } } });
	bucket.close();
	origin.close();
});

test("a frame names each counter once and a missing counter reads zero", () => {
	const traffic = TrafficSchema.parse({ announces_started: 3, broadcasts_started: 2, broadcasts_ended: 1 });
	expect(traffic).toMatchObject({
		announces_started: 3,
		announces_ended: 0,
		broadcasts_started: 2,
		broadcasts_ended: 1,
	});
	expect(traffic).not.toHaveProperty("announced");
	expect(PresenceSchema.parse({ sessions_started: 4, sessions_ended: 2 })).toMatchObject({
		sessions_started: 4,
		sessions_ended: 2,
		// A label that never routed sends no `routes`.
		routes: { changed: 0, closed: { other: 0 } },
	});
	// The pre-rename spellings are neither read nor kept.
	expect(PresenceSchema.parse({ sessions: 4, sessions_closed: 2 })).toMatchObject({
		sessions_started: 0,
		sessions_ended: 0,
	});
});

test("track and node names", () => {
	expect(parseBucketTrack("bucket.x.json")).toEqual({ bucket: "", epoch: "x" });
	expect(parseBucketTrack("acme/live/bucket.x.json")).toEqual({ bucket: "acme/live", epoch: "x" });
	for (const name of ["bucket.json", "acme/bucket..json", "/bucket.x.json", "a//bucket.x.json", "acme/totals.json"]) {
		expect(parseBucketTrack(name)).toBeUndefined();
	}

	const node = (path: string) => parseNodePath(Path.from(".stats"), Path.from(path)) as string | undefined;
	expect(node(".stats/node/sjc/1")).toBe("sjc/1");
	expect(node(".stats/node")).toBe("");
	expect(node(".stats/nodes/sjc")).toBeUndefined();
	expect(node(".stats/acme/node/sjc")).toBeUndefined();
	expect(node("other/node/sjc")).toBeUndefined();
});

test("a finished run yields its final frame before it ends", async () => {
	const origin = new Origin.Producer();
	const broadcast = origin.route(Path.from(".stats/node/a")).broadcast;
	const bucket = new Consumer(broadcast.consume()).bucket("");
	const run = publish(broadcast, "bucket.e1.json");
	run.update({ epoch: "e1" }, key());
	expect((await bucket.next())?.kind).toBe("frame");

	// The final frame and the listing's `finished` arrive together; the frame must not be lost.
	run.update({ epoch: "e1", tiers: { "": {} } }, key());
	run.finish();
	broadcast.removeTrack("bucket.e1.json");
	// Let the listing report the run finished, so it is ready before the frame is read.
	await yieldNow();
	expect(await bucket.next()).toMatchObject({ kind: "frame", frame: { tiers: { "": {} } } });
	expect(await bucket.next()).toEqual({ kind: "ended" });
	bucket.close();
	origin.close();
});
