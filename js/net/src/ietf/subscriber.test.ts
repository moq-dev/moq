import { expect, test } from "bun:test";
import { createMockTransportPair } from "../mock.ts";
import * as Path from "../path.ts";
import { Stream } from "../stream.ts";
import { NativeSession } from "./adapter.ts";
import { Subscriber } from "./subscriber.ts";
import { ALPN, Version } from "./version.ts";

const VERSION = Version.DRAFT_19;

/** Accept the next stream the subscriber opens, or give up rather than hang forever. */
async function nextStream(transport: WebTransport): Promise<Stream | undefined> {
	return Promise.race([
		Stream.accept(transport, VERSION),
		new Promise<undefined>((resolve) => setTimeout(() => resolve(undefined), 500)),
	]);
}

/**
 * A peer that declared it advertises nothing is never asked: a SUBSCRIBE_NAMESPACE would
 * buy one stream and an empty answer.
 */
test("a peer that advertises nothing is not asked", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_19);
	const session = new NativeSession(pair.server, VERSION, true);

	const asked = new Subscriber(session, { announce: false, interest: false });
	asked.announced(Path.empty());
	expect(await nextStream(pair.client)).toBeDefined();

	const quiet = new Subscriber(session, { announce: false, interest: true });
	const announced = quiet.announced(Path.empty());
	expect(await nextStream(pair.client)).toBeUndefined();

	// The feed is closed rather than left hanging, so a consumer sees the end.
	expect(await announced.next()).toBeUndefined();
});
