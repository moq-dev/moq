/**
 * Sweep identities claimed at one path and readers of each identity's claim on one lite-07 session,
 * as a relay peer announces a pool: the time for every reader to hold every identity's claim.
 */
import { yieldNow } from "@moq/clock";
import { toHop } from "../src/hop.ts";
import * as Identity from "../src/identity.ts";
import { ownClaim } from "../src/lite/announce.ts";
import { Subscriber } from "../src/lite/subscriber.ts";
import { Version } from "../src/lite/version.ts";
import * as Path from "../src/path.ts";

console.debug = () => {};
console.log("identities,readers,settle_ms");
const path = Path.from("pool");
for (const identities of [1, 64, 256, 1024])
	for (const readers of [1, 8, 32]) {
		// The session writes its ANNOUNCE_REQUEST and reads nothing back: the bench answers it below.
		const quic = {
			close: () => {},
			createUnidirectionalStream: async () => new WritableStream<Uint8Array>(),
			createBidirectionalStream: () => new Promise<never>(() => {}),
		} as unknown as WebTransport;
		const subscriber = new Subscriber(quic, Version.DRAFT_07, toHop(1n));
		const streams = Array.from({ length: readers }, () => subscriber.routes(Path.empty(), { claims: true }));
		for (let i = 0; i < 8; i++) await yieldNow();
		await subscriber.announce({ status: "ok" });
		const start = performance.now();
		for (let i = 0; i < identities; i++)
			await subscriber.announce({
				status: "start",
				suffix: path,
				identity: Identity.parse(`job-${i}`),
				claim: ownClaim(0n),
			});
		await Promise.all(
			streams.map(async (stream) => {
				for (let seen = 0; seen < identities; ) {
					const event = await stream.next();
					if (!event) throw new Error("the route stream closed");
					if (event.kind === "start") seen++;
				}
			}),
		);
		console.log(`${identities},${readers},${(performance.now() - start).toFixed(2)}`);
		for (const stream of streams) stream.close();
		subscriber.close();
	}
