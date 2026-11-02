import { expect, test } from "bun:test";
import { Manual, Milli, Nano, yieldNow } from "@moq/clock";
import { Echo as Schema } from "@moq/hang";
import type * as Catalog from "@moq/hang/catalog";
import * as Json from "@moq/json";
import { Origin, Path } from "@moq/net";
import { Signal } from "@moq/signals";
import { Broadcast } from "./broadcast";
import { checkName, Echo } from "./echo";
import type { Received } from "./feedback";

const flush = async (): Promise<void> => {
	for (let i = 0; i < 5; i++) await yieldNow();
};

// A watched broadcast whose catalog the test sets directly.
function watched(origin: Origin.Producer, name: string, catalog: Signal<Catalog.Root | undefined>): Broadcast {
	return new Broadcast({ origin, name: Path.from(name), follow: false, catalogFormat: "manual", catalog });
}

// A decoder that reports `received` for the catalog at `path`, and nothing for any other.
function source(path: string, received: Record<string, Received>) {
	return { feedback: (base: Path.Valid) => (base === path ? received : {}) };
}

// The feedback broadcasts announced on `origin`, by path.
function announced(origin: Origin.Producer): Set<string> {
	const paths = new Set<string>();
	const listing = origin.announced(Path.Pattern.subtree(Path.empty()));
	void (async () => {
		for (;;) {
			const entry = await listing.next();
			if (!entry) break;
			if (entry.kind === "end") paths.delete(entry.prefix);
			else paths.add(entry.prefix);
		}
	})();
	return paths;
}

// Read the newest feedback report published at `path`.
async function read(origin: Origin.Producer, path: string, clock: Manual): Promise<Schema.Snapshot | undefined> {
	const request = origin.request(Path.from(path));
	await flush();
	const broadcast = request.active.peek();
	if (!broadcast) throw new Error(`no feedback at ${path}`);
	const track = broadcast.track(Schema.TRACK).subscribe();
	const consumer = new Json.Snapshot.Consumer<Schema.Snapshot>({ track, schema: Schema.SnapshotSchema });
	await flush();
	await clock.advance(Nano.fromMilli(Milli(1000)));
	const value = (await consumer.latest())?.value;
	track.close();
	request.close();
	return value;
}

test("refuses a name that is not one visible path segment", () => {
	for (const name of ["", "a/b", ".", "..", "*", "**", ".hidden"]) expect(() => checkName(name)).toThrow();
	expect(checkName("alice")).toBe("alice");
});

test("publishes feedback where the catalog solicits it and follows the solicitation", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const origin = new Origin.Producer();
	const paths = announced(origin);
	const catalog = new Signal<Catalog.Root | undefined>({});
	const broadcast = watched(origin, "room/live.hang", catalog);
	const echo = new Echo({
		broadcast,
		origin,
		name: "alice",
		probe: { rtt: Milli(20) },
		sources: [
			source("room/live.hang", { video: { frames: 2, bytes: 20 } }),
			source("room/live.hang", { audio: { underruns: 1 } }),
		],
	});
	try {
		await flush();
		// Nothing is solicited, so nothing is published.
		expect([...paths]).toEqual([]);

		catalog.set({ echo: { path: "viewers" } } as Catalog.Root);
		await flush();
		expect([...paths]).toEqual(["room/viewers/alice.echo"]);
		expect((await read(origin, "room/viewers/alice.echo", clock)) as unknown).toEqual({
			transport: { rtt: 20 },
			renditions: { video: { frames: 2, bytes: 20 }, audio: { underruns: 1 } },
		});

		// A moved path retracts the broadcast it replaces.
		catalog.set({ echo: { path: "live/viewers" } } as Catalog.Root);
		await flush();
		expect([...paths]).toEqual(["room/live/viewers/alice.echo"]);

		// A removed section retracts it.
		catalog.set({});
		await flush();
		expect([...paths]).toEqual([]);
	} finally {
		echo.close();
		broadcast.close();
		origin.close();
		installed.close();
	}
});

test("a second reporter at a feedback path in use publishes nothing until it is free", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const origin = new Origin.Producer();
	const paths = announced(origin);
	const catalog = new Signal<Catalog.Root | undefined>({ echo: { path: "viewers" } } as Catalog.Root);
	const first = watched(origin, "room/live.hang", catalog);
	const second = watched(origin, "room/live.hang", catalog);
	const enabled = new Signal(true);
	const reporters = [
		new Echo({ broadcast: first, origin, name: "alice", sources: [source("room/live.hang", { video: { frames: 1 } })] }),
		new Echo({
			broadcast: second,
			origin,
			enabled,
			name: "alice",
			sources: [source("room/live.hang", { video: { frames: 10 } })],
		}),
	];
	try {
		await flush();
		expect([...paths]).toEqual(["room/viewers/alice.echo"]);
		// Only the first reporter's own counts, never a sum.
		expect((await read(origin, "room/viewers/alice.echo", clock)) as unknown).toEqual({
			renditions: { video: { frames: 1 } },
		});

		// The refused one leaving changes nothing.
		enabled.set(false);
		await flush();
		expect([...paths]).toEqual(["room/viewers/alice.echo"]);
		expect((await read(origin, "room/viewers/alice.echo", clock))?.renditions?.video.frames as unknown).toBe(1);

		// The holder leaving retracts it and frees the path for a reporter that asks again.
		reporters[0].close();
		await flush();
		expect([...paths]).toEqual([]);
		enabled.set(true);
		await flush();
		expect([...paths]).toEqual(["room/viewers/alice.echo"]);
		expect((await read(origin, "room/viewers/alice.echo", clock))?.renditions?.video.frames as unknown).toBe(10);
	} finally {
		for (const reporter of reporters) reporter.close();
		first.close();
		second.close();
		origin.close();
		installed.close();
	}
});

test("a reporter for another catalog at a path in use publishes nothing", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const origin = new Origin.Producer();
	const paths = announced(origin);
	const catalog = new Signal<Catalog.Root | undefined>({ echo: { path: "viewers" } } as Catalog.Root);
	const a = watched(origin, "room/a.hang", catalog);
	const b = watched(origin, "room/b.hang", catalog);
	const reporters = [
		new Echo({ broadcast: a, origin, name: "alice", sources: [source("room/a.hang", { video: { frames: 2 } })] }),
		new Echo({ broadcast: b, origin, name: "alice", sources: [source("room/b.hang", { video: { frames: 100 } })] }),
	];
	try {
		await flush();
		expect([...paths]).toEqual(["room/viewers/alice.echo"]);
		// Only the first catalog's counts, never a sum across catalogs.
		expect((await read(origin, "room/viewers/alice.echo", clock)) as unknown).toEqual({
			renditions: { video: { frames: 2 } },
		});
	} finally {
		for (const reporter of reporters) reporter.close();
		a.close();
		b.close();
		origin.close();
		installed.close();
	}
});
