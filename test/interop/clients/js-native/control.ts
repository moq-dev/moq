/** Real JS wire peer for a TRACK_STATUS query. */
import assert from "node:assert/strict";
import * as Moq from "@moq/net";
import { offered } from "../../../../js/net/src/connection/extensions.ts";
import { exchangeSetup } from "../../../../js/net/src/connection/handshake.ts";
import type * as Ietf from "../../../../js/net/src/ietf/index.ts";
import { NativeSession } from "../../../../js/net/src/ietf/session.ts";
import { TrackStatusOk, TrackStatusRequest } from "../../../../js/net/src/ietf/track.ts";
import Qmux from "../../../../js/qmux/src/index.ts";

console.debug = console.error;
console.info = console.error;
const [address, name] = process.argv.slice(2);
assert(address && name, "usage: control.ts URL VERSION");
const version = (0xff000000 + Number(name.split("-").at(-1))) as Ietf.IetfVersion;
const protocol = `moqt-${Number(version) - 0xff000000}`;
const quic = new Qmux(new URL(address), {
	protocols: [protocol],
	versions: { [protocol]: "qmux-01" },
	requireProtocol: true,
});
await quic.ready;
const extensions = offered({ auth: false, solicit: false });
await exchangeSetup(quic, version, "interop-control", { client: true, extensions });

const session = new NativeSession(quic, version, true);
try {
	const requestId = await session.nextRequestId();
	assert(requestId !== undefined);
	const stream = await session.openBi();
	await stream.writer.u53(TrackStatusRequest.id);
	await new TrackStatusRequest({
		requestId,
		trackNamespace: Moq.Path.from("control"),
		trackName: "video",
	}).encode(stream.writer, version);
	assert.equal(await stream.reader.u53(), TrackStatusOk.id, "TRACK_STATUS was not answered");
	const status = await TrackStatusOk.decode(stream.reader, version);
	assert.deepEqual(status.largest, { groupId: 4n, objectId: 1n }, "wrong publisher location");
	// Rust publishes priority 2, which moq-transport carries as 127 - 2.
	assert.equal(status.properties.priority, 125, "wrong publisher priority");
	stream.close();
	console.error("wire track status answered");
} finally {
	session.close();
	quic.close();
}
