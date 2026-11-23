import { expect, spyOn, test } from "bun:test";
import * as Json from "@moq/json";
import * as Net from "@moq/net";
import type * as Publish from "@moq/publish";
import { Effect, Signal } from "@moq/signals";
import type * as Watch from "@moq/watch";
import { defined } from "./defined.test-util.ts";
import { consume, type ExtendedCatalog, type Preview, serve, TRACK, userFields } from "./metadata.ts";

// Drain queued effects and their asynchronous subscription continuations.
async function flush() {
	for (let i = 0; i < 30; i++) await Promise.resolve();
}

// Track names are wire identifiers shared with every other publisher and subscriber.
test("track names match the hang catalog section", () => {
	expect({ ...TRACK }).toEqual({
		user: "hang/user.json",
		preview: "hang/preview.json",
		chat: "hang/chat.json",
		location: "hang/location.json",
	});
});

test("metadata updates keep the same track and reach an existing subscriber", async () => {
	const net = new Net.Broadcast.Producer();
	const catalog = new Signal<ExtendedCatalog>({});
	const broadcast = { net: new Signal(net), catalog } as unknown as Publish.Broadcast;
	const user = userFields({ name: "Alice" });
	const effect = new Effect();
	try {
		serve(broadcast, user, new Signal<Preview>({}), effect);
		await flush();
		const consumer = new Json.Snapshot.Consumer<{ name: string }>({ track: net.consume().track(TRACK.user) });
		expect((await consumer.latest())?.value.name).toBe("Alice");
		user.name.set("Bob");
		await flush();
		expect((await consumer.latest())?.value.name).toBe("Bob");
		consumer.close();
	} finally {
		effect.close();
		net.close();
	}
});

test("metadata clears when entries disappear or broadcast becomes inactive", async () => {
	const net = new Net.Broadcast.Producer();
	const catalog = new Signal<ExtendedCatalog | undefined>({
		hang: { user: { track: TRACK.user }, preview: { track: TRACK.preview } },
	});
	const active = new Signal<Net.Broadcast.Consumer | undefined>(net.consume());
	const user = new Json.Snapshot.Producer({
		track: defined(net.createTrack(TRACK.user, { timescale: Net.Time.Timescale.MILLI })),
	});
	const preview = new Json.Snapshot.Producer({
		track: defined(net.createTrack(TRACK.preview, { timescale: Net.Time.Timescale.MILLI })),
	});
	user.update({ name: "Alice" }, Net.Time.Timestamp.now());
	preview.update({ info: { video: true } }, Net.Time.Timestamp.now());
	const consumed = consume({ out: { catalog, active } } as unknown as Watch.Broadcast);
	try {
		await flush();
		expect(consumed.user.name.peek()).toBe("Alice");
		expect(consumed.preview.peek()).toEqual({ video: true });
		catalog.set({ hang: { preview: { track: TRACK.preview } } });
		await flush();
		expect(consumed.user.name.peek()).toBeUndefined();
		expect(consumed.preview.peek()).toEqual({ video: true });
		active.set(undefined);
		await flush();
		expect(consumed.preview.peek()).toEqual({});
	} finally {
		consumed.close();
		user.finish();
		preview.finish();
		net.close();
	}
});

