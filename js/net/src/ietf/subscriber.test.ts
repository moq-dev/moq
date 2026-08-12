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

/**
 * Accept the next stream the subscriber opens, or give up rather than hang forever.
 *
 * Reads the queue directly instead of racing {@link Stream.accept}, whose pending read
 * would keep the reader locked after the race resolves and could swallow a later stream.
 */
async function nextStream(transport: WebTransport): Promise<Stream | undefined> {
	const reader =
		transport.incomingBidirectionalStreams.getReader() as ReadableStreamDefaultReader<WebTransportBidirectionalStream>;

	let timer: ReturnType<typeof setTimeout> | undefined;
	try {
		const next = await Promise.race([
			reader.read(),
			new Promise<undefined>((resolve) => {
				timer = setTimeout(() => resolve(undefined), STREAM_WAIT);
			}),
		]);

		if (!next || next.done) return undefined;
		return new Stream({ readable: next.value.readable, writable: next.value.writable, version: VERSION });
	} finally {
		clearTimeout(timer);
		reader.releaseLock();
	}
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
	const handler = subscriber.runPublishNamespace(
		new PublishNamespace({ requestId: 0n, trackNamespace: Path.from("surprise") }),
		stream,
	);

	const next = await announced.next();
	expect(next?.path).toBe(Path.from("surprise"));
	expect(next?.active).toBe(true);

	// The handler holds the request open until the peer drops it, and withdraws the
	// namespace on the way out.
	const peer = await nextStream(pair.client);
	peer?.close();
	await handler;
	expect(await announced.next()).toMatchObject({ path: Path.from("surprise"), active: false });

	// The feed ends with the session, since no stream of its own ever does it.
	subscriber.close();
	expect(await announced.next()).toBeUndefined();
});
