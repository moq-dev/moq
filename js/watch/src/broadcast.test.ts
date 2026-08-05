import { describe, expect, it } from "bun:test";
import type * as Catalog from "@moq/hang/catalog";
import type * as Moq from "@moq/net";
import { Path } from "@moq/net";
import { Effect } from "@moq/signals";
import { Broadcast } from "./broadcast";

// A consumer stub that only remembers which path it was consumed from.
type Consumed = { path: Moq.Path.Valid; close: () => void };

// The connection is only asked to consume a path; `reload: false` skips the announcement
// stream and `catalogFormat: "manual"` skips the catalog fetch.
function connection(): Moq.Connection.Established {
	return {
		consume: (path: Moq.Path.Valid): Consumed => ({ path, close: () => {} }),
	} as unknown as Moq.Connection.Established;
}

function broadcast(name: string): Broadcast {
	return new Broadcast({
		connection: connection(),
		name: Path.from(name),
		enabled: true,
		reload: false,
		catalogFormat: "manual",
	});
}

function consumed(value: Moq.Broadcast.Consumer | undefined): string | undefined {
	return (value as unknown as Consumed | undefined)?.path;
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
	it("resolves a legal reference against the catalog broadcast", () => {
		const source = broadcast("a/b");
		const effect = new Effect();
		try {
			expect(consumed(source.relativeBroadcast(effect, "../source"))).toBe("a/source");
			expect(consumed(source.relativeBroadcast(effect, "sub"))).toBe("a/b/sub");
		} finally {
			effect.close();
			source.close();
		}
	});

	it("ignores a reference that escapes above the root", () => {
		const source = broadcast("a/b");
		const effect = new Effect();
		try {
			// Clamping would subscribe to an unrelated `x` instead of dropping the rendition.
			withoutWarnings(() => {
				expect(source.relativeBroadcast(effect, "../../../x")).toBeUndefined();
				expect(source.relativeBroadcast(effect, "../../../..")).toBeUndefined();
			});
			// Popping to exactly the root stops at it, and the root still names a broadcast.
			expect(consumed(source.relativeBroadcast(effect, "../.."))).toBe(Path.empty());
		} finally {
			effect.close();
			source.close();
		}
	});

	const rendition = (broadcast?: string): Catalog.VideoConfig =>
		({ codec: "avc1.42001f", container: { kind: "legacy" }, broadcast }) as Catalog.VideoConfig;

	const manualCatalog = (catalog: Catalog.Root) =>
		new Broadcast({
			connection: connection(),
			name: Path.from("a/b"),
			enabled: true,
			reload: false,
			catalogFormat: "manual",
			catalog,
		});

	const manual = (renditions: Record<string, Catalog.VideoConfig>) =>
		manualCatalog({ video: { renditions } } as Catalog.Root);

	it("rejects a catalog carrying an escaping rendition", async () => {
		// The whole catalog goes, not just the offending rendition: the root is this
		// consumer's authorized subtree, so the reference names content it cannot reach,
		// and serving the rest would hide a publisher bug behind a track that never fills.
		const source = manual({ good: rendition(), sibling: rendition("../source"), bad: rendition("../../../x") });

		const error = console.error;
		console.error = () => {};
		try {
			// The catalog is validated by an effect, which settles a microtask later.
			await Promise.resolve();
			expect(source.out.catalog.peek()).toBeUndefined();
		} finally {
			console.error = error;
			source.close();
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
		const source = manualCatalog({
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
		}
	});

	it("accepts a catalog whose references stay within the root", async () => {
		const source = manual({ good: rendition(), sibling: rendition("../source"), root: rendition("../..") });

		try {
			await Promise.resolve();
			const renditions = source.out.catalog.peek()?.video?.renditions ?? {};
			expect(Object.keys(renditions).sort()).toEqual(["good", "root", "sibling"]);
		} finally {
			source.close();
		}
	});

	it("uses the catalog's own broadcast when the reference is absent, empty, or self", async () => {
		const source = broadcast("a/b");
		const effect = new Effect();
		try {
			// The catalog broadcast is consumed by an effect, which settles a microtask later.
			await Promise.resolve();
			expect(consumed(source.relativeBroadcast(effect, undefined))).toBe("a/b");
			expect(consumed(source.relativeBroadcast(effect, ""))).toBe("a/b");
			expect(consumed(source.relativeBroadcast(effect, "../b"))).toBe("a/b");
		} finally {
			effect.close();
			source.close();
		}
	});
});
