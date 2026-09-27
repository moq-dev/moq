import { expect, test } from "bun:test";
import * as Ietf from "../ietf/index.ts";
import * as Lite from "../lite/index.ts";
import { createMockTransportPair } from "../mock.ts";
import { Producer } from "../origin.ts";
import * as Path from "../path.ts";
import { withTimeout } from "../util/timeout.ts";
import { accept } from "./accept.ts";
import { connect } from "./connect.ts";

for (const protocol of [Lite.ALPN_07_WIP, Ietf.ALPN.DRAFT_19]) {
	test(`scoped origins discover multiple prefixes over ${protocol}`, async () => {
		const pair = createMockTransportPair(protocol);
		const url = new URL("https://localhost/test");
		const source = new Producer();
		const destination = new Producer();
		const publish = source.scope(Path.from("server"), new Path.Patterns([Path.Pattern.all()]));
		const consume = destination.scope(
			Path.from("client"),
			new Path.Patterns(["room/**", "other/**"].map(Path.Pattern.parse)),
		);
		const broadcasts = ["room/live", "room/.hidden", "other/live", "outside"].map((path) => {
			const broadcast = publish.createBroadcast(Path.from(path));
			broadcast.announce();
			return broadcast;
		});
		const [client, server] = await Promise.all([
			connect({ url, transport: pair.client, consume }),
			accept({ url, transport: pair.server, publish: publish.consume() }),
		]);
		const announced = destination.announced(undefined, { hidden: true });
		try {
			const received: string[] = [];
			for (let index = 0; index < 3; index++) {
				const update = await withTimeout(announced.next(), 1000, "scoped announcement did not arrive");
				if (!update) throw new Error("announcement stream ended before replay");
				expect(update.kind).toBe("announced");
				received.push(update.prefix);
			}
			expect(received.sort()).toEqual(["client/other/live", "client/room/.hidden", "client/room/live"]);
			expect([...consume.broadcasts().peek().keys()].sort()).toEqual([
				Path.from("other/live"),
				Path.from("room/live"),
			]);
		} finally {
			announced.close();
			client.close();
			server.close();
			for (const broadcast of broadcasts) broadcast.close();
			source.close();
			destination.close();
			await Promise.all([client.closed, server.closed]);
		}
	});
}
