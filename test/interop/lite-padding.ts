/** Receive Rust padding on a JS session, then return JS padding for Rust's receiver. */
import assert from "node:assert/strict";
import { Connection } from "../../js/net/src/lite/connection.ts";
import { Goaway } from "../../js/net/src/lite/goaway.ts";
import { DataType, StreamId } from "../../js/net/src/lite/stream.ts";
import { ALPN_05, ALPN_06, ALPN_07_WIP, Version } from "../../js/net/src/lite/version.ts";
import { createMockTransportPair } from "../../js/net/src/mock.ts";
import { Stream, Writer } from "../../js/net/src/stream.ts";
import { wireOf } from "../../js/net/src/wire.ts";

console.debug = console.error;
const input: { alpn: string; bytes: number[] } = JSON.parse(process.argv[2]);
const versions: Record<string, Version> = {
	[ALPN_05]: Version.DRAFT_05,
	[ALPN_06]: Version.DRAFT_06,
	[ALPN_07_WIP]: Version.DRAFT_07,
};
const version = versions[input.alpn];
assert(version);
const pair = createMockTransportPair(input.alpn);
const connection = new Connection({ url: new URL("https://example.com/"), quic: pair.server, version });
let closed = false;
void connection.closed.then(() => {
	closed = true;
});
try {
	const sender = new Writer(await pair.client.createUnidirectionalStream(), version);
	await sender.write(Uint8Array.from(input.bytes));
	if (version === Version.DRAFT_07) {
		// A write must drain before FIN, not just when the whole body is available.
		await sender.write(new Uint8Array(4096));
		sender.close();
		await sender.closed;
	} else {
		await assert.rejects(sender.closed);
	}
	const goaway = await Stream.open(pair.client, { version });
	await goaway.writer.u53(StreamId.Goaway);
	await new Goaway("").encode(goaway.writer, version);
	goaway.writer.close();
	assert.equal((await wireOf(connection).goaway).uri, "");
	assert.equal(closed, false);
	const bytes: number[] = [];
	const output = new Writer(
		new WritableStream<Uint8Array>({
			write: (chunk) => {
				bytes.push(...chunk);
			},
		}),
		version,
	);
	await output.u53(DataType.Padding);
	await output.write(new Uint8Array(4096).fill(0xab));
	output.close();
	await output.closed;
	console.log(JSON.stringify(bytes));
} finally {
	connection.abort();
}
