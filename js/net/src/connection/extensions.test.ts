import { expect, spyOn, test } from "bun:test";
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
		exchangeSetup(pair.client, VERSION, "test", { client: true, extensions: offered(client) }),
		exchangeSetup(pair.server, VERSION, "test", { client: false, extensions: offered(server) }),
	]);
	return { dialed, accepted };
}

/** Accept a draft-18 session offering `extensions`, against a peer offering every extension. */
async function acceptWith(extensions: Extensions) {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const [peer, connection] = await Promise.all([
		exchangeSetup(pair.client, VERSION, "test", { client: true, extensions: offered() }),
		accept({ transport: pair.server, url: URL_, extensions }),
	]);
	return { pair, peer, connection };
}

test("a declined extension stays out of SETUP", async () => {
	// Control: both sides offer it.
	const all = await handshake({}, {});
	expect(all.accepted.solicit).toBe(true);

	const none = await handshake({ solicit: false }, {});
	expect(none.accepted.solicit).toBeUndefined();
	expect(none.dialed.solicit).toBe(true);
});

test("a side that declined solicit takes an unsolicited announce", async () => {
	const announce = async (session: Awaited<ReturnType<typeof acceptWith>>) => {
		const stream = await Stream.open(session.pair.client, { version: VERSION });
		await stream.writer.u53(PublishNamespace.id);
		await new PublishNamespace({
			requestId: 0n,
			trackNamespace: Path.from("room"),
			params: { cluster: { hops: [session.peer.cluster.self], cost: 0n } },
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
