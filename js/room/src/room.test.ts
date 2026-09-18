import { expect, mock, spyOn, test } from "bun:test";
import * as Net from "@moq/net";
import { Signal } from "@moq/signals";

// Vite's worklet loader is not available in Bun; discovery does not start audio.
mock.module("../../watch/src/audio/render-worklet.ts?worklet", () => ({ default: "blob:fake-render" }));
const { Remote } = await import("./remote.ts");
const { Room } = await import("./room.ts");

async function flush() {
	for (let i = 0; i < 30; i++) await Promise.resolve();
}

test("room restores the announce prefix and reconciles local identity changes", async () => {
	const streams: Net.Announce.Producer[] = [];
	const connection = {
		origin: new Signal({
			announced(scope: Net.Path.Pattern) {
				expect(scope.equals(Net.Path.Pattern.subtree(Net.Path.from("room-a")))).toBe(true);
				const stream = new Net.Announce.Producer();
				streams.push(stream);
				stream.append({
					path: Net.Path.from("room-a/bob/camera.hang"),
					kind: "announced",
					route: { hops: [], cost: { warm: 0n, cold: 0n } },
				});
				return stream.consume();
			},
		}),
	} as unknown as Net.Connection;
	const attach = spyOn(Remote.prototype, "attach").mockImplementation(() => {});
	const identity = new Signal(Net.Path.from("alice"));
	const room = new Room({ connection, identity, prefix: Net.Path.from("room-a") });
	try {
		await flush();
		expect(attach).toHaveBeenCalledWith("camera", Net.Path.from("room-a/bob/camera.hang"));
		expect(room.remotes.peek().has(Net.Path.from("bob"))).toBe(true);
		identity.set(Net.Path.from("bob"));
		await flush();
		expect(streams).toHaveLength(2);
		expect(room.remotes.peek().size).toBe(0);
	} finally {
		room.close();
		attach.mockRestore();
	}
});