test("a value that does not match its schema ends that track's subscription only", async () => {
	const net = new Net.Broadcast.Producer();
	const catalog = new Signal<ExtendedCatalog | undefined>({
		hang: { user: { track: TRACK.user }, preview: { track: TRACK.preview } },
	});
	const active = new Signal<Net.Broadcast.Consumer | undefined>(net.consume());
	// Unvalidated producers, standing in for a remote publisher that sends anything.
	const user = new Json.Snapshot.Producer<unknown>({
		track: defined(net.createTrack(TRACK.user, { timescale: Net.Time.Timescale.MILLI })),
	});
	const preview = new Json.Snapshot.Producer<unknown>({
		track: defined(net.createTrack(TRACK.preview, { timescale: Net.Time.Timescale.MILLI })),
	});
	const error = spyOn(console, "error").mockImplementation(() => {});
	const consumed = consume({ out: { catalog, active } } as unknown as Watch.Broadcast);
	try {
		// Each write opens a group at its time, so the writes take distinct ones.
		user.update({ name: "Alice" }, Net.Time.Timestamp.fromMillis(1));
		preview.update({ info: { video: true } }, Net.Time.Timestamp.fromMillis(1));
		await flush();
		expect(consumed.user.name.peek()).toBe("Alice");
		expect(consumed.preview.peek()).toEqual({ video: true });

		// A refused value ends its subscription: the last valid value stays and later ones are never read.
		user.update({ name: 42 }, Net.Time.Timestamp.fromMillis(2));
		await flush();
		expect(error).toHaveBeenCalledTimes(1);
		user.update({ name: "Bob" }, Net.Time.Timestamp.fromMillis(3));
		await flush();
		expect(consumed.user.name.peek()).toBe("Alice");

		// The other track is its own subscription.
		preview.update({ info: { video: false } }, Net.Time.Timestamp.fromMillis(2));
		await flush();
		expect(consumed.preview.peek()).toEqual({ video: false });
	} finally {
		consumed.close();
		error.mockRestore();
		user.finish();
		preview.finish();
		net.close();
	}
});

test("an unrelated catalog update keeps the metadata subscriptions", async () => {
	const net = new Net.Broadcast.Producer();
	const hang = { user: { track: TRACK.user }, preview: { track: TRACK.preview } };
	const catalog = new Signal<ExtendedCatalog | undefined>({ hang });
	const active = new Signal<Net.Broadcast.Consumer | undefined>(net.consume());
	const user = new Json.Snapshot.Producer({
		track: defined(net.createTrack(TRACK.user, { timescale: Net.Time.Timescale.MILLI })),
	});
	const preview = new Json.Snapshot.Producer({
		track: defined(net.createTrack(TRACK.preview, { timescale: Net.Time.Timescale.MILLI })),
	});
	user.update({ name: "Alice" }, Net.Time.Timestamp.now());
	preview.update({ info: { video: true } }, Net.Time.Timestamp.now());
	const consumed = consume({ out: { catalog, active } } as unknown as Watch.Broadcast);
	const names: (string | undefined)[] = [];
	const previews: Preview[] = [];
	const watcher = new Effect();
	watcher.run((effect) => {
		names.push(effect.get(consumed.user.name));
	});
	watcher.run((effect) => {
		previews.push(effect.get(consumed.preview));
	});
	try {
		await flush();
		expect(names.at(-1)).toBe("Alice");
		const seen = [names.length, previews.length];

		// A new rendition and an app's chat entry leave both track names alone.
		catalog.set({ video: { renditions: {} }, hang: { ...hang, chat: { track: TRACK.chat } } });
		await flush();
		expect([names.length, previews.length]).toEqual(seen);

		// Renaming one track resubscribes only that one.
		const other = defined(net.createTrack("other.json", { timescale: Net.Time.Timescale.MILLI }));
		catalog.set({ hang: { ...hang, user: { track: "other.json" } } });
		await flush();
		expect(names.at(-1)).toBeUndefined();
		expect(previews.length).toBe(seen[1]);
		other.close();
	} finally {
		watcher.close();
		consumed.close();
		user.finish();
		preview.finish();
		net.close();
	}
});

test("a malformed hang catalog section is refused as absent", async () => {
	const net = new Net.Broadcast.Producer();
	const catalog = new Signal<ExtendedCatalog | undefined>(
		JSON.parse('{"hang":{"user":{"track":5},"preview":"hang/preview.json"}}'),
	);
	const active = new Signal<Net.Broadcast.Consumer | undefined>(net.consume());
	const warn = spyOn(console, "warn").mockImplementation(() => {});
	const consumed = consume({ out: { catalog, active } } as unknown as Watch.Broadcast);
	try {
		await flush();
		expect(consumed.user.name.peek()).toBeUndefined();
		expect(consumed.preview.peek()).toEqual({});
		expect(warn).toHaveBeenCalledTimes(1);
	} finally {
		consumed.close();
		warn.mockRestore();
		net.close();
	}
});
