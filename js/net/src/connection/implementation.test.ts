import { expect, test } from "bun:test";
import { ProtocolViolation, SessionCode } from "../error.ts";
import * as Ietf from "../ietf/index.ts";
import * as Message from "../ietf/message.ts";
import { IMPLEMENTATION } from "../implementation.ts";
import * as Lite from "../lite/index.ts";
import { createMockTransportPair } from "../mock.ts";
import { Stream } from "../stream.ts";
import { accept } from "./accept.ts";
import { connect } from "./connect.ts";

const url = new URL("https://localhost/test");

/** Connect a mock pair over `protocol`, each side advertising what it is given. */
async function session(protocol: string, client: string | undefined, server: string | undefined) {
	const pair = createMockTransportPair(protocol);
	const [dialed, accepted] = await Promise.all([
		connect({ url, transport: pair.client, implementation: client }),
		accept({ url, transport: pair.server, implementation: server }),
	]);
	const peers = await Promise.all([dialed.peerImplementation, accepted.peerImplementation]);
	dialed.abort();
	accepted.abort();
	await Promise.all([dialed.closed, accepted.closed]);
	return { client: peers[0], server: peers[1] };
}

// The lite Setup Stream, a draft-17+ uni SETUP, and a draft-14 to 16 bidi SETUP each carry it on their own path.
for (const protocol of [Lite.ALPN_06, Ietf.ALPN.DRAFT_18, Ietf.ALPN.DRAFT_16]) {
	test(`each side reads the other's implementation over ${protocol}`, async () => {
		expect(await session(protocol, "client/1", "server/2")).toEqual({ client: "server/2", server: "client/1" });
	});

	test(`an empty implementation is omitted over ${protocol}`, async () => {
		expect(await session(protocol, "", undefined)).toEqual({ client: IMPLEMENTATION, server: undefined });
	});
}

// lite-02 negotiates in a compat SETUP that carries it, but only lite-05+ reads it.
for (const protocol of [Lite.ALPN_04, Lite.ALPN]) {
	test(`a lite version before 05 reads no implementation over ${protocol}`, async () => {
		expect(await session(protocol, "client/1", "server/2")).toEqual({ client: undefined, server: undefined });
	});
}

test("a bidi SETUP with a malformed implementation closes the session", async () => {
	const pair = createMockTransportPair(Ietf.ALPN.DRAFT_16);
	const version = Ietf.Version.DRAFT_16;
	const params = new Ietf.SetupOptions();
	params.setBytes(Ietf.SetupOption.Implementation, new Uint8Array([0xc3, 0x28]));
	const stream = await Stream.open(pair.client, { version });
	await Message.write(
		stream.writer,
		Lite.StreamId.ClientCompat,
		new Ietf.ClientSetup({ versions: [], parameters: params }),
		version,
	);

	await expect(accept({ url, transport: pair.server })).rejects.toThrow(ProtocolViolation);
	expect((await pair.client.closed).closeCode).toBe(SessionCode.ProtocolViolation);
});

test("a session that closes before the peer's SETUP reads no implementation", async () => {
	const pair = createMockTransportPair(Lite.ALPN_06);
	const accepted = await accept({ url, transport: pair.server });
	pair.client.close();
	expect(await accepted.peerImplementation).toBeUndefined();
});
