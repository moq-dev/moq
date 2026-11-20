import { expect, test } from "bun:test";
import { Duration } from "@moq/clock";
import { Signal } from "@moq/signals";
import type { Request } from "../auth.ts";
import { SessionCode, SessionError } from "../error.ts";
import * as Lite from "../lite/index.ts";
import { createMockTransportPair, type MockTransport, manual, waitFor } from "../mock.ts";
import * as Path from "../path.ts";
import { wireOf } from "../wire.ts";
import { accept as acceptSession } from "./index.ts";
import { Connection } from "./pool.ts";
import { Reload } from "./reload.ts";

const URL_ = new URL("https://example.com/");
const DELAY = { initial: Duration.millis(1), multiplier: 2, max: Duration.millis(1), timeout: null };

/** The token a server's request carries as text, where the connection's own credential is "connection". */
function named(request: Request): string {
	return request.token.byteLength === 0 ? "connection" : new TextDecoder().decode(request.token);
}

/** Grants everything and refuses nothing. */
const GRANT = { publish: new Path.Patterns([Path.Pattern.all()]), subscribe: new Path.Patterns() };

/**
 * A lite-07 relay that answers each token itself: `decide` accepts or rejects the request, and
 * may hold it, such as a token a test answers later.
 */
function relay(decide: (token: string, request: Request) => void) {
	const stub = function StubWebTransport(target: string | URL) {
		const pair = createMockTransportPair(Lite.ALPN_07_WIP);
		void acceptSession({ transport: pair.server, url: new URL(target) }).then(async (session) => {
			const requests = session.auth.requests();
			for (;;) {
				const request = await requests.next();
				if (!request) break;
				decide(named(request), request);
			}
		});
		return pair.client;
	};
	return stub as unknown as typeof WebTransport;
}

/**
 * A lite-06 relay, which has no AUTH: it accepts any credential but `refused`, whose session it
 * closes once `release` resolves, before its SETUP. `dials` counts the dials, and `served` holds
 * the server side of each session it accepted.
 */
function closing(refused: string, release: Promise<void>) {
	const served: MockTransport[] = [];
	const dials = new Signal(0);
	const stub = function StubWebTransport(target: string | URL) {
		dials.update((n) => n + 1);
		const pair = createMockTransportPair(Lite.ALPN_06);
		const url = new URL(target);
		if (url.searchParams.get("jwt") === refused) {
			void release.then(() => pair.server.close({ closeCode: SessionCode.Unauthorized, reason: "unauthorized" }));
		} else {
			served.push(pair.server);
			void acceptSession({ transport: pair.server, url });
		}
		return pair.client;
	};
	return { dials, served, stub: stub as unknown as typeof WebTransport };
}

function loop(versions: string[], tokens: Signal<readonly string[]>) {
	return new Reload({ url: URL_, versions, websocket: { enabled: false }, tokens, backoff: DELAY });
}

/** Whether `promise` rejected with a session close of `code`, once everything pending has run. */
async function refusal(promise: Promise<unknown>): Promise<SessionError | undefined> {
	try {
		await promise;
	} catch (err) {
		return err instanceof SessionError ? err : undefined;
	}
	return undefined;
}

/** Whether `promise` rejected, once everything pending has run. */
async function rejection(promise: Promise<unknown>): Promise<void> {
	let rejected = false;
	try {
		await promise;
	} catch {
		rejected = true;
	}
	expect(rejected).toBe(true);
}

test("a set granted in band answers, and a later set the peer refuses answers with its refusal", async () => {
	using clock = manual();
	const original = globalThis.WebTransport;
	globalThis.WebTransport = relay((token, request) => {
		if (token === "revoked") request.reject(SessionCode.Unauthorized, "no");
		else request.accept(GRANT);
	});
	const tokens = new Signal<readonly string[]>(["connection", "a"]);
	const reload = loop(["moq-lite-07-wip"], tokens);
	try {
		await clock.drive(reload.answered());

		tokens.set(["connection", "revoked"]);
		const err = await clock.drive(refusal(reload.answered()));
		expect(err?.code).toBe(SessionCode.Unauthorized);
	} finally {
		reload.close();
		globalThis.WebTransport = original;
	}
});

