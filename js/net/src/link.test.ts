import { afterEach, expect, mock, spyOn, test } from "bun:test";
import { accept, connect, type Session } from "./connection/index.ts";
import * as Ietf from "./ietf/index.ts";
import * as Instance from "./instance.ts";
import * as Lite from "./lite/index.ts";
import { Routed } from "./lite/routed.ts";
import { createMockTransportPair, waitFor } from "./mock.ts";
import { Producer as OriginProducer } from "./origin.ts";
import * as Path from "./path.ts";

afterEach(() => mock.restore());

const url = new URL("https://localhost:4443/test");
const path = Path.from("live");

/** One upstream claiming `path` for `instance` at `claimCost`, dialed at `cost` and ranked `priority`. */
async function upstream(
	alpn: string,
	edge: OriginProducer,
	instance: Instance.Valid,
	link: { cost?: bigint; priority?: number; claimCost?: bigint },
) {
	const pair = createMockTransportPair(alpn);
	const origin = new OriginProducer();
	const claim = origin.claim(path, { instance: { id: instance, epoch: 0n }, cost: link.claimCost ?? 0n });
	const consume = link.priority === undefined ? edge : edge.withPriority(link.priority);
	const sessions: Session[] = await Promise.all([
		connect({ url, transport: pair.client, consume, cost: link.cost }),
		accept({ transport: pair.server, url, publish: origin.consume() }),
	]);
	return {
		client: sessions[0],
		asked: claim.requested().next(),
		close() {
			for (const session of sessions) session.abort();
			claim.close();
			origin.close();
		},
	};
}

type Link = Parameters<typeof upstream>[3];

/** Which of two links to one instance the edge pulls `path` through. */
async function chosen(alpn: string, a: Link, b: Link): Promise<"a" | "b"> {
	const edge = new OriginProducer();
	const instance = Instance.random();
	const links = { a: await upstream(alpn, edge, instance, a), b: await upstream(alpn, edge, instance, b) };
	// Both routes land before the request resolves, so it picks between them. Each session hands
	// its route to the edge before this reader of the same session hears of it.
	const announced = edge.announced(Path.Pattern.subtree(path));
	for (const link of Object.values(links)) {
		const routes = link.client.routes(Path.Pattern.subtree(path));
		await routes.next();
		routes.close();
	}
	const request = edge.follow(path);
	const active = await waitFor(request.active, (broadcast) => broadcast !== undefined);
	const track = active?.track("video").subscribe();
	const winner = await Promise.race([links.a.asked.then(() => "a" as const), links.b.asked.then(() => "b" as const)]);
	track?.close();
	request.close();
	announced.close();
	links.a.close();
	links.b.close();
	edge.close();
	return winner;
}

test.each([Lite.ALPN_06, Lite.ALPN_07_WIP, Ietf.ALPN.DRAFT_18])("%s: the cheaper link wins", async (alpn) => {
	expect(await chosen(alpn, { cost: 5n }, { cost: 2n })).toBe("b");
	expect(await chosen(alpn, { cost: 2n }, { cost: 5n })).toBe("a");
	// An unpriced link costs 1.
	expect(await chosen(alpn, {}, { cost: 2n })).toBe("a");
});

test.each([Lite.ALPN_06, Lite.ALPN_07_WIP, Ietf.ALPN.DRAFT_18])(
	"%s: a higher priority wins despite its cost",
	async (alpn) => {
		expect(await chosen(alpn, { cost: 1n }, { cost: 50n, priority: 1 })).toBe("b");
		expect(await chosen(alpn, { cost: 1n, priority: -1 }, { cost: 50n })).toBe("b");
	},
);

test("moq-lite-07-wip: a route ranks by the lowest priority on its path", async () => {
	// The claim through `a` says a link before it is demoted, as a relay forwarding it would.
	const receive = Routed.prototype.receive;
	spyOn(Routed.prototype, "receive").mockImplementation(function (this: Routed, msg) {
		if (msg.status === "start" && msg.claim.cost === 2n) msg = { ...msg, claim: { ...msg.claim, priority: -3 } };
		return receive.call(this, msg);
	});
	expect(await chosen(Lite.ALPN_07_WIP, { priority: 5, claimCost: 2n }, { cost: 50n })).toBe("b");
	// Without the demotion, the promoted link wins.
	expect(await chosen(Lite.ALPN_07_WIP, { priority: 5, claimCost: 3n }, { cost: 50n })).toBe("a");
});

test("a link refuses a cost no wire carries, and lite-07 refuses a free one", async () => {
	await expect(connect({ url, cost: -1n })).rejects.toThrow(RangeError);
	expect(() => new OriginProducer().withPriority(128)).toThrow(RangeError);
	const { client } = createMockTransportPair(Lite.ALPN_07_WIP);
	expect(() => new Lite.Connection({ url, quic: client, version: Lite.Version.DRAFT_07, cost: 0n })).toThrow(
		RangeError,
	);
});
