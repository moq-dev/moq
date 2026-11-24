import { afterEach, expect, test } from "bun:test";
import { Duration } from "@moq/clock";
import { SessionCode, SessionError } from "../error.ts";
import * as Lite from "../lite/index.ts";
import { createMockTransportPair, manual, waitFor } from "../mock.ts";
import * as Path from "../path.ts";
import { Stream } from "../stream.ts";
import { accept } from "./accept.ts";
import { Dials } from "./dials.ts";
import { Reload } from "./reload.ts";

const original = globalThis.WebTransport;
afterEach(() => {
	globalThis.WebTransport = original;
});

const URL_ = new URL("https://example.com/");
const BACKOFF = { initial: Duration.millis(1), multiplier: 1, max: Duration.millis(1), timeout: null };
const GRANT = { publish: new Path.Patterns([Path.Pattern.all()]), subscribe: new Path.Patterns() };

/**
 * One session a relay accepted: its server side, the credential its URL's `auth` carried, and
 * every token presented in band, past the connection's own.
 */
interface Served {
	server: WebTransport;
	credential: string | null;
	inband: string[];
}

/** A lite-07 relay that grants every token, refusing only `refused` as the URL's credential. */
function relay(refused?: string) {
	const served: Served[] = [];
	const stub = function StubWebTransport(target: string | URL) {
		const url = new URL(target);
		const pair = createMockTransportPair(Lite.ALPN_07_WIP);
		const session: Served = { server: pair.server, credential: url.searchParams.get("auth"), inband: [] };
		served.push(session);
		void accept({ transport: pair.server, url }).then(async (accepted) => {
			const requests = accepted.auth.requests();
			for (;;) {
				const request = await requests.next();
				if (!request) break;
				if (request.token.byteLength > 0) session.inband.push(new TextDecoder().decode(request.token));
				if (session.credential !== null && session.credential === refused) {
					request.reject(SessionCode.Unauthorized, "spent");
					pair.server.close({ closeCode: SessionCode.Unauthorized, reason: "spent" });
				} else request.accept(GRANT);
			}
		});
		return pair.client;
	};
	globalThis.WebTransport = stub as unknown as typeof WebTransport;
	return served;
}

/** Answer every dial with `sig-<n>`, counting from 1, until the dials end. */
function sign(dials: Dials): string[] {
	const signed: string[] = [];
	void (async () => {
		for (let dial = await dials.next(); dial; dial = await dials.next()) {
			const token = `sig-${signed.length + 1}`;
			signed.push(token);
			dial.present([token]);
		}
	})();
	return signed;
}

async function goaway(server: WebTransport) {
	const stream = await Stream.open(server, { version: Lite.Version.DRAFT_07 });
	await stream.writer.u53(Lite.StreamId.Goaway);
	await new Lite.Goaway("").encode(stream.writer, Lite.Version.DRAFT_07);
}

for (const urlAuth of [true, false]) {
	test(`every dial presents a set signed for it alone, a GOAWAY's handover included (urlAuth: ${urlAuth})`, async () => {
		using clock = manual();
		const served = relay();
		const dials = new Dials();
		const signed = sign(dials);
		const reload = new Reload({
			url: URL_,
			websocket: { enabled: false },
			tokens: dials,
			urlAuth,
			backoff: BACKOFF,
		});
		try {
			await clock.drive(reload.answered());
			const first = served[0];
			if (!first) throw new Error("no first dial");

			await clock.drive(goaway(first.server));
			await clock.drive(waitFor(reload.established, (session) => session !== undefined && served.length === 2));
			await clock.drive(reload.answered());
			await clock.advance(Duration.millis(10));

			// Each session took only its own dial's credential: the draining one never read its successor's in band.
			expect(signed).toEqual(["sig-1", "sig-2"]);
			expect(served.map((session) => session.credential)).toEqual(urlAuth ? ["sig-1", "sig-2"] : [null, null]);
			expect(served.map((session) => session.inband)).toEqual([[], []]);
			expect(reload.tokens.peek()).toEqual(["sig-2"]);
		} finally {
			reload.close();
			dials.close();
		}
	});
}

test("a dropped session redials with a new set", async () => {
	using clock = manual();
	const served = relay();
	const dials = new Dials();
	const signed = sign(dials);
	const reload = new Reload({
		url: URL_,
		websocket: { enabled: false },
		tokens: dials,
		urlAuth: true,
		backoff: BACKOFF,
	});
	try {
		await clock.drive(reload.answered());
		served[0]?.server.close({ closeCode: 0, reason: "restart" });
		await clock.drive(waitFor(reload.established, (session) => session !== undefined && served.length === 2));
		await clock.drive(reload.answered());
		expect(signed).toEqual(["sig-1", "sig-2"]);
		expect(served.map((session) => session.credential)).toEqual(["sig-1", "sig-2"]);
	} finally {
		reload.close();
		dials.close();
	}
});

test("an answer awaited while a dial signs is that dial's verdict, even for a set repeating the last", async () => {
	using clock = manual();
	relay("spent");
	const dials = new Dials();
	const reload = new Reload({
		url: URL_,
		websocket: { enabled: false },
		tokens: dials,
		urlAuth: true,
		backoff: BACKOFF,
	});
	try {
		const first = await clock.drive(dials.next());
		first?.present(["fresh"]);
		await clock.drive(reload.answered());

		// The session ends, so the loop redials, and the app answers the same token again.
		const session = reload.established.peek();
		void session?.close();
		const second = await clock.drive(dials.next());
		const answer = reload.answered().then(
			() => undefined,
			(err: unknown) => err,
		);
		second?.present(["spent"]);
		const err = await clock.drive(answer);
		expect(err).toBeInstanceOf(SessionError);
		expect((err as SessionError).code).toBe(SessionCode.Unauthorized);
	} finally {
		reload.close();
		dials.close();
	}
});

test("a dial the app fails fails as an unreachable peer does", async () => {
	using clock = manual();
	const served = relay();
	const dials = new Dials();
	const reload = new Reload({ url: URL_, websocket: { enabled: false }, tokens: dials, backoff: BACKOFF });
	try {
		const dial = await clock.drive(dials.next());
		expect(dial?.url.href).toBe(URL_.href);
		dial?.fail(new Error("no key"));
		// The URL never had a session, so its first failure stops it.
		const err = await clock.drive(waitFor(reload.error, (error) => error !== undefined));
		expect(err?.message).toBe("no key");
		expect(served).toHaveLength(0);
	} finally {
		reload.close();
		dials.close();
	}
});

test("a closed connection ends its dials, and a dial it abandoned is never handed out", async () => {
	using clock = manual();
	relay();
	const dials = new Dials();
	const reload = new Reload({ url: URL_, websocket: { enabled: false }, tokens: dials, backoff: BACKOFF });
	// Unanswered, the dial is abandoned once the loop goes.
	await clock.advance(Duration.millis(1));
	reload.close();
	dials.close();
	expect(await clock.drive(dials.next())).toBeUndefined();
});
