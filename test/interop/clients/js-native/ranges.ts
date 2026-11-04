/** Standalone multi-group FETCH against the pinned Moxygen publisher. @module */
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import * as Moq from "@moq/net";
import WebTransport from "@moq/web-transport";
import { offered } from "../../../../js/net/src/connection/extensions.ts";
import { exchangeSetup } from "../../../../js/net/src/connection/handshake.ts";
import { Connection } from "../../../../js/net/src/ietf/connection.ts";
import { Version } from "../../../../js/net/src/ietf/version.ts";
import { wireOf } from "../../../../js/net/src/wire.ts";

console.debug = console.error;
console.info = console.error;
const [address, certificate] = process.argv.slice(2);
assert(address && certificate, "usage: ranges.ts URL CERTIFICATE");
const url = new URL(address);
const cert = readFileSync(certificate, "utf8");
// MoQTest's 16 namespace fields: subgroup zero, groups 0,2,...,1000, three objects per
// group, 1024/513-byte payloads, 10 ms between objects, and no status markers or properties.
const path = Moq.Path.from("moq-test-00/0/0/0/1000/2/3/1024/513/10/2/1/0/-1/-1/0");
async function connect() {
	const transport = new WebTransport(url, {
		protocols: ["moqt-18"],
		serverCertificateHashes: [{ algorithm: "sha-256", value: await Moq.Connection.certificateHash(cert) }],
	});
	await transport.ready;
	// Moxygen's pinned HTTP/3 server predates subprotocol negotiation. This fixture's
	// server offers draft 18 alone, so exchange that SETUP directly on its WebTransport.
	const extensions = offered({ auth: false, solicit: false });
	const setup = await exchangeSetup(transport, Version.DRAFT_18, "interop-ranges", { client: true, extensions });
	return new Connection({ url, quic: transport, version: Version.DRAFT_18, client: true, extensions, ...setup });
}

function payload(bytes: Uint8Array, size: number) {
	assert.equal(bytes.byteLength, size, "wrong object size");
	for (const [offset, byte] of bytes.entries())
		assert.equal(byte, offset % 256 === 0 ? (offset >> 8) & 255 : 0x74, `corrupt object at ${offset}`);
}

const live = await connect();
const source = wireOf(live).consume(path);
const holder = source.track("test").subscribe({ delay: { max: null } });
try {
	// Give TRACK_STATUS a real live edge without sharing this connection's cache with FETCH.
	for (;;) {
		const group = await holder.recvGroup();
		assert(group, "publisher ended before its past range existed");
		while (await group.readFrame()) {}
		group.close();
		if (group.sequence >= 8n) break;
	}
	const history = await connect();
	const broadcast = wireOf(history).consume(path);
	try {
		for (const [start, end, expected] of [
			[2n, 7n, [2n, 4n, 6n]],
			[0n, 5n, [0n, 2n, 4n]],
		] as const) {
			const reader = broadcast.track("test").subscribe({
				ranges: [{ start: { absolute: start }, end: { absolute: end } }],
				delay: { max: null },
				order: "ascending",
			});
			try {
				const seen: bigint[] = [];
				for (let group = await reader.recvGroup(); group; group = await reader.recvGroup()) {
					seen.push(group.sequence);
					try {
						for (const size of [1024, 513, 513]) {
							const frame = await group.readFrame();
							assert(frame, "FETCH omitted an object");
							payload(frame.payload, size);
						}
						assert.equal(await group.readFrame(), undefined, "FETCH repeated an object");
					} finally {
						group.close();
					}
				}
				assert.deepEqual(seen, expected, "FETCH omitted, repeated, or renumbered a group");
			} finally {
				reader.close();
			}
		}
		console.error("moxygen ranges clean groups=2,4,6;0,2,4 frames=18");
	} finally {
		broadcast.close();
		await history.close();
	}
} finally {
	holder.close();
	source.close();
	await live.close();
}
process.exit(0);
