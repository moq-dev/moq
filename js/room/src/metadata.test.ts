import { expect, test } from "bun:test";
import { TRACK, userFields } from "./metadata.ts";

test("userFields seeds from values", () => {
	const user = userFields({ id: "a", name: "Ada", avatar: "ada.png" });
	expect(user.id.peek()).toBe("a");
	expect(user.name.peek()).toBe("Ada");
	expect(user.avatar.peek()).toBe("ada.png");
	expect(user.color.peek()).toBeUndefined();
});

test("core tracks are hang/*.json and extras share the section", () => {
	expect(TRACK.user).toBe("hang/user.json");
	expect(TRACK.preview).toBe("hang/preview.json");
	expect(TRACK.chat).toBe("hang/chat.json");
	expect(TRACK.location).toBe("hang/location.json");
});

import * as Json from "@moq/json";
import * as Net from "@moq/net";
import type * as Publish from "@moq/publish";
import { Effect, Signal } from "@moq/signals";
import type * as Watch from "@moq/watch";
import { consume, type ExtendedCatalog, type Preview, serve } from "./metadata.ts";

// Drain queued effects and their asynchronous subscription continuations.
async function flush() {
	for (let i = 0; i < 30; i++) await Promise.resolve();
}

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
		const consumer = new Json.Snapshot.Consumer<{ name: string }>(track);
		expect((await consumer.next())?.name).toBe("Alice");
		user.name.set("Bob");
		await flush();
		expect((await consumer.next())?.name).toBe("Bob");
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
	const user = new Json.Snapshot.Producer({ track: net.createTrack(TRACK.user) });
	const preview = new Json.Snapshot.Producer({ track: net.createTrack(TRACK.preview) });
	user.update({ name: "Alice" });
	preview.update({ info: { video: true } });
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
