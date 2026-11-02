/** JS does not offer IETF FLATE and consumes Rust's inflated fallback through its subscriber. */
import assert from "node:assert/strict";
import { offered } from "../../js/net/src/connection/extensions.ts";
import { exchangeSetup } from "../../js/net/src/connection/handshake.ts";
import { Group } from "../../js/net/src/ietf/object.ts";
import { NativeSession } from "../../js/net/src/ietf/session.ts";
import { Setup } from "../../js/net/src/ietf/setup.ts";
import { Subscribe, SubscribeOk } from "../../js/net/src/ietf/subscribe.ts";
import { Subscriber } from "../../js/net/src/ietf/subscriber.ts";
import type { IetfVersion } from "../../js/net/src/ietf/version.ts";
import { createMockTransportPair } from "../../js/net/src/mock.ts";
import * as Path from "../../js/net/src/path.ts";
import { Reader, Stream, Writer } from "../../js/net/src/stream.ts";

console.debug = console.error;
const input: { draft: number; response?: number[]; group?: number[] } = JSON.parse(process.argv[2]);
const version = (0xff000000 + input.draft) as IetfVersion;
const pair = createMockTransportPair(`moqt-${input.draft}`);

try {
	if (input.response === undefined) {
		// Exercise the real offer builder rather than constructing a reduced test offer.
		const exchanging = exchangeSetup(pair.client, version, "compression interop", {
			client: true,
			extensions: offered(),
		});
		const remote = new Writer(await pair.server.createUnidirectionalStream(), version);
		await remote.u53(Setup.id);
		await new Setup().encode(remote, version);
		const incoming = pair.server.incomingUnidirectionalStreams.getReader();
		const next = await incoming.read();
		assert(next.value);
		incoming.releaseLock();
		const reader = new Reader(next.value, undefined, version);
		assert.equal(await reader.u53(), Setup.id);
		const setup = await Setup.decode(reader, version);
		await exchanging;
		const bytes: number[] = [];
		const writer = new Writer(
			new WritableStream<Uint8Array>({
				write: (chunk) => {
					bytes.push(...chunk);
				},
			}),
			version,
		);
		await setup.parameters.encode(writer);
		await writer.close();
		console.log(JSON.stringify(bytes));
	} else {
		const subscriber = new Subscriber({ session: new NativeSession(pair.client, version, true) });
		const track = subscriber.consume(Path.from("room")).track("data").subscribe();
		try {
			const request = await Stream.accept(pair.server, version);
			assert(request);
			assert.equal(await request.reader.u53(), Subscribe.id);
			await Subscribe.decode(request.reader, version);
			await request.writer.u53(SubscribeOk.id);
			await request.writer.write(Uint8Array.from(input.response));
			await track.info();
			assert(input.group);
			const reader = new Reader(undefined, Uint8Array.from(input.group), version);
			await subscriber.handleGroup(await Group.decode(reader, version), reader);
			const group = await track.recvGroup();
			assert(group);
			const frames: number[][] = [];
			for (;;) {
				const frame = await group.readFrame();
				if (!frame) break;
				frames.push([...frame.payload]);
			}
			console.log(JSON.stringify(frames));
			group.close();
		} finally {
			track.close();
		}
	}
} finally {
	pair.client.close();
	pair.server.close();
}
