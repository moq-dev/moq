import { expect, it } from "bun:test";
import { yieldNow } from "@moq/clock";
import * as Catalog from "@moq/hang/catalog";
import * as Json from "@moq/json";
import { Instance, Origin, Path, type Track } from "@moq/net";
import { Effect, Signal } from "@moq/signals";
import * as Mint from "../../net/src/mint.ts";
import { wireOf } from "../../net/src/wire.ts";
import { Broadcast } from "./broadcast";
import { Source } from "./video/source";

function publish(origin: Origin.Producer, path: string, instance?: Instance.Valid) {
	const publication = origin.route(Path.from(path), { instance: instance && { id: instance, epoch: 0n } });
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

it("reads a listed include's catalog but no media, keeping its rendition IDs its own", async () => {
	const origin = new Origin.Producer();
	const parent = publish(origin, "room/main.hang");
	const include = publish(origin, "room/extra.hang");
	const includeCatalog = include.createTrack(Catalog.TRACK);
	const catalog = new Json.Snapshot.Producer<Catalog.Root>({ track: includeCatalog, deltas: false });
	parent.createTrack("video").writeFrame({ payload: new TextEncoder().encode("source frames") });
	const includeVideo = include.createTrack("video");
	includeVideo.writeFrame({ payload: new TextEncoder().encode("include frames") });

	// The include and a source rendition share the ID "video".
	const root = Catalog.RootSchema.parse({
		video: { renditions: { video: video("avc1.42001f") } },
		include: { video: { broadcast: "./extra.hang" } },
	});
	const source = new Broadcast({
		origin,
		name: Path.from("room/main.hang"),
		catalogFormat: "manual",
		catalog: root,
	});
	const selection = new Effect();
	try {
		catalog.update({ value: { video: { renditions: { video: video("vp8") } } } });
		await settle();
		expect(source.out.catalog.peek()?.include).toEqual(root.include);
		// The root selects every include it lists: it reads the catalog, but subscribes no media.
		expect(includeCatalog.demand().wanted.peek()).toBeDefined();
		expect(includeVideo.demand().wanted.peek()).toBeUndefined();
		// Listed under its include's ID, so it never shadows the root's own "video".
		expect(source.out.catalog.peek()?.video?.renditions).toEqual({
			video: video("avc1.42001f"),
			"video/video": { ...video("vp8"), broadcast: "extra.hang" as Path.Relative, track: "video" },
		});

		const child = source.include(selection, "video");
		await settle();
		expect(child.out.catalog.peek()?.video?.renditions.video?.codec).toBe("vp8");

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
			expect(await frames?.readString()).toBe("include frames");
			expect(await original?.readString()).toBe("source frames");
		} finally {
			frames?.close();
			original?.close();
		}

		// An unlisted ID leaves the child disabled.
		expect(source.include(selection, "unknown").in.enabled.peek()).toBe(false);

		// Dropping the selection releases the media; closing the root releases the catalog.
		selection.close();
		await unwanted(includeVideo);
		expect(includeCatalog.demand().wanted.peek()).toBeDefined();
		source.close();
		await unwanted(includeCatalog);
	} finally {
		selection.close();
		source.close();
		origin.close();
	}
});

