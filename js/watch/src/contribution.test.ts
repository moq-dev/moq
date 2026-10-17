import { expect, it } from "bun:test";
import * as Catalog from "@moq/hang/catalog";
import { Epoch, Origin, Path } from "@moq/net";
import { Effect, Signal } from "@moq/signals";
import { Broadcast } from "./broadcast";
import { Source } from "./video/source";

it("lists contributions without demand and keeps selected rendition IDs in their own broadcast", async () => {
	const origin = new Origin.Producer();
	const parent = origin.createBroadcast(Path.from("room/main.hang"));
	const contribution = origin.createBroadcast(Path.from("room/extra.hang"));
	parent.announce();
	contribution.announce();
	const track = contribution.createTrack(Catalog.TRACK);
	const catalog = Catalog.RootSchema.parse({
		video: { renditions: { video: { codec: "avc1.42001f", container: { kind: "legacy" } } } },
		contributions: { extra: { broadcast: "./extra.hang" }, missing: { broadcast: "./absent.hang" } },
	});
	const source = new Broadcast({
		origin,
		name: Path.from("room/main.hang"),
		catalogFormat: "manual",
		catalog,
		announced: false,
	});
	const selection = new Effect();
	for (const [publisher, content] of [
		[parent, "source frames"],
		[contribution, "contribution frames"],
	] as const) {
		publisher.createTrack("video").writeFrame({ payload: new TextEncoder().encode(content) });
	}
	try {
		await source.out.catalog.changed();
		expect(source.out.catalog.peek()?.contributions).toEqual(catalog.contributions);
		// Listing the reference does not subscribe to its existing catalog track.
		expect(contribution.demand().used.peek()).toBe(false);
		const child = source.contribution(selection, "extra");
		expect(child).toBeDefined();
		if (!child) throw new Error("missing selected contribution");
		await contribution.demand().used.changed();
		expect(contribution.demand().used.peek()).toBe(true);
		track.writeFrame({
			timestamp: undefined,
			payload: new TextEncoder().encode(
				JSON.stringify({
					video: { renditions: { video: { codec: "vp8", container: { kind: "legacy" } } } },
				}),
			),
		});
		await child.out.catalog.changed();
		expect(child.out.catalog.peek()?.video?.renditions.video.codec).toBe("vp8");
		expect(source.out.catalog.peek()?.video?.renditions.video.codec).toBe("avc1.42001f");
		const media = new Source({ broadcast: child, target: { name: "video" }, supported: async () => true });
		selection.cleanup(() => media.close());
		await media.out.config.changed();
		expect(media.out.track.peek()).toBe("video");
		const selected = child.relativeBroadcast(selection, media.out.config.peek()?.broadcast);
		const frames = selected
			?.track(media.out.track.peek() ?? "")
			.subscribe()
			.ordered();
		const original = source.relativeBroadcast(selection, undefined)?.track("video").subscribe().ordered();
		try {
			expect(await frames?.readString()).toBe("contribution frames");
			expect(await original?.readString()).toBe("source frames");
		} finally {
			frames?.close();
			original?.close();
		}
		expect(source.contribution(selection, "unknown").in.enabled.peek()).toBe(false);
		const absent = origin.dynamic(Path.from("room/absent.hang"));
		selection.cleanup(() => absent.close());
		const requests = absent.requested();
		const missing = source.contribution(selection, "missing");
		if (!missing) throw new Error("missing contribution selection");
		const { value: request } = await requests.next();
		expect(request?.path).toBe(Path.from("room/absent.hang"));
		request?.reject(new Error("contribution unavailable"));
		await settle();
		// Like a rendition's reference, an unavailable contribution is offline.
		expect(missing.out.active.peek()).toBeUndefined();
		expect(missing.out.status.peek()).toBe("offline");
		expect(source.out.catalog.peek()?.video?.renditions.video.codec).toBe("avc1.42001f");
		selection.close();
		await contribution.demand().unused();
		expect(contribution.demand().used.peek()).toBe(false);
	} finally {
		selection.close();
		source.close();
		origin.close();
	}
});

