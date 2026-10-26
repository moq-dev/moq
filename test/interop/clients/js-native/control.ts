/** Real JS wire peer for metadata queries and a deliberately small legacy request window. */
import assert from "node:assert/strict";
import { createInterface } from "node:readline";
import * as Moq from "@moq/net";
import { offered } from "../../../../js/net/src/connection/extensions.ts";
import { exchangeSetup } from "../../../../js/net/src/connection/handshake.ts";
import { ControlStreamAdapter, NativeSession } from "../../../../js/net/src/ietf/adapter.ts";
import { Connection } from "../../../../js/net/src/ietf/connection.ts";
import * as Ietf from "../../../../js/net/src/ietf/index.ts";
import { TrackStatusOk, TrackStatusRequest } from "../../../../js/net/src/ietf/track.ts";
import * as Lite from "../../../../js/net/src/lite/index.ts";
import { Stream } from "../../../../js/net/src/stream.ts";
import Qmux from "../../../../js/qmux/src/index.ts";

console.debug = console.error;
console.info = console.error;
const [mode, address, name] = process.argv.slice(2);
assert(address && name && ["status", "credit"].includes(mode), "usage: control.ts status|credit URL VERSION");
const version = (0xff000000 + Number(name.split("-").at(-1))) as Ietf.IetfVersion;
const legacy = version === Ietf.Version.DRAFT_14 || version === Ietf.Version.DRAFT_16;
const protocol = version === Ietf.Version.DRAFT_14 ? "moql" : `moqt-${Number(version) - 0xff000000}`;
const url = new URL(address);
const quic = new Qmux(url, {
	protocols: [protocol],
	versions: { [protocol]: legacy ? "qmux-00" : "qmux-01" },
	requireProtocol: true,
});
await quic.ready;
const extensions = offered({ auth: false, epoch: false, solicit: false });
// The ordinary client offers a large window. This peer deliberately advertises one usable track
// slot: draft 16 also holds namespace discovery open on its native request stream.
const window = mode === "credit" ? (version === Ietf.Version.DRAFT_16 ? 2n : 1n) : Ietf.REQUEST_WINDOW;
let control: Stream;
let maxRequestId = 0n;
if (legacy) {
	// Reuse the production codecs and adapter. Only the small SETUP grant differs from connect().
	control = await Stream.open(quic, { version });
	await control.writer.u53(Lite.StreamId.ClientCompat);
	const parameters = new Ietf.SetupOptions();
	parameters.setVarint(Ietf.SetupOption.MaxRequestId, Ietf.initialMaxRequestId(false, window));
	await new Ietf.ClientSetup({ versions: [version], parameters }).encode(control.writer, version);
	assert.equal(await control.reader.u53(), Lite.StreamId.ServerCompat);
	const setup = await Ietf.ServerSetup.decode(control.reader, version);
	assert.equal(setup.version, version, "wrong negotiated draft");
	maxRequestId = setup.parameters.getVarint(Ietf.SetupOption.MaxRequestId) ?? 0n;
} else {
	({ control } = await exchangeSetup(quic, version, "interop-control", { client: true, extensions }));
}

if (mode === "status") {
	const session = legacy
		? new ControlStreamAdapter(quic, control, version, maxRequestId, true)
		: new NativeSession(quic, version, true);
	const driver = session instanceof ControlStreamAdapter ? session.run() : undefined;
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
		assert.equal(await stream.reader.u53(), TrackStatusOk.id(version), "TRACK_STATUS was not answered");
		const status = await TrackStatusOk.decode(stream.reader, version);
		if (legacy) assert.equal(status.requestId, requestId);
		assert.deepEqual(status.largest, { groupId: 4n, objectId: 1n }, "wrong publisher location");
		// Rust publishes priority 2, which moq-transport carries as 127 - 2.
		if (!legacy) assert.equal(status.properties.priority, 125, "wrong publisher priority");
		stream.close();
		console.error("wire track status answered");
	} finally {
		session.close();
		quic.close();
		await driver?.catch(() => {});
	}
} else {
	assert(legacy, "request credit only exists on drafts 14 and 16");
	const origin = new Moq.Origin.Producer();
	const broadcastPublication = origin.createBroadcast(Moq.Path.from("control"));
	const broadcast = broadcastPublication.broadcast;
	const first = broadcast.createTrack("first");
	const second = broadcast.createTrack("second");
	broadcastPublication.announce();
	const connection = new Connection({
		url,
		quic,
		control,
		version,
		maxRequestId,
		client: true,
		publish: origin.consume(),
		requestWindow: window,
		extensions,
	});
	const lines = createInterface({ input: process.stdin });
	const commands = lines[Symbol.asyncIterator]();
	try {
		assert.equal((await commands.next()).value, "probe", "missing queued-request barrier");
		const group = first.appendGroup();
		group.writeFrame({ payload: new Uint8Array([1]) });
		group.finish();
		const demand = second.demand();
		while (demand.wanted.peek() === undefined) await demand.wanted.changed();
		const next = second.appendGroup();
		next.writeFrame({ payload: new Uint8Array([2]) });
		next.finish();
		assert.equal((await commands.next()).value, "done");
		console.error("request credit resumed");
	} finally {
		lines.close();
		connection.abort();
		origin.close();
	}
}
