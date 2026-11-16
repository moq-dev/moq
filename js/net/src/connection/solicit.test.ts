import { expect, spyOn, test } from "bun:test";
import { Connection } from "../ietf/connection.ts";
import { PublishNamespace } from "../ietf/publish_namespace.ts";
import { RequestOk } from "../ietf/request.ts";
import { ALPN, Version } from "../ietf/version.ts";
import { createMockTransportPair } from "../mock.ts";
import * as Path from "../path.ts";
import { Stream } from "../stream.ts";
import { accept } from "./accept.ts";
import { exchangeSetup } from "./handshake.ts";

const VERSION = Version.DRAFT_18;
const URL_ = new URL("https://example.com");

test("a peer without MoQ Solicit leaves it out of SETUP", async () => {
	const handshake = async (solicit: boolean) => {
		const pair = createMockTransportPair(ALPN.DRAFT_18);
		const [dialed, accepted] = await Promise.all([
			exchangeSetup(pair.client, VERSION, "test", { client: true, solicit }),
			exchangeSetup(pair.server, VERSION, "test", { client: false }),
		]);
		return { dialed, accepted };
	};

	const declared = await handshake(true);
	expect(declared.accepted.solicit).toBe(true);

	const peer = await handshake(false);
	expect(peer.accepted.solicit).toBeUndefined();
	expect(peer.dialed.solicit).toBe(true);
});

test("a peer that implements MoQ Solicit may not announce unasked", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const [peer, connection] = await Promise.all([
		exchangeSetup(pair.client, VERSION, "test", { client: true }),
		accept({ transport: pair.server, url: URL_ }),
	]);

	const logged = spyOn(console, "error").mockImplementation(() => void 0);
	try {
		const stream = await Stream.open(pair.client, { version: VERSION });
		await stream.writer.u53(PublishNamespace.id);
		await new PublishNamespace({
			requestId: 0n,
			trackNamespace: Path.from("room"),
			params: { cluster: { hops: [peer.cluster.self], cost: 0n } },
		}).encode(stream.writer, VERSION);
		await pair.client.closed;
		expect(logged).toHaveBeenCalled();
	} finally {
		logged.mockRestore();
		connection.close();
	}
});

test("a side that declined MoQ Solicit takes an unasked announce", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const [peer, setup] = await Promise.all([
		exchangeSetup(pair.client, VERSION, "test", { client: true }),
		exchangeSetup(pair.server, VERSION, "test", { client: false, solicit: false }),
	]);
	const connection = new Connection({ url: URL_, quic: pair.server, version: VERSION, client: false, ...setup });
	try {
		const stream = await Stream.open(pair.client, { version: VERSION });
		await stream.writer.u53(PublishNamespace.id);
		await new PublishNamespace({
			requestId: 0n,
			trackNamespace: Path.from("room"),
			params: { cluster: { hops: [peer.cluster.self], cost: 0n } },
		}).encode(stream.writer, VERSION);
		expect(await stream.reader.u53()).toBe(RequestOk.id);
	} finally {
		await connection.close();
	}
});