it("refuses malformed and escaping contribution entries", () => {
	for (const entry of [null, {}, { broadcast: 42 }, { broadcast: "./extra.hang", epoch: "not-an-epoch" }]) {
		expect(() => Catalog.RootSchema.parse({ contributions: { extra: entry } })).toThrow();
	}
	const catalog = Catalog.RootSchema.parse({ contributions: { extra: { broadcast: "../../escape" } } });
	expect(() => Catalog.checkResolvable(catalog, Path.from("room/main.hang"))).toThrow(Catalog.EscapingBroadcast);
});

const settle = async (): Promise<void> => {
	for (let i = 0; i < 5; i++) await new Promise((resolve) => setTimeout(resolve, 0));
};

async function quietly<T>(fn: () => Promise<T>): Promise<T> {
	const warn = console.warn;
	console.warn = () => {};
	try {
		return await fn();
	} finally {
		console.warn = warn;
	}
}

it("tracks the contribution entry without reopening it on unrelated catalog updates", async () => {
	const origin = new Origin.Producer();
	const main = origin.createBroadcast(Path.from("room/main.hang"));
	main.announce();
	const first = origin.createBroadcast(Path.from("room/first.hang"));
	const second = origin.createBroadcast(Path.from("room/second.hang"));
	for (const publisher of [first, second]) {
		publisher.announce();
		publisher.createTrack(Catalog.TRACK).writeFrame({ payload: new TextEncoder().encode("{}") });
	}
	const catalog = new Signal(Catalog.RootSchema.parse({ contributions: { extra: { broadcast: "./first.hang" } } }));
	const parent = new Broadcast({
		origin,
		name: Path.from("room/main.hang"),
		catalogFormat: "manual",
		catalog,
		announced: false,
	});
	const owner = new Effect();
	try {
		const child = parent.contribution(owner, "extra");
		await child.out.catalog.changed();
		const active = child.out.active.peek();
		expect(active?.path).toBe(Path.from("room/first.hang"));

		// An update that leaves the entry alone keeps the same request.
		let withdrawals = 0;
		owner.cleanup(
			first.demand().used.subscribe((used) => {
				if (!used) withdrawals += 1;
			}),
		);
		catalog.set(
			Catalog.RootSchema.parse({
				contributions: { extra: { broadcast: "./first.hang" }, other: { broadcast: "./other.hang" } },
				metadata: "changed",
			}),
		);
		await settle();
		expect(child.out.active.peek()).toBe(active);
		expect(withdrawals).toBe(0);

		// A changed path is a different reference, so the child follows it.
		catalog.set(Catalog.RootSchema.parse({ contributions: { extra: { broadcast: "./second.hang" } } }));
		await settle();
		expect(child.out.active.peek()?.path).toBe(Path.from("room/second.hang"));

		// Removing the entry disables the child.
		catalog.set(Catalog.RootSchema.parse({}));
		await settle();
		expect(child.in.enabled.peek()).toBe(false);
		expect(child.out.active.peek()).toBeUndefined();
	} finally {
		owner.close();
		parent.close();
		origin.close();
	}
});

it("follows a restarted contribution only for an entry that names no epoch", async () => {
	const origin = new Origin.Producer();
	const main = origin.createBroadcast(Path.from("room/main.hang"));
	main.announce();
	const [oldEpoch, newEpoch] = [Epoch.mint(), Epoch.mint()];
	const old = origin.createBroadcast(Path.from("room/extra.hang"));
	old.announce({ epoch: oldEpoch });
	const parent = new Broadcast({
		origin,
		name: Path.from("room/main.hang"),
		catalogFormat: "manual",
		catalog: Catalog.RootSchema.parse({
			contributions: {
				following: { broadcast: "./extra.hang" },
				pinned: { broadcast: "./extra.hang", epoch: oldEpoch },
			},
		}),
	});
	const owner = new Effect();
	try {
		const following = parent.contribution(owner, "following");
		const pinned = parent.contribution(owner, "pinned");
		await settle();
		expect(following.out.active.peek()?.epoch).toBe(oldEpoch);
		expect(pinned.out.active.peek()?.epoch).toBe(oldEpoch);

		// The announcement is the online signal: the plain entry follows the restart, while the
		// one naming the old instance goes offline rather than reading the new one.
		const replacement = origin.createBroadcast(Path.from("room/extra.hang"));
		replacement.announce({ epoch: newEpoch });
		await quietly(settle);
		expect(following.out.active.peek()?.epoch).toBe(newEpoch);
		expect(pinned.out.active.peek()).toBeUndefined();
		expect(pinned.out.status.peek()).toBe("offline");
		replacement.close();
	} finally {
		owner.close();
		parent.close();
		old.close();
		origin.close();
	}
});

