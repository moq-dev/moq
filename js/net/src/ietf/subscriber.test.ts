import { expect, test } from "bun:test";
import { createMockTransportPair } from "../mock.ts";
import * as Path from "../path.ts";
import { Stream } from "../stream.ts";
import { NativeSession } from "./adapter.ts";
import { PublishNamespace } from "./publish_namespace.ts";
import { Subscriber } from "./subscriber.ts";
import { ALPN, Version } from "./version.ts";

const VERSION = Version.DRAFT_19;

/** How long to wait for a stream before calling it absent. */
const STREAM_WAIT = 500;

/** Accept the next stream the subscriber opens, or give up rather than hang forever. */
async function nextStream(transport: WebTransport): Promise<Stream | undefined> {
	return Promise.race([
		Stream.accept(transport, VERSION),
		new Promise<undefined>((resolve) => setTimeout(() => resolve(undefined), STREAM_WAIT)),
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
	quiet.announced(Path.empty());
	expect(await nextStream(pair.client)).toBeUndefined();
});

/**
 * The declaration is advisory, so a peer that said it advertises nothing may still send
 * an unsolicited PUBLISH_NAMESPACE. Skipping the question must not make us deaf to the
 * answer nobody asked for.
 */
test("an announcement from a peer that advertises nothing still lands", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_19);
	const session = new NativeSession(pair.server, VERSION, true);
	const subscriber = new Subscriber(session, { announce: false, interest: true });

	const announced = subscriber.announced(Path.empty());

	// What the connection dispatch does when a PUBLISH_NAMESPACE arrives.
	const stream = await Stream.open(pair.server, { version: VERSION });
	void subscriber.runPublishNamespace(
		new PublishNamespace({ requestId: 0n, trackNamespace: Path.from("surprise") }),
		stream,
	);

	const next = await announced.next();
	expect(next?.path).toBe(Path.from("surprise"));

	// The feed ends with the session, since no stream of its own ever does it.
	subscriber.close();
	expect(await announced.next()).toBeUndefined();
});
