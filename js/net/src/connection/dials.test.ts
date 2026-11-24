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
const GRANT = { publish: Path.Prefixes.all(), subscribe: new Path.Prefixes() };

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

test("a refused credential arrives as the next dial's failure, and a URL never reached keeps asking", async () => {
	using clock = manual();
	const served = relay("spent");
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
		expect(first?.failure).toBeUndefined();
		first?.present(["spent"]);

		// Refused before any session served: fixed tokens would stop here, but the loop asks again.
		const second = await clock.drive(dials.next());
		expect(second?.failure).toBeInstanceOf(SessionError);
		expect((second?.failure as SessionError | undefined)?.code).toBe(SessionCode.Unauthorized);
		expect(reload.error.peek()).toBeUndefined();
		second?.present(["fresh"]);
		await clock.drive(waitFor(reload.status, (status) => status === "connected"));
		expect(served.map((session) => session.credential)).toEqual(["spent", "fresh"]);

		// A session that served clears it: the next dial after a clean drop reads only that drop.
		void reload.established.peek()?.close();
		const third = await clock.drive(dials.next());
		expect(third?.failure?.message).toBe("connection closed");
		third?.present(["fresh-2"]);
	} finally {
		reload.close();
		dials.close();
	}
});

test("an unreachable URL is retried on the backoff while the app answers", async () => {
	using clock = manual();
	const dials = new Dials();
	let dialed = 0;
	globalThis.WebTransport = function Unreachable() {
		dialed++;
		throw new Error("unreachable");
	} as unknown as typeof WebTransport;
	const reload = new Reload({ url: URL_, websocket: { enabled: false }, tokens: dials, backoff: BACKOFF });
	try {
		for (let i = 0; i < 3; i++) {
			const dial = await clock.drive(dials.next());
			if (i > 0) expect(dial?.failure?.message).toContain("unreachable");
			dial?.present(["token"]);
		}
		expect(dialed).toBeGreaterThanOrEqual(2);
		expect(reload.error.peek()).toBeUndefined();
		// Failing the dial is how the app gives up.
		const last = await clock.drive(dials.next());
		last?.fail(new Error("gave up"));
		const err = await clock.drive(waitFor(reload.error, (error) => error !== undefined));
		expect(err?.message).toBe("gave up");
	} finally {
		reload.close();
		dials.close();
	}
});

test("a dial left unanswered times out like a slow attempt, and the loop asks again", async () => {
	using clock = manual();
	relay();
	const dials = new Dials();
	const reload = new Reload({
		url: URL_,
		websocket: { enabled: false },
		tokens: dials,
		timeout: Duration.millis(50),
		backoff: BACKOFF,
	});
	try {
		const first = await clock.drive(dials.next());
		await clock.advance(Duration.millis(50));
		expect(first?.signal.aborted).toBe(true);
		const second = await clock.drive(dials.next());
		expect(second?.failure?.name).toBe("ConnectTimeout");
		second?.present(["late"]);
		await clock.drive(reload.answered());
		expect(reload.status.peek()).toBe("connected");
	} finally {
		reload.close();
		dials.close();
	}
});

test("a dial the app fails stops the connection with its error", async () => {
	using clock = manual();
	const served = relay();
	const dials = new Dials();
	const reload = new Reload({ url: URL_, websocket: { enabled: false }, tokens: dials, backoff: BACKOFF });
	try {
		const dial = await clock.drive(dials.next());
		expect(dial?.url.href).toBe(URL_.href);
		dial?.fail(new Error("no key"));
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