it("never reads a contribution proven to be another epoch, but reads an epochless route", async () => {
	const origin = new Origin.Producer();
	const main = origin.createBroadcast(Path.from("room/main.hang"));
	main.announce();
	const served = Epoch.mint();
	const proven = origin.createBroadcast(Path.from("room/proven.hang"));
	proven.announce({ epoch: served });
	const track = proven.createTrack(Catalog.TRACK);
	const plain = origin.createBroadcast(Path.from("room/plain.hang"));
	plain.announce();
	plain.createTrack(Catalog.TRACK).writeFrame({ payload: new TextEncoder().encode("{}") });
	const named = Epoch.mint();
	const contributions = (epoch: Epoch.Valid) =>
		Catalog.RootSchema.parse({
			contributions: {
				proven: { broadcast: "./proven.hang", epoch },
				plain: { broadcast: "./plain.hang", epoch: named },
			},
		});
	const catalog = new Signal(contributions(named));
	const parent = new Broadcast({
		origin,
		name: Path.from("room/main.hang"),
		catalogFormat: "manual",
		catalog,
		announced: false,
	});
	const owner = new Effect();
	try {
		const refused = parent.contribution(owner, "proven");
		const accepted = parent.contribution(owner, "plain");
		await quietly(settle);
		expect(refused.out.active.peek()).toBeUndefined();
		expect(refused.out.error.peek()).toBeUndefined();
		expect(track.demand().used.peek()).toBe(false);
		// An epochless route cannot prove its instance, so the epoch cannot refuse it.
		expect(accepted.out.active.peek()?.path).toBe(Path.from("room/plain.hang"));

		// A catalog update naming the served epoch admits the instance the request already holds.
		catalog.set(contributions(served));
		await settle();
		expect(refused.out.active.peek()?.epoch).toBe(served);
	} finally {
		owner.close();
		parent.close();
		origin.close();
	}
});

it("shares the parent's resolution, so a new epoch checks the held handle", async () => {
	const origin = new Origin.Producer();
	const [served, other] = [Epoch.mint(), Epoch.mint()];
	const main = origin.createBroadcast(Path.from("room/main.hang"));
	main.announce({ epoch: served });
	const extra = origin.createBroadcast(Path.from("room/extra.hang"));
	extra.announce({ epoch: served });
	const entries = (epoch?: Epoch.Valid) =>
		Catalog.RootSchema.parse({
			contributions: {
				sibling: { broadcast: "./extra.hang", epoch },
				self: { broadcast: "./main.hang", epoch },
				empty: { broadcast: "", epoch },
			},
		});
	const catalog = new Signal(entries());
	const parent = new Broadcast({
		origin,
		name: Path.from("room/main.hang"),
		catalogFormat: "manual",
		catalog,
		announced: false,
	});
	const owner = new Effect();
	try {
		const sibling = parent.contribution(owner, "sibling");
		const self = parent.contribution(owner, "self");
		await settle();
		const held = sibling.out.active.peek();
		expect(held?.epoch).toBe(served);
		// A self-reference is the parent's own handle, not a request of its own. An empty one is
		// too, and is named like the parent, so its own references resolve against the same base.
		expect(self.out.active.peek()).toBe(parent.out.active.peek());
		const empty = parent.contribution(owner, "empty");
		await settle();
		expect(empty.in.name.peek()).toBe(Path.from("room/main.hang"));
		expect(empty.in.enabled.peek()).toBe(true);

		let withdrawals = 0;
		owner.cleanup(
			extra.demand().used.subscribe((used) => {
				if (!used) withdrawals += 1;
			}),
		);

		// Naming the held instance keeps the same handle rather than resolving the path again.
		catalog.set(entries(served));
		await settle();
		expect(sibling.out.active.peek()).toBe(held);
		expect(self.out.active.peek()).toBe(parent.out.active.peek());
		expect(withdrawals).toBe(0);

		// Naming another instance fails the held one.
		catalog.set(entries(other));
		await quietly(settle);
		expect(sibling.out.active.peek()).toBeUndefined();
		expect(self.out.active.peek()).toBeUndefined();
	} finally {
		owner.close();
		parent.close();
		origin.close();
	}
});