test("a close that refuses the connection credential answers a session without AUTH", async () => {
	using clock = manual();
	const original = globalThis.WebTransport;
	const { promise: release, resolve } = Promise.withResolvers<void>();
	const { dials, served, stub } = closing("old", release);
	globalThis.WebTransport = stub;
	const tokens = new Signal<readonly string[]>(["ok"]);
	const reload = loop(["moq-lite-06"], tokens);
	try {
		// The server's SETUP accepts the first credential, so the refusing session's close is
		// retried rather than ending the URL.
		await clock.drive(waitFor(reload.established, (established) => established !== undefined));
		// Swapped while no session is live, so no session takes it in band.
		served[0]?.close({ closeCode: 0, reason: "restart" });
		await clock.drive(waitFor(reload.established, (established) => established === undefined));
		tokens.set(["old"]);
		await clock.drive(waitFor(dials, (n) => n >= 2));
		await clock.advance(Duration.millis(10));
		const waiting = refusal(reload.answered());
		resolve();
		const err = await clock.drive(waiting);
		expect(err?.code).toBe(SessionCode.Unauthorized);

		// The replacement is accepted by the server's SETUP, which is the answer without AUTH.
		tokens.set(["new"]);
		await clock.drive(reload.answered());
	} finally {
		reload.close();
		globalThis.WebTransport = original;
	}
});

test("a credential in a URL parameter is judged for each dial like a token", async () => {
	using clock = manual();
	const original = globalThis.WebTransport;
	const dialed: URL[] = [];
	const served: MockTransport[] = [];
	// A relay that reads its credential from the URL and refuses "revoked" in band.
	globalThis.WebTransport = function StubWebTransport(target: string | URL) {
		const url = new URL(target);
		dialed.push(url);
		const pair = createMockTransportPair(Lite.ALPN_07_WIP);
		served.push(pair.server);
		void acceptSession({ transport: pair.server, url }).then(async (session) => {
			const requests = session.auth.requests();
			for (;;) {
				const request = await requests.next();
				if (!request) break;
				if (url.searchParams.get("credential") === "revoked") {
					request.reject(SessionCode.Unauthorized, "no");
					pair.server.close({ closeCode: SessionCode.Unauthorized, reason: "no" });
				} else request.accept(GRANT);
			}
		});
		return pair.client;
	} as unknown as typeof WebTransport;
	const tokens = new Signal<readonly string[]>(["first"]);
	const reload = new Reload({
		url: URL_,
		versions: ["moq-lite-07-wip"],
		websocket: { enabled: false },
		tokens,
		credentialParam: "credential",
		backoff: DELAY,
	});
	try {
		await clock.drive(reload.answered());
		expect(dialed[0]?.searchParams.get("credential")).toBe("first");
		expect(dialed[0]?.searchParams.has("jwt")).toBe(false);
		// The configured URL stays the key, so a new credential never starts another sequence.
		expect(reload.url.peek()?.href).toBe(URL_.href);

		// The relay drops the session; the redial presents the replacement, which it refuses.
		served[0]?.close({ closeCode: 0, reason: "restart" });
		await clock.drive(waitFor(reload.established, (established) => established === undefined));
		tokens.set(["revoked"]);
		const err = await clock.drive(refusal(reload.answered()));
		expect(err?.code).toBe(SessionCode.Unauthorized);
		expect(dialed.at(-1)?.searchParams.get("credential")).toBe("revoked");

		// The loop is still live, and the next credential is judged on its own.
		tokens.set(["second"]);
		await clock.drive(reload.answered());
		expect(dialed.at(-1)?.searchParams.get("credential")).toBe("second");
		expect(reload.error.peek()).toBeUndefined();
	} finally {
		reload.close();
		globalThis.WebTransport = original;
	}
});

test("a lite-07 session up past the healthy bar without an AUTH_OK never reads as connected", async () => {
	using clock = manual();
	const original = globalThis.WebTransport;
	const held = new Signal<readonly Request[]>([]);
	globalThis.WebTransport = relay((_token, request) => held.update((requests) => [...requests, request]));
	const reload = loop(["moq-lite-07-wip"], new Signal<readonly string[]>([]));
	const statuses: string[] = [];
	const unsubscribe = reload.status.subscribe((status) => statuses.push(status));
	try {
		const [credential] = await clock.drive(waitFor(held, (requests) => requests.length > 0));
		// Uptime stands in for an answer only on moq-lite 03/04, which have none.
		await clock.advance(Duration.millis(5000));
		expect(statuses).not.toContain("connected");
		expect(reload.established.peek()).toBeUndefined();

		credential?.accept(GRANT);
		await clock.drive(waitFor(reload.status, (status) => status === "connected"));
		expect(reload.established.peek()).toBeDefined();
	} finally {
		unsubscribe();
		reload.close();
		globalThis.WebTransport = original;
	}
});

