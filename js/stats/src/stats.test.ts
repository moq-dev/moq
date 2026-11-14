import { expect, test } from "bun:test";
import * as Json from "@moq/json";
import { type Broadcast, Origin, Path, Time } from "@moq/net";
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
	return new Json.Snapshot.Producer<unknown>({ track: broadcast.createTrack(name) });
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
	first.update({ value: { epoch: "e1", tiers: { "": { paths: { cam: { publisher: { bytes: 5 } } } } } }, at: key() });
	const update = await bucket.next();
	if (update?.kind !== "frame") throw new Error("expected a frame");
	expect(update.frame.epoch).toBe("e1");
	// Missing counters read as zero.
	expect(update.frame.tiers[""]?.paths.cam?.publisher).toMatchObject({ bytes: 5, frames: 0, stale: { bytes: 0 } });

	// A merge-patch delta lands on the snapshot.
	first.update({ value: { epoch: "e1", tiers: { "": { paths: { cam: { publisher: { bytes: 9 } } } } } }, at: key() });
	expect(await bucket.next()).toMatchObject({
		kind: "frame",
		frame: { tiers: { "": { paths: { cam: { publisher: { bytes: 9 } } } } } },
	});

	// A new run replaces the old one, whose counters start over.
	const second = publish(broadcast, "bucket.e2.json");
	broadcast.removeTrack("bucket.e1.json");
	second.update({ value: { epoch: "e2", tiers: {} }, at: key() });
	expect(await bucket.next()).toMatchObject({ kind: "frame", frame: { epoch: "e2", tiers: {} } });

	// Another bucket's run is not this one's.
	publish(broadcast, "acme/bucket.e3.json").update({ value: { epoch: "e3" }, at: key() });
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

	publish(broadcast, "totals.json").update({ value: { "": { sessions: { sessions_started: 2 } } }, at: key() });
	publish(broadcast, presenceTrack("acme")).update({ value: { gold: {} }, at: key() });
	publish(broadcast, prefixTrack("acme/live")).update({
		value: { "": { self: { publisher: { bytes: 3 } } } },
		at: key(),
	});

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
	run.update({ value: { epoch: "e1" }, at: key() });

	const first = bucket.next();
	const second = bucket.next();
	expect((await first)?.kind).toBe("frame");
	run.update({ value: { epoch: "e1", tiers: { "": {} } }, at: key() });
	expect(await second).toMatchObject({ kind: "frame", frame: { tiers: { "": {} } } });
	bucket.close();
	origin.close();
});

test("legacy counter names fill in for missing canonical ones", () => {
	const traffic = TrafficSchema.parse({ announced: 3, broadcasts: 2, broadcasts_ended: 1, broadcasts_closed: 9 });
	expect(traffic).toMatchObject({ announces_started: 3, broadcasts_started: 2, broadcasts_ended: 1 });
	expect(PresenceSchema.parse({ sessions: 4, sessions_closed: 2 })).toMatchObject({
		sessions_started: 4,
		sessions_ended: 2,
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
