import { expect, it } from "bun:test";
import * as Catalog from "@moq/hang/catalog";
import { Origin, Path } from "@moq/net";
import { Effect } from "@moq/signals";
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
		expect(source.contribution(selection, "unknown")).toBeUndefined();
		const absent = origin.dynamic(Path.from("room/absent.hang"));
		selection.cleanup(() => absent.close());
		const requests = absent.requested();
		const missing = source.contribution(selection, "missing");
		if (!missing) throw new Error("missing contribution selection");
		const { value: request } = await requests.next();
		expect(request?.path).toBe(Path.from("room/absent.hang"));
		request?.reject(new Error("contribution unavailable"));
		await missing.out.error.changed();
		expect(missing.out.active.peek()).toBeUndefined();
		expect(missing.out.error.peek()?.message).toBe("contribution unavailable");
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
	for (const entry of [null, {}, { broadcast: 42 }]) {
		expect(() => Catalog.RootSchema.parse({ contributions: { extra: entry } })).toThrow();
	}
	const catalog = Catalog.RootSchema.parse({ contributions: { extra: { broadcast: "../../escape" } } });
	expect(() => Catalog.checkResolvable(catalog, Path.from("room/main.hang"))).toThrow(Catalog.EscapingBroadcast);
});
