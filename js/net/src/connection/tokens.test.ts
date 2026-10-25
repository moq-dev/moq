import { expect, test } from "bun:test";
import type { Auth, Handle } from "../auth.ts";
import * as Ietf from "../ietf/index.ts";
import { createMockTransportPair } from "../mock.ts";
import { Reader } from "../stream.ts";
import type { Established } from "./established.ts";
import { offered } from "./extensions.ts";
import { exchangeSetup } from "./handshake.ts";
import { credential, extra, Held, hold } from "./tokens.ts";

const URL_ = new URL("https://relay.example.com/room");

test("versions with a SETUP token keep the credential out of the URL", () => {
	const dial = credential(URL_, ["a", "b"], true);
	expect(dial.url.href).toBe(URL_.href);
	expect(dial.setup).toBe("a");
	expect(extra(dial, ["a", "b", "b"])).toEqual(["b"]);
});

test("an older offered version puts the credential in the URL too", () => {
	const dial = credential(new URL("https://relay.example.com/room?x=1"), ["a b"], false);
	expect(dial.url.href).toBe("https://relay.example.com/room?x=1&jwt=a+b");
	expect(dial.setup).toBe("a b");
	expect(extra(dial, ["a b"])).toEqual([]);
});

test("a URL credential sends every other token on AUTH", () => {
	const url = new URL("https://relay.example.com/room?jwt=peer");
	const dial = credential(url, ["a", "peer"], false);
	expect(dial.url.href).toBe(url.href);
	expect(dial.setup).toBeUndefined();
	expect(extra(dial, ["a", "peer"])).toEqual(["a"]);
});

test("no tokens present nothing", () => {
	const dial = credential(URL_, [], false);
	expect(dial.url.href).toBe(URL_.href);
	expect(dial.setup).toBeUndefined();
});

/** A handle whose answers the test releases one token at a time. */
function handle() {
	const answers = new Map<string, (auth: Auth) => void>();
	const closed: string[] = [];
	const presented: string[] = [];
	const fake = {
		add(token: string) {
			presented.push(token);
			return new Promise<Auth>((resolve) => answers.set(token, resolve));
		},
	} as unknown as Handle;
	const answer = async (token: string) => {
		const resolve = answers.get(token);
		if (!resolve) throw new Error(`${token} was never presented`);
		resolve({ close: () => closed.push(token) } as unknown as Auth);
		// Let the answer and the settle it schedules run.
		for (let i = 0; i < 3; i++) await Promise.resolve();
	};
	return { fake, answers, closed, presented, answer };
}

test("a replaced token is withdrawn only once its replacement answers", async () => {
	const { fake, answers, closed, answer } = handle();
	const held = new Held(fake, credential(URL_, ["connection"], true));
	held.update(["connection", "old"]);
	expect([...answers.keys()]).toEqual(["old"]);
	await answer("old");

	held.update(["connection", "new"]);
	expect([...answers.keys()]).toEqual(["old", "new"]);
	expect(closed).toEqual([]);

	await answer("new");
	expect(closed).toEqual(["old"]);
});

test("a token replaced before its answer closes when it arrives", async () => {
	const { fake, closed, answer } = handle();
	const held = new Held(fake, credential(URL_, ["connection"], true));
	held.update(["connection", "old"]);
	held.update(["connection", "new"]);
	await answer("new");
	expect(closed).toEqual([]);
	await answer("old");
	expect(closed).toEqual(["old"]);
});

test("sessions sharing an auth handle present each configured token once", async () => {
	const { fake, closed, presented, answer } = handle();
	const first = { auth: fake } as unknown as Established;
	const second = { auth: fake } as unknown as Established;
	hold(first, credential(URL_, ["connection", "a"], true), ["connection", "a"]);
	await answer("a");

	// A reconnect on the same handle, whose credential now comes from the URL.
	const url = new URL("https://relay.example.com/room?jwt=peer");
	hold(second, credential(url, ["connection", "a"], true), ["connection", "a"]);
	expect(presented).toEqual(["a", "connection"]);
	expect(closed).toEqual([]);
});

test("a draft-17+ client presents its token in SETUP", async () => {
	const version = Ietf.Version.DRAFT_18;
	const pair = createMockTransportPair(Ietf.ALPN.DRAFT_18);
	const token = new TextEncoder().encode("credential");
	// Never answered: only the SETUP the client writes matters here.
	void exchangeSetup(pair.client, version, "test", { client: true, extensions: offered(), token }).catch(() => {});

	const streams = pair.server.incomingUnidirectionalStreams.getReader();
	const next = await streams.read();
	if (next.done) throw new Error("no SETUP stream");
	const reader = new Reader(next.value, undefined, version);
	expect(await reader.u53()).toBe(Ietf.Setup.id);
	const setup = await Ietf.Setup.decode(reader, version);
	const presented = Ietf.tokenFromSetup(setup.parameters, version);
	expect(presented?.kind).toBe(Ietf.TOKEN_OUT_OF_BAND);
	expect(new TextDecoder().decode(presented?.value)).toBe("credential");
	pair.client.close();
});
