import { describe, expect, it } from "bun:test";
import type * as Catalog from "@moq/hang/catalog";
import * as Moq from "@moq/net";
import { Origin, Path } from "@moq/net";
import { Effect } from "@moq/signals";
import { Broadcast } from "./broadcast";

// A real origin with local broadcasts at the given paths. Resolution is proven by
// discrimination: `relativeBroadcast` resolves blind against the table (reload: false), so
// a defined result means the reference resolved to a published path and nothing else.
function origin(paths: string[]): Origin.Producer {
	const producer = new Origin.Producer();
	for (const path of paths) producer.publish(Path.from(path));
	return producer;
}

function broadcast(name: string, paths: string[] = [name]): { source: Broadcast; owner: Origin.Producer } {
	const owner = origin(paths);
	const source = new Broadcast({
		origin: owner,
		name: Path.from(name),
		enabled: true,
		reload: false,
		catalogFormat: "manual",
	});
	return { source, owner };
}

function withoutWarnings<T>(fn: () => T): T {
	const warn = console.warn;
	console.warn = () => {};
	try {
		return fn();
	} finally {
		console.warn = warn;
	}
}

describe("relativeBroadcast", () => {
	it("resolves a legal reference against the origin", () => {
		const { source, owner } = broadcast("a/b", ["a/b", "a/source", "a/b/sub"]);
		const effect = new Effect();
		try {
			expect(source.relativeBroadcast(effect, "../source")).toBeDefined();
			expect(source.relativeBroadcast(effect, "sub")).toBeDefined();
			// Nothing routes an unpublished sibling, so the reference stays pending.
			expect(source.relativeBroadcast(effect, "../missing")).toBeUndefined();
		} finally {
			effect.close();
			source.close();
			owner.close();
		}
	});

	it("ignores a reference that escapes above the root", () => {
		const { source, owner } = broadcast("a/b", ["a/b", "x", ""]);
		const effect = new Effect();
		try {
			// Clamping would subscribe to an unrelated `x` instead of dropping the rendition;
			// `x` is published, so a defined result here would prove the clamp bug.
			withoutWarnings(() => {
				expect(source.relativeBroadcast(effect, "../../../x")).toBeUndefined();
				expect(source.relativeBroadcast(effect, "../../../..")).toBeUndefined();
			});
			// Popping to exactly the root stops at it, and the root still names a broadcast.
			expect(source.relativeBroadcast(effect, "../..")).toBeDefined();
		} finally {
			effect.close();
			source.close();
			owner.close();
		}
	});

	const rendition = (broadcast?: string): Catalog.VideoConfig =>
		({ codec: "avc1.42001f", container: { kind: "legacy" }, broadcast }) as Catalog.VideoConfig;

	const manualCatalog = (catalog: Catalog.Root) => {
		const owner = origin(["a/b"]);
		const source = new Broadcast({
			origin: owner,
			name: Path.from("a/b"),
			enabled: true,
			reload: false,
			catalogFormat: "manual",
			catalog,
		});
		return { source, owner };
	};

	const manual = (renditions: Record<string, Catalog.VideoConfig>) =>
		manualCatalog({ video: { renditions } } as Catalog.Root);

	it("rejects a catalog carrying an escaping rendition", async () => {
		// The whole catalog goes, not just the offending rendition: the root is this
		// consumer's authorized subtree, so the reference names content it cannot reach,
		// and serving the rest would hide a publisher bug behind a track that never fills.
		const { source, owner } = manual({
			good: rendition(),
			sibling: rendition("../source"),
			bad: rendition("../../../x"),
		});

		const error = console.error;
		console.error = () => {};
		try {
			// The catalog is validated by an effect, which settles a microtask later.
			await Promise.resolve();
			expect(source.out.catalog.peek()).toBeUndefined();
		} finally {
			console.error = error;
			source.close();
			owner.close();
		}
	});

	it("rejects a catalog whose text rendition escapes the root", async () => {
		// The containment check covers every section carrying renditions: a text (caption)
		// reference escaping the root rejects the catalog like a video or audio one.
		const captions = {
			format: "vtt",
			role: "subtitle",
			container: { kind: "legacy" },
			broadcast: "../../../x",
		} as Catalog.TextConfig;
		const { source, owner } = manualCatalog({
			video: { renditions: { good: rendition() } },
			text: { renditions: { captions } },
		} as Catalog.Root);

		const error = console.error;
		console.error = () => {};
		try {
			await Promise.resolve();
			expect(source.out.catalog.peek()).toBeUndefined();
		} finally {
			console.error = error;
			source.close();
			owner.close();
		}
	});

	it("accepts a catalog whose references stay within the root", async () => {
		const { source, owner } = manual({
			good: rendition(),
			sibling: rendition("../source"),
			root: rendition("../.."),
		});

		try {
			await Promise.resolve();
			const renditions = source.out.catalog.peek()?.video?.renditions ?? {};
			expect(Object.keys(renditions).sort()).toEqual(["good", "root", "sibling"]);
		} finally {
			source.close();
			owner.close();
		}
	});

	it("uses the catalog's own broadcast when the reference is absent, empty, or self", async () => {
		const { source, owner } = broadcast("a/b");
		const effect = new Effect();
		try {
			// The catalog broadcast is consumed by an effect, which settles a microtask later.
			await Promise.resolve();
			const own = source.out.active.peek();
			expect(own).toBeDefined();
			expect(source.relativeBroadcast(effect, undefined)).toBe(own);
			expect(source.relativeBroadcast(effect, "")).toBe(own);
			expect(source.relativeBroadcast(effect, "../b")).toBe(own);
		} finally {
			effect.close();
			source.close();
			owner.close();
		}
	});
});

describe("blind resolution", () => {
	it("holds a resolved request steady instead of flapping", async () => {
		// reload: false with nothing routed stands a request; when a session answers, the
		// effect that read `request.active` reruns. That rerun must re-acquire the same
		// answer, not close the request and re-dial forever.
		const owner = new Origin.Producer();
		const source = new Broadcast({
			origin: owner,
			name: Path.from("blind.hang"),
			enabled: true,
			reload: false,
			catalogFormat: "manual",
		});

		const settle = () => new Promise((resolve) => setTimeout(resolve, 0));
		await settle();

		// Stand in for a session's serving loop answering the request.
		const upstream = new Moq.Broadcast.Producer();
		const withdraw = owner.answer(Path.from("blind.hang"), upstream.consume());
		expect(withdraw).toBeDefined();

		await settle();
		const active = source.out.active.peek();
		expect(active).toBeDefined();

		// Several tick boundaries later the same front is still held and the answer was
		// never withdrawn; a flap would close the upstream and vacate the request.
		for (let i = 0; i < 5; i++) await settle();
		expect(source.out.active.peek()).toBe(active);
		expect(upstream.closed.peek()).toBeUndefined();

		source.close();
		owner.close();
		await settle();
	});
});
