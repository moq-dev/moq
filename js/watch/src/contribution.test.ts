import { expect, it } from "bun:test";
import { yieldNow } from "@moq/clock";
import * as Catalog from "@moq/hang/catalog";
import * as Json from "@moq/json";
import { Epoch, Origin, Path, type Track } from "@moq/net";
import { Effect, Signal } from "@moq/signals";
import { Broadcast } from "./broadcast";
import { Source } from "./video/source";

function publish(origin: Origin.Producer, path: string, epoch?: Epoch.Valid) {
	const publication = origin.createBroadcast(Path.from(path));
	publication.announce({ epoch });
	return publication.broadcast;
}

const settle = async (): Promise<void> => {
	for (let i = 0; i < 5; i++) await yieldNow();
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

// Resolves once nobody subscribes to the track.
async function unwanted(track: Track.Producer): Promise<void> {
	const { wanted } = track.demand();
	while (wanted.peek() !== undefined) await wanted.changed();
}

const video = (codec: string) => ({ codec, container: { kind: "legacy" as const } });

it("opens a listed contribution only on selection, keeping its rendition IDs its own", async () => {
	const origin = new Origin.Producer();
	const parent = publish(origin, "room/main.hang");
	const contribution = publish(origin, "room/extra.hang");
	const contributionCatalog = contribution.createTrack(Catalog.TRACK);
	const catalog = new Json.Snapshot.Producer<Catalog.Root>({ track: contributionCatalog, deltas: false });
	parent.createTrack("video").writeFrame({ payload: new TextEncoder().encode("source frames") });
	const contributionVideo = contribution.createTrack("video");
	contributionVideo.writeFrame({ payload: new TextEncoder().encode("contribution frames") });

	// The contribution and a source rendition share the ID "video".
	const root = Catalog.RootSchema.parse({
		video: { renditions: { video: video("avc1.42001f") } },
		contributions: { video: { broadcast: "./extra.hang" } },
	});
	const source = new Broadcast({
		origin,
		name: Path.from("room/main.hang"),
		catalogFormat: "manual",
		catalog: root,
	});
	const selection = new Effect();
	try {
		await settle();
		expect(source.out.catalog.peek()?.contributions).toEqual(root.contributions);
		// Listing the entry does not subscribe to its catalog.
		expect(contributionCatalog.demand().wanted.peek()).toBeUndefined();

		const child = source.contribution(selection, "video");
		catalog.update({ value: { video: { renditions: { video: video("vp8") } } } });
		await settle();
		expect(contributionCatalog.demand().wanted.peek()).toBeDefined();
		expect(child.out.catalog.peek()?.video?.renditions.video?.codec).toBe("vp8");
		expect(source.out.catalog.peek()?.video?.renditions.video?.codec).toBe("avc1.42001f");

		const media = new Source({ broadcast: child, target: { name: "video" }, supported: async () => true });
		selection.cleanup(() => media.close());
		await settle();
		expect(media.out.track.peek()).toBe("video");
		const frames = child
			.relativeBroadcast(selection, media.out.config.peek()?.broadcast)
			?.track("video")
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

		// An unlisted ID leaves the child disabled.
		expect(source.contribution(selection, "unknown").in.enabled.peek()).toBe(false);

		selection.close();
		await unwanted(contributionCatalog);
		await unwanted(contributionVideo);
	} finally {
		selection.close();
		source.close();
		origin.close();
	}
});

it("reads an unavailable contribution as offline without touching the parent", async () => {
	const origin = new Origin.Producer();
	publish(origin, "room/main.hang");
	const claim = origin.requests(Path.from("room/absent.hang"));
	const requests = claim.requested();
	const source = new Broadcast({
		origin,
		name: Path.from("room/main.hang"),
		catalogFormat: "manual",
		catalog: Catalog.RootSchema.parse({
			video: { renditions: { hd: video("avc1.42001f") } },
			contributions: { missing: { broadcast: "./absent.hang" } },
		}),
		follow: false,
	});
	const selection = new Effect();
	try {
		await settle();
		const missing = source.contribution(selection, "missing");
		const { value: request } = await requests.next();
		expect(request?.path).toBe(Path.from("room/absent.hang"));
		request?.reject(new Error("contribution unavailable"));
		await settle();
		// Like a rendition's reference, an unavailable contribution is offline, not an error.
		expect(missing.out.active.peek()).toBeUndefined();
		expect(missing.out.error.peek()).toBeUndefined();
		expect(missing.out.status.peek()).toBe("offline");
		expect(source.out.status.peek()).toBe("live");
		expect(Object.keys(source.out.catalog.peek()?.video?.renditions ?? {})).toEqual(["hd"]);
	} finally {
		selection.close();
		source.close();
		claim.close();
		origin.close();
	}
});

it("reports a contribution outside the origin's scope as an error, leaving the parent live", async () => {
	const origin = new Origin.Producer();
	const scoped = origin.scope(Path.empty(), new Path.Patterns([Path.Pattern.parse("room/*")]));
	publish(scoped, "room/main.hang");
	const parent = new Broadcast({
		origin: scoped,
		name: Path.from("room/main.hang"),
		catalogFormat: "manual",
		catalog: Catalog.RootSchema.parse({ contributions: { outside: { broadcast: "../other/extra.hang" } } }),
		follow: false,
	});
	const owner = new Effect();
	try {
		const child = parent.contribution(owner, "outside");
		await settle();
		expect(child.out.active.peek()).toBeUndefined();
		expect(child.out.error.peek()?.message).toContain("outside the origin scope");
		expect(parent.out.status.peek()).toBe("live");
	} finally {
		owner.close();
		parent.close();
		origin.close();
	}
});

it("tracks the contribution entry without reopening it on unrelated catalog updates", async () => {
	const origin = new Origin.Producer();
	publish(origin, "room/main.hang");
	const firstCatalog = publish(origin, "room/first.hang").createTrack(Catalog.TRACK);
	publish(origin, "room/second.hang");
	const catalog = new Signal(Catalog.RootSchema.parse({ contributions: { extra: { broadcast: "./first.hang" } } }));
	const parent = new Broadcast({
		origin,
		name: Path.from("room/main.hang"),
		catalogFormat: "manual",
		catalog,
		follow: false,
	});
	const owner = new Effect();
	try {
		const child = parent.contribution(owner, "extra");
		await settle();
		const active = child.out.active.peek();
		expect(active?.path).toBe(Path.from("room/first.hang"));

		// An update that leaves the entry alone keeps the same request.
		expect(firstCatalog.demand().wanted.peek()).toBeDefined();
		let withdrawals = 0;
		owner.cleanup(
			firstCatalog.demand().wanted.subscribe((wanted) => {
				if (wanted === undefined) withdrawals += 1;
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

it("follows a restarted contributor only for an entry that names no epoch", async () => {
	const origin = new Origin.Producer();
	publish(origin, "room/main.hang");
	const [oldEpoch, newEpoch] = [Epoch.mint(), Epoch.mint()];
	const old = publish(origin, "room/extra.hang", oldEpoch);
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
		const replacement = publish(origin, "room/extra.hang", newEpoch);
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

it("pins the first instance of a contribution when not following announcements", async () => {
	const origin = new Origin.Producer();
	publish(origin, "room/main.hang");
	const first = publish(origin, "room/extra.hang", Epoch.mint());
	const parent = new Broadcast({
		origin,
		name: Path.from("room/main.hang"),
		catalogFormat: "manual",
		catalog: Catalog.RootSchema.parse({ contributions: { extra: { broadcast: "./extra.hang" } } }),
		follow: false,
	});
	const owner = new Effect();
	try {
		const child = parent.contribution(owner, "extra");
		await settle();
		const pinned = child.out.active.peek();
		expect(pinned?.path).toBe(Path.from("room/extra.hang"));

		// A restart ends the pinned instance without moving the child to the new one.
		const second = publish(origin, "room/extra.hang", Epoch.mint());
		await settle();
		expect(child.out.active.peek()).toBeUndefined();
		expect(child.out.error.peek()).toBeUndefined();
		second.close();
	} finally {
		owner.close();
		parent.close();
		first.close();
		origin.close();
	}
});

it("never reads a contribution proven to be another epoch, but reads an epochless route", async () => {
	const origin = new Origin.Producer();
	publish(origin, "room/main.hang");
	const served = Epoch.mint();
	const proven = publish(origin, "room/proven.hang", served);
	const track = proven.createTrack(Catalog.TRACK);
	publish(origin, "room/plain.hang");
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
		follow: false,
	});
	const owner = new Effect();
	try {
		const refused = parent.contribution(owner, "proven");
		const accepted = parent.contribution(owner, "plain");
		await quietly(settle);
		expect(refused.out.active.peek()).toBeUndefined();
		expect(refused.out.error.peek()).toBeUndefined();
		expect(track.closed.peek()).toBeUndefined();
		expect(track.demand().wanted.peek()).toBeUndefined();
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
	publish(origin, "room/main.hang", served);
	const extraCatalog = publish(origin, "room/extra.hang", served).createTrack(Catalog.TRACK);
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
		follow: false,
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

		expect(extraCatalog.demand().wanted.peek()).toBeDefined();
		let withdrawals = 0;
		owner.cleanup(
			extraCatalog.demand().wanted.subscribe((wanted) => {
				if (wanted === undefined) withdrawals += 1;
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
