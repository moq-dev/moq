import { expect, test } from "bun:test";
import * as Json from "@moq/json";
import * as Net from "@moq/net";
import type * as Publish from "@moq/publish";
import { Effect, Signal } from "@moq/signals";
import type * as Watch from "@moq/watch";
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
		const track = net.consume().track(TRACK.user).subscribe();
		const consumer = new Json.Snapshot.Consumer<{ name: string }>({ track });
		expect((await consumer.latest())?.value.name).toBe("Alice");
		user.name.set("Bob");
		await flush();
		expect((await consumer.latest())?.value.name).toBe("Bob");
		track.close();
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
		track: net.createTrack(TRACK.user, { timescale: Net.Time.Timescale.MILLI }),
	});
	const preview = new Json.Snapshot.Producer({
		track: net.createTrack(TRACK.preview, { timescale: Net.Time.Timescale.MILLI }),
	});
	user.update({ value: { name: "Alice" }, at: Net.Time.Timestamp.now() });
	preview.update({ value: { info: { video: true } }, at: Net.Time.Timestamp.now() });
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