it("reads an unavailable include as offline without touching the parent", async () => {
	const origin = new Origin.Producer();
	// A session that could announce the include, so its absence reads offline.
	wireOf(origin).attach(true);
	publish(origin, "room/main.hang");
	const claim = origin.claim(Path.from("room/absent.hang"));
	const requests = claim.requested();
	const source = new Broadcast({
		origin,
		name: Path.from("room/main.hang"),
		catalogFormat: "manual",
		catalog: Catalog.RootSchema.parse({
			video: { renditions: { hd: video("avc1.42001f") } },
			include: { missing: { broadcast: "./absent.hang" } },
		}),
		follow: false,
	});
	const selection = new Effect();
	try {
		await settle();
		const missing = source.include(selection, "missing");
		const { value: request } = await requests.next();
		expect(request?.path).toBe(Path.from("room/absent.hang"));
		request?.reject(new Error("include unavailable"));
		await settle();
		// Like a rendition's reference, an unavailable include is offline, not an error.
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

it("reports an include outside the origin's scope as an error, leaving the parent live", async () => {
	const origin = new Origin.Producer();
	const scoped = origin.scope(Path.empty(), new Path.Patterns([Path.Pattern.parse("room/*")]));
	publish(scoped, "room/main.hang");
	const parent = new Broadcast({
		origin: scoped,
		name: Path.from("room/main.hang"),
		catalogFormat: "manual",
		catalog: Catalog.RootSchema.parse({ include: { outside: { broadcast: "../other/extra.hang" } } }),
		follow: false,
	});
	const owner = new Effect();
	try {
		const child = parent.include(owner, "outside");
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

it("tracks the include entry without reopening it on unrelated catalog updates", async () => {
	const origin = new Origin.Producer();
	publish(origin, "room/main.hang");
	const firstCatalog = publish(origin, "room/first.hang").createTrack(Catalog.TRACK);
	publish(origin, "room/second.hang");
	const catalog = new Signal(Catalog.RootSchema.parse({ include: { extra: { broadcast: "./first.hang" } } }));
	const parent = new Broadcast({
		origin,
		name: Path.from("room/main.hang"),
		catalogFormat: "manual",
		catalog,
		follow: false,
	});
	const owner = new Effect();
	try {
		const child = parent.include(owner, "extra");
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
				include: { extra: { broadcast: "./first.hang" }, other: { broadcast: "./other.hang" } },
				metadata: "changed",
			}),
		);
		await settle();
		expect(child.out.active.peek()).toBe(active);
		expect(withdrawals).toBe(0);

		// A changed path is a different reference, so the child follows it.
		catalog.set(Catalog.RootSchema.parse({ include: { extra: { broadcast: "./second.hang" } } }));
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

it("follows a restarted contributor only for an entry that names no instance", async () => {
	const origin = new Origin.Producer();
	// A session that could announce the include, so its absence reads offline.
	wireOf(origin).attach(true);
	publish(origin, "room/main.hang");
	const [oldInstance, newInstance] = [Instance.random(), Instance.random()];
	const old = publish(origin, "room/extra.hang", oldInstance);
	const parent = new Broadcast({
		origin,
		name: Path.from("room/main.hang"),
		catalogFormat: "manual",
		catalog: Catalog.RootSchema.parse({
			include: {
				following: { broadcast: "./extra.hang" },
				pinned: { broadcast: "./extra.hang", instance: oldInstance },
			},
		}),
	});
	const owner = new Effect();
	try {
		const following = parent.include(owner, "following");
		const pinned = parent.include(owner, "pinned");
		await settle();
		expect(following.out.active.peek()?.instance).toBe(oldInstance);
		expect(pinned.out.active.peek()?.instance).toBe(oldInstance);

		// The announcement is the online signal: the plain entry follows the restart, while the
		// one naming the old instance goes offline rather than reading the new one.
		const replacement = publish(origin, "room/extra.hang", newInstance);
		await quietly(settle);
		expect(following.out.active.peek()?.instance).toBe(newInstance);
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

it("pins the first instance of an include when not following announcements", async () => {
	const origin = new Origin.Producer();
	publish(origin, "room/main.hang");
	const first = publish(origin, "room/extra.hang", Instance.random());
	const parent = new Broadcast({
		origin,
		name: Path.from("room/main.hang"),
		catalogFormat: "manual",
		catalog: Catalog.RootSchema.parse({ include: { extra: { broadcast: "./extra.hang" } } }),
		follow: false,
	});
	const owner = new Effect();
	try {
		const child = parent.include(owner, "extra");
		await settle();
		const pinned = child.out.active.peek();
		expect(pinned?.path).toBe(Path.from("room/extra.hang"));

		// A restart ends the pinned instance without moving the child to the new one.
		const second = publish(origin, "room/extra.hang", Instance.random());
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

it("never reads an include of another instance, but reads a minted one", async () => {
	const origin = new Origin.Producer();
	publish(origin, "room/main.hang");
	const served = Instance.random();
	const proven = publish(origin, "room/proven.hang", served);
	const track = proven.createTrack(Catalog.TRACK);
	// A route over a wire that carries no instance has one its session minted.
	publish(origin, "room/plain.hang", Mint.fresh());
	const named = Instance.random();
	const include = (instance: Instance.Valid) =>
		Catalog.RootSchema.parse({
			include: {
				proven: { broadcast: "./proven.hang", instance },
				plain: { broadcast: "./plain.hang", instance: named },
			},
		});
	const catalog = new Signal(include(named));
	const parent = new Broadcast({
		origin,
		name: Path.from("room/main.hang"),
		catalogFormat: "manual",
		catalog,
		follow: false,
	});
	const owner = new Effect();
	try {
		const refused = parent.include(owner, "proven");
		const accepted = parent.include(owner, "plain");
		await quietly(settle);
		expect(refused.out.active.peek()).toBeUndefined();
		expect(refused.out.error.peek()).toBeUndefined();
		expect(track.closed.peek()).toBeUndefined();
		expect(track.demand().wanted.peek()).toBeUndefined();
		// A minted instance cannot prove the stream, so the named instance cannot refuse it.
		expect(accepted.out.active.peek()?.path).toBe(Path.from("room/plain.hang"));

		// A catalog update naming the served instance admits the stream the request already holds.
		catalog.set(include(served));
		await settle();
		expect(refused.out.active.peek()?.instance).toBe(served);
	} finally {
		owner.close();
		parent.close();
		origin.close();
	}
});

it("shares the parent's resolution, so a new instance checks the held handle", async () => {
	const origin = new Origin.Producer();
	const [served, other] = [Instance.random(), Instance.random()];
	publish(origin, "room/main.hang", served);
	const extraCatalog = publish(origin, "room/extra.hang", served).createTrack(Catalog.TRACK);
	const entries = (instance?: Instance.Valid) =>
		Catalog.RootSchema.parse({
			include: {
				sibling: { broadcast: "./extra.hang", instance },
				self: { broadcast: "./main.hang", instance },
				empty: { broadcast: "", instance },
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
		const sibling = parent.include(owner, "sibling");
		const self = parent.include(owner, "self");
		await settle();
		const held = sibling.out.active.peek();
		expect(held?.instance).toBe(served);
		// A self-reference is the parent's own handle, not a request of its own. An empty one is
		// too, and is named like the parent, so its own references resolve against the same base.
		expect(self.out.active.peek()).toBe(parent.out.active.peek());
		const empty = parent.include(owner, "empty");
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

it("offers every include's renditions to selection, subscribing only the selected one", async () => {
	const origin = new Origin.Producer();
	const parent = publish(origin, "room/main.hang");
	const parentHd = parent.createTrack("hd");
	const served = Instance.random();
	const ladder = publish(origin, "room/ladder.hang", served);
	const ladderTrack = ladder.createTrack(Catalog.TRACK);
	const ladderCatalog = new Json.Snapshot.Producer<Catalog.Root>({ track: ladderTrack, deltas: false });
	const rung = (name: string) => {
		const track = ladder.createTrack(name);
		track.writeFrame({ payload: new TextEncoder().encode(name) });
		return track;
	};
	const [high, low, ladderHd] = [rung("720p"), rung("360p"), rung("hd")];
	const remote = publish(origin, "room/remote.hang").createTrack("remote");
	const nested = publish(origin, "room/nested.hang").createTrack(Catalog.TRACK);

	const sized = (width: number, height: number, bitrate: number): Catalog.VideoConfig =>
		Catalog.VideoConfigSchema.parse({ ...video("avc1.42001f"), codedWidth: width, codedHeight: height, bitrate });
	ladderCatalog.update({
		value: Catalog.RootSchema.parse({
			video: {
				renditions: {
					"720p": sized(1280, 720, 2_000_000),
					"360p": sized(640, 360, 600_000),
					// The source the ladder relists, read from the root itself: the same media, so left out.
					source: { ...sized(1920, 1080, 5_000_000), broadcast: "./main.hang", track: "hd" },
					// Its own track named like the root's rendition: other media, under its include's ID.
					hd: sized(1920, 1080, 4_000_000),
					// A reference of its own, rebased onto the root's broadcast.
					remote: { ...sized(320, 180, 200_000), broadcast: "./remote.hang" },
					// A reference back to the include itself still reads the stream its entry names.
					alt: { ...sized(426, 240, 300_000), broadcast: "./ladder.hang" },
				},
			},
			audio: {
				renditions: {
					aac: { codec: "mp4a.40.2", container: { kind: "legacy" }, sampleRate: 48000, numberOfChannels: 2 },
				},
			},
			// An include's own include needs a selection of its own, so it is never expanded.
			include: { nested: { broadcast: "./nested.hang" } },
		}),
	});

	const root = (own: Record<string, Catalog.VideoConfig>) =>
		Catalog.RootSchema.parse({
			video: { renditions: own },
			include: { ladder: { broadcast: "./ladder.hang", instance: served } },
		});
	const catalog = new Signal(root({ hd: sized(1920, 1080, 5_000_000) }));
	const broadcast = new Broadcast({ origin, name: Path.from("room/main.hang"), catalogFormat: "manual", catalog });
	const rate = new Signal<number | undefined>(1_000_000);
	const source = new Source({ broadcast, supported: async () => true, rate });
	const selection = new Effect();
	try {
		await settle();
		const ref = { broadcast: "ladder.hang" as Path.Relative, instance: served };
		expect(broadcast.out.catalog.peek()?.video?.renditions).toEqual({
			hd: sized(1920, 1080, 5_000_000),
			"ladder/720p": { ...sized(1280, 720, 2_000_000), ...ref, track: "720p" },
			"ladder/360p": { ...sized(640, 360, 600_000), ...ref, track: "360p" },
			"ladder/hd": { ...sized(1920, 1080, 4_000_000), ...ref, track: "hd" },
			"ladder/remote": {
				...sized(320, 180, 200_000),
				broadcast: "remote.hang" as Path.Relative,
				track: "remote",
			},
			"ladder/alt": { ...sized(426, 240, 300_000), ...ref, track: "alt" },
		});
		expect(Object.keys(broadcast.out.catalog.peek()?.audio?.renditions ?? {})).toEqual(["ladder/aac"]);
		expect(nested.demand().wanted.peek()).toBeUndefined();
		// Listing a rendition subscribes none of its media.
		for (const track of [parentHd, high, low, ladderHd, remote])
			expect(track.demand().wanted.peek()).toBeUndefined();

		// One selection spans the root and its include, and the config says where the media lives.
		expect(source.out.track.peek()).toBe("ladder/360p");
		rate.set(10_000_000);
		await settle();
		expect(source.out.track.peek()).toBe("hd");
		rate.set(3_000_000);
		await settle();
		const selected = source.out.config.peek();
		expect(source.out.track.peek()).toBe("ladder/720p");
		expect(selected?.broadcast).toBe(ref.broadcast);

		// Reading the selected rendition, as the decoder does, subscribes that track alone.
		const frames = broadcast
			.relativeBroadcast(selection, selected?.broadcast, selected?.instance)
			?.track(Catalog.track("ladder/720p", selected ?? {}))
			.subscribe()
			.ordered();
		try {
			expect(await frames?.readString()).toBe("720p");
			for (const track of [parentHd, low, ladderHd, remote]) expect(track.demand().wanted.peek()).toBeUndefined();
		} finally {
			frames?.close();
		}

		// The root's own ID always wins over an include's.
		catalog.set(root({ hd: sized(1920, 1080, 5_000_000), "ladder/360p": sized(640, 360, 500_000) }));
		await settle();
		expect(broadcast.out.catalog.peek()?.video?.renditions["ladder/360p"]).toEqual(sized(640, 360, 500_000));

		// Dropping the entry drops its renditions and releases its catalog.
		catalog.set(Catalog.RootSchema.parse({ video: { renditions: { hd: sized(1920, 1080, 5_000_000) } } }));
		await settle();
		expect(Object.keys(broadcast.out.catalog.peek()?.video?.renditions ?? {})).toEqual(["hd"]);
		expect(broadcast.out.catalog.peek()?.audio).toBeUndefined();
		await unwanted(ladderTrack);
	} finally {
		selection.close();
		source.close();
		broadcast.close();
		origin.close();
	}
});

it("dedupes an include's rendition only against a usable one", async () => {
	const origin = new Origin.Producer();
	publish(origin, "room/main.hang");
	const [oldInstance, newInstance] = [Instance.random(), Instance.random()];
	const serve = (instance: Instance.Valid) => {
		const broadcast = publish(origin, "room/extra.hang", instance);
		const catalog = new Json.Snapshot.Producer<Catalog.Root>({
			track: broadcast.createTrack(Catalog.TRACK),
			deltas: false,
		});
		catalog.update({ value: Catalog.RootSchema.parse({ video: { renditions: { v: video("vp8") } } }) });
		return broadcast;
	};
	const old = serve(oldInstance);
	const parent = new Broadcast({
		origin,
		name: Path.from("room/main.hang"),
		catalogFormat: "manual",
		catalog: Catalog.RootSchema.parse({
			video: { renditions: { v: { ...video("vp8"), broadcast: "./extra.hang", instance: oldInstance } } },
			include: { extra: { broadcast: "./extra.hang" } },
		}),
	});
	try {
		await settle();
		// The root already lists the include's media, so it is listed once.
		expect(Object.keys(parent.out.catalog.peek()?.video?.renditions ?? {})).toEqual(["v"]);

		// A restart proves the root's entry stale, while the include follows it.
		const replacement = serve(newInstance);
		await quietly(settle);
		expect(parent.out.catalog.peek()?.video?.renditions).toEqual({
			"extra/v": { ...video("vp8"), broadcast: "extra.hang" as Path.Relative, track: "v" },
		});
		replacement.close();
	} finally {
		parent.close();
		old.close();
		origin.close();
	}
});
