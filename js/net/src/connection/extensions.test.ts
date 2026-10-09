import { expect, spyOn, test } from "bun:test";
import { SessionCode } from "../error.ts";
import { AuthMessage, AuthOk } from "../ietf/auth.ts";
import { PublishNamespace } from "../ietf/publish_namespace.ts";
import { RequestOk } from "../ietf/request.ts";
import { ALPN, Version } from "../ietf/version.ts";
import { createMockTransportPair } from "../mock.ts";
import * as Path from "../path.ts";
import { Stream } from "../stream.ts";
import { accept } from "./accept.ts";
import { type Extensions, offered } from "./extensions.ts";
import { exchangeSetup } from "./handshake.ts";

const VERSION = Version.DRAFT_18;
const URL_ = new URL("https://example.com");

/** What each side learns from the other's SETUP when each offers `client` and `server`. */
async function handshake(client: Extensions, server: Extensions) {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const [dialed, accepted] = await Promise.all([
		exchangeSetup(pair.client, VERSION, "test", offered(client)),
		exchangeSetup(pair.server, VERSION, "test", offered(server)),
	]);
	return { dialed, accepted };
}

/** Accept a draft-18 session offering `extensions`, against a peer offering every extension. */
async function acceptWith(extensions: Extensions) {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const [peer, connection] = await Promise.all([
		exchangeSetup(pair.client, VERSION, "test", offered()),
		accept({ transport: pair.server, url: URL_, extensions }),
	]);
	return { pair, peer, connection };
}

test("every extension is offered by default", () => {
	expect(offered()).toEqual({ auth: true, solicit: true });
	expect(offered({ solicit: false })).toEqual({ auth: true, solicit: false });
});

test("a declined extension stays out of SETUP", async () => {
	// Control: both sides offer both.
	const all = await handshake({}, {});
	expect(all.accepted.solicit).toBe(true);
	expect(all.accepted.auth).toBe(true);
	expect(all.dialed.auth).toBe(true);

	const none = await handshake({ auth: false, solicit: false }, {});
	expect(none.accepted.solicit).toBeUndefined();
	expect(none.accepted.auth).toBe(false);
	// Negotiated only when both offer it, so the declining side does not speak it either.
	expect(none.dialed.auth).toBe(false);
	expect(none.dialed.solicit).toBe(true);
});

test("an AUTH to a side that declined it closes the session", async () => {
	// Control: offered by both, the connection credential is granted.
	const offering = await acceptWith({});
	const granted = await Stream.open(offering.pair.client, { version: VERSION });
	await new AuthMessage(0n, new Uint8Array()).encode(granted.writer, VERSION);
	expect(await granted.reader.u53()).toBe(AuthOk.id);
	offering.connection.close();

	const logged = spyOn(console, "error").mockImplementation(() => void 0);
	try {
		const declined = await acceptWith({ auth: false });
		const refused = await Stream.open(declined.pair.client, { version: VERSION });
		// The session closes on the message type alone, so the rest of the write may fail.
		await new AuthMessage(0n, new Uint8Array()).encode(refused.writer, VERSION).catch(() => undefined);
		const info = await declined.pair.client.closed;
		expect(info.closeCode).toBe(SessionCode.ProtocolViolation);
	} finally {
		logged.mockRestore();
	}
});

test("a side that declined solicit takes an unsolicited announce", async () => {
	const announce = async (session: Awaited<ReturnType<typeof acceptWith>>) => {
		const stream = await Stream.open(session.pair.client, { version: VERSION });
		await stream.writer.u53(PublishNamespace.id);
		await new PublishNamespace({
			requestId: 0n,
			trackNamespace: Path.from("room"),
			cluster: { hops: [session.peer.cluster.self], cost: 0n },
		}).encode(stream.writer, VERSION);
		return stream;
	};

	const declined = await acceptWith({ solicit: false });
	const taken = await announce(declined);
	expect(await taken.reader.u53()).toBe(RequestOk.id);
	declined.connection.close();

	// Control: our SETUP declared it, so the peer that implements it broke it.
	const logged = spyOn(console, "error").mockImplementation(() => void 0);
	try {
		const declaring = await acceptWith({});
		await announce(declaring);
		await declaring.pair.client.closed;
		expect(logged).toHaveBeenCalled();
	} finally {
		logged.mockRestore();
	}
});
