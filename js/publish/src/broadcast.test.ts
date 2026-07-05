import { expect, test } from "bun:test";
import * as Moq from "@moq/net";
import { Signal } from "@moq/signals";
import { Broadcast } from "./broadcast";

// A class (not an object literal) so Signal equality compares by identity and
// swapping one fake for another re-runs the publish effect.
class FakeEstablished implements Moq.Connection.Established {
	readonly url = new URL("http://localhost");
	readonly version = "fake";
	published: Moq.Broadcast | undefined;

	announced(): Moq.Announced {
		throw new Error("unused");
	}
	publish(_path: Moq.Path.Valid, broadcast: Moq.Broadcast): void {
		this.published = broadcast;
	}
	consume(): Moq.Broadcast {
		throw new Error("unused");
	}
	close(): void {}
	closed = new Promise<void>(() => {});
}

async function waitFor(cond: () => boolean, ms = 1000): Promise<void> {
	const start = Date.now();
	while (!cond()) {
		if (Date.now() - start > ms) throw new Error("waitFor timed out");
		await new Promise((resolve) => setTimeout(resolve, 1));
	}
}

test("requestCount tracks subscribe requests and resets per session", async () => {
	const conn = new FakeEstablished();
	const connection = new Signal<Moq.Connection.Established | undefined>(conn);

	const broadcast = new Broadcast({
		connection,
		enabled: true,
		name: Moq.Path.from("test.hang"),
	});

	try {
		await waitFor(() => conn.published !== undefined);
		expect(broadcast.requestCount.peek()).toBe(0);

		// A catalog subscribe (what relays send within ~100ms of an announce) counts as activity.
		conn.published?.subscribe(Broadcast.CATALOG_TRACK, 0);
		await waitFor(() => broadcast.requestCount.peek() === 1);

		// A new session starts back at zero: the count is per-session wedge evidence.
		const conn2 = new FakeEstablished();
		connection.set(conn2);
		await waitFor(() => conn2.published !== undefined);
		expect(broadcast.requestCount.peek()).toBe(0);

		conn2.published?.subscribe(Broadcast.CATALOG_TRACK, 0);
		await waitFor(() => broadcast.requestCount.peek() === 1);
	} finally {
		broadcast.close();
	}
});