test("a kept credential counts only once the peer refuses it", async () => {
	using clock = manual();
	const original = globalThis.WebTransport;
	const issued: { revoke(code: SessionCode, reason: string): void }[] = [];
	globalThis.WebTransport = relay((token, request) => {
		const grant = request.accept(GRANT);
		if (token === "connection") issued.push(grant);
	});
	const tokens = new Signal<readonly string[]>(["connection", "a", "b"]);
	const reload = loop(["moq-lite-07-wip"], tokens);
	try {
		await clock.drive(reload.answered());

		// Nothing new to grant, and the credential still stands.
		tokens.set(["connection", "a"]);
		await clock.drive(reload.answered());

		issued[0]?.revoke(SessionCode.Unauthorized, "removed");
		const session = reload.established.peek();
		if (!session) throw new Error("not connected");
		await clock.drive(waitFor(wireOf(session).auth.credential, (verdict) => verdict instanceof Error));
		tokens.set(["connection"]);
		const err = await clock.drive(refusal(reload.answered()));
		expect(err?.code).toBe(SessionCode.Unauthorized);
	} finally {
		reload.close();
		globalThis.WebTransport = original;
	}
});

test("a kept credential the peer ends cleanly after granting it refuses the next set", async () => {
	using clock = manual();
	const original = globalThis.WebTransport;
	const issued: { close(): void }[] = [];
	globalThis.WebTransport = relay((token, request) => {
		const grant = request.accept(GRANT);
		if (token === "connection") issued.push(grant);
	});
	const tokens = new Signal<readonly string[]>(["connection", "a"]);
	const reload = loop(["moq-lite-07-wip"], tokens);
	try {
		await clock.drive(reload.answered());

		issued[0]?.close();
		const session = reload.established.peek();
		if (!session) throw new Error("not connected");
		await clock.drive(waitFor(wireOf(session).auth.credential, (verdict) => verdict instanceof Error));

		// The set keeps the dropped credential, so it is refused rather than answered by "b" alone.
		tokens.set(["connection", "b"]);
		await clock.drive(rejection(reload.answered()));
	} finally {
		reload.close();
		globalThis.WebTransport = original;
	}
});

test("a set replaced before its answer moves the wait to the replacement", async () => {
	using clock = manual();
	const original = globalThis.WebTransport;
	globalThis.WebTransport = relay((token, request) => {
		// "slow" is never answered.
		if (token !== "slow") request.accept(GRANT);
	});
	const tokens = new Signal<readonly string[]>(["connection"]);
	const reload = loop(["moq-lite-07-wip"], tokens);
	try {
		await clock.drive(reload.answered());

		tokens.set(["connection", "slow"]);
		let answered = false;
		const waiting = reload.answered().then(() => {
			answered = true;
		});
		await clock.advance(Duration.millis(10));
		expect(answered).toBe(false);

		tokens.set(["connection", "fast"]);
		await clock.drive(waiting);
		expect(answered).toBe(true);
	} finally {
		reload.close();
		globalThis.WebTransport = original;
	}
});

test("a connection answers once its loop ends with the abort's error", async () => {
	using clock = manual();
	const original = globalThis.WebTransport;
	globalThis.WebTransport = relay((token, request) => {
		if (token !== "slow") request.accept(GRANT);
	});
	const tokens = new Signal<readonly string[]>(["connection", "slow"]);
	const connection = new Connection({
		url: URL_,
		versions: ["moq-lite-07-wip"],
		websocket: { enabled: false },
		tokens,
		backoff: DELAY,
	});
	try {
		await clock.advance(Duration.millis(10));
		const waiting = refusal(connection.answered());
		connection.auth.peek()?.abort(new SessionError(SessionCode.Unauthorized));
		const err = await clock.drive(waiting);
		expect(err?.code).toBe(SessionCode.Unauthorized);
	} finally {
		connection.close();
		globalThis.WebTransport = original;
	}
});

test("a first dial the peer refuses answers with its failure", async () => {
	using clock = manual();
	const original = globalThis.WebTransport;
	// Closed before the handshake finishes, so the dial never yields a session.
	globalThis.WebTransport = function StubWebTransport() {
		const pair = createMockTransportPair(Lite.ALPN_06);
		pair.server.close({ closeCode: SessionCode.Unauthorized, reason: "unauthorized" });
		return pair.client;
	} as unknown as typeof WebTransport;
	const reload = loop(["moq-lite-06"], new Signal<readonly string[]>(["old"]));
	try {
		const err = await clock.drive(refusal(reload.answered()));
		expect(err?.code).toBe(SessionCode.Unauthorized);
	} finally {
		reload.close();
		globalThis.WebTransport = original;
	}
});

test("releasing a connection that holds no loop ends its wait", async () => {
	const connection = new Connection({ tokens: ["old"] });
	const waiting = connection.answered();
	connection.close();
	await expect(waiting).rejects.toThrow("connection closed");
});
