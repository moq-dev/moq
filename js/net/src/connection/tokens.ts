import { type Getter, Signal } from "@moq/signals";
import { type Handle, Unsupported } from "../auth.ts";
import { ending, type Pending } from "../auth_session.ts";
import { error } from "../error.ts";
import { wireOf } from "../wire.ts";
import type { Session } from "./session.ts";

/** What one dial presents, decided from its URL, the offered versions, and the tokens. @internal */
export interface Presentation {
	/** The URL to dial, carrying the connection credential when an offered version has no SETUP token. */
	url: URL;
	/** The token SETUP carries, unless the URL brought its own credential. */
	setup?: string;
	/** The credential the connection presents, from the URL or SETUP. Never also presented on AUTH. */
	connection?: string;
}

/**
 * Decide what one dial presents.
 *
 * A URL that already carries a `jwt` keeps it (a per-peer credential), so every token rides
 * AUTH. Otherwise the first token is the connection credential: it rides SETUP, and the URL
 * too when `setupToken` is false, since the URL is fixed before the version is negotiated.
 *
 * @internal
 */
export function credential(url: URL, tokens: readonly string[], setupToken: boolean): Presentation {
	const jwt = url.searchParams.get("jwt");
	if (jwt) return { url, connection: jwt };
	const first = tokens[0];
	if (first === undefined) return { url };
	return { url: setupToken ? url : withJwt(url, first), setup: first, connection: first };
}

/**
 * Decide what a dial to the WebSocket fallback's own URL presents.
 *
 * The fallback presents the credential its URL decides, never one taken from the primary:
 * its own `jwt`, else the primary URL's `jwt`, else the configured tokens as {@link credential}
 * decides them.
 *
 * @internal
 */
export function fallback(primary: URL, url: URL, tokens: readonly string[], setupToken: boolean): Presentation {
	const inherited = primary.searchParams.get("jwt");
	if (inherited && !url.searchParams.get("jwt")) return credential(withJwt(url, inherited), tokens, setupToken);
	return credential(url, tokens, setupToken);
}

/** `url` with `jwt` appended to its query. @internal */
export function withJwt(url: URL, jwt: string): URL {
	const next = new URL(url);
	next.searchParams.append("jwt", jwt);
	return next;
}

/** The tokens to hold on their own AUTH streams, each once. @internal */
export function extra(credential: Presentation, tokens: readonly string[]): string[] {
	return [...new Set(tokens)].filter((token) => token !== credential.connection);
}

interface Entry {
	auth?: Pending;
	pending: boolean;
	/** Withdrawn by us, so its rejected answer is no refusal. */
	withdrawn: boolean;
	/** The peer's first verdict: `null` once granted, its refusal, or undefined while unanswered. */
	answer?: Error | null;
	/** Why the peer ended the token for good: its refusal or revocation, or the handle's end. */
	end?: Error;
}

/**
 * The configured tokens an auth handle holds on their own AUTH streams, beside the
 * latest dial's connection credential. A handle shared across reconnects keeps them on
 * every session it backs.
 *
 * A change presents the new tokens at once and withdraws the granted ones they replace only
 * after every new one has its answer, so the grant never narrows in between.
 *
 * @internal
 */
export class Held {
	#handle: Handle;
	#credential: Presentation;
	#entries = new Map<string, Entry>();
	#retiring: Entry[] = [];
	#changed = new Signal(0);

	constructor(handle: Handle, credential: Presentation) {
		this.#handle = handle;
		this.#credential = credential;
	}

	/** Bumps whenever a held token is answered or ends. */
	get changed(): Signal<number> {
		return this.#changed;
	}

	/** The credential the latest dial presented as the connection's, which no AUTH stream carries. */
	get connection(): string | undefined {
		return this.#credential.connection;
	}

	/** Whether `token` rides its own AUTH stream. */
	has(token: string): boolean {
		return this.#entries.has(token);
	}

	/**
	 * The peer's first verdict on held `token`: `null` once granted in band, its refusal, or
	 * undefined while it has none. A token only a request carries is unsupported, not granted.
	 */
	verdict(token: string): Error | null | undefined {
		return this.#entries.get(token)?.answer;
	}

	/** Why the peer ended the first of `tokens` that is held, for good. */
	ended(tokens: readonly string[]): Error | undefined {
		for (const token of tokens) {
			const end = this.#entries.get(token)?.end;
			if (end) return end;
		}
		return undefined;
	}

	/** A new dial on the handle, whose credential decides which tokens ride AUTH. */
	dialed(credential: Presentation, tokens: readonly string[]) {
		this.#credential = credential;
		this.update(tokens);
	}

	/** Hold exactly `tokens`, other than the connection credential. */
	update(tokens: readonly string[]) {
		const next = new Map<string, Entry>();
		for (const token of extra(this.#credential, tokens)) {
			next.set(token, this.#entries.get(token) ?? this.#present(token));
		}
		for (const [token, entry] of this.#entries) {
			if (next.has(token)) continue;
			// Only a granted token keeps the grant wide; one refused or still unanswered, such as
			// one retrying while its backend fails, is withdrawn at once so repeated updates never
			// pile up retrying streams.
			if (entry.auth?.held) this.#retiring.push(entry);
			else this.#withdraw(entry);
		}
		this.#entries = next;
		this.#settle();
	}

	#present(token: string): Entry {
		const entry: Entry = { pending: true, withdrawn: false };
		const refused = (err: unknown) => {
			if (entry.withdrawn) return;
			if (err instanceof Unsupported) console.debug("the peer takes no tokens in band");
			else console.warn("a configured token was refused:", err);
		};
		let auth: Pending;
		try {
			// Registered synchronously, so a request sent right after waits on its answer.
			auth = this.#handle.present(token);
		} catch (err) {
			refused(err);
			entry.answer = error(err);
			entry.end = entry.answer;
			entry.pending = false;
			return entry;
		}
		entry.auth = auth;
		auth.answered
			.then(
				() => {
					entry.answer = auth.inband ? null : new Unsupported();
				},
				(err: unknown) => {
					entry.answer = error(err);
					refused(err);
				},
			)
			.finally(() => {
				entry.pending = false;
				this.#changed.update((n) => n + 1);
				this.#settle();
			});
		// Withdrawn by us, it ends with no verdict of the peer's.
		void auth.closed.then((end) => {
			if (entry.withdrawn) return;
			entry.end = end ?? new Error("the peer ended the token");
			this.#changed.update((n) => n + 1);
		});
		return entry;
	}

	#withdraw(entry: Entry) {
		entry.withdrawn = true;
		entry.auth?.close();
	}

	#settle() {
		if ([...this.#entries.values()].some((entry) => entry.pending)) return;
		for (const entry of this.#retiring) this.#withdraw(entry);
		this.#retiring = [];
	}
}

// The tokens each auth handle holds, so a handle shared across reconnects holds them once
// and a reconnecting wrapper can update them.
const held = new WeakMap<Handle, Held>();

/** Present `tokens` on `connection`, beside the credential its dial carried. @internal */
export function hold(connection: Session, credential: Presentation, tokens: readonly string[]): Held {
	const existing = held.get(connection.auth);
	if (existing) {
		existing.dialed(credential, tokens);
		return existing;
	}
	const tokensHeld = new Held(connection.auth, credential);
	tokensHeld.update(tokens);
	held.set(connection.auth, tokensHeld);
	return tokensHeld;
}

/** The tokens `connection` holds, if it was dialed with any. @internal */
export function heldOf(connection: Session): Held | undefined {
	return held.get(connection.auth);
}

/** The latest dial's session, whose own answer to the connection credential counts. */
interface Connected {
	credential: Getter<Error | null | undefined>;
	/** Why the session closed, once it did. */
	closed?: Error;
}

/**
 * The peer's verdict on the tokens a connection last set, across its reconnects, like Rust's
 * `Tokens::poll_answered`.
 *
 * It covers the tokens the latest set newly presented, or the initial ones before any set: each
 * is granted in band, or is the connection credential a dial carried and the peer accepted.
 * A token the set kept, the connection credential included, counts only once the peer refuses
 * it. The first verdict is the answer, and nothing is re-checked after it: a later revocation, or
 * a new session that refuses a token an earlier one granted, changes nothing.
 *
 * @internal
 */
export class Verdict {
	#source: Getter<readonly string[]>;
	#tokens: readonly string[];
	// The tokens the latest set newly presented whose verdict is not yet known.
	#fresh: string[];
	// The first verdict on the latest set: null once granted.
	#latched?: { outcome: Error | null };
	// The connection credential the latest dial presented, once it connected.
	#dialed?: string;
	#connected?: Connected;
	#held?: Held;
	// Why the loop ended, which answers a set no session will.
	#closed?: Error;
	// Why the loop stopped retrying its URL, such as a refused first dial: no session will answer
	// until a new sequence dials, so a waiting set fails with it, but it is no verdict to latch.
	#gaveUp?: Error;
	// Bumped whenever an input changes, so a waiting answer looks again.
	#epoch = new Signal(0);
	#stops: (() => void)[] = [];
	// Stops watching the latest session's credential.
	#unwatch?: () => void;

	constructor(source: Getter<readonly string[]>, handle: Handle) {
		this.#source = source;
		this.#tokens = source.peek();
		this.#fresh = [...new Set(this.#tokens)];
		this.#stops.push(
			ending(handle).subscribe((err) => {
				if (err) this.#end(err);
			}),
		);
	}

	/** Resolve once the peer first answers the latest set: rejects with its refusal, `Unsupported` when a session cannot carry a token in band, or the connection's end. */
	async answered(): Promise<void> {
		for (;;) {
			this.#sync();
			this.#record();
			if (this.#latched) {
				if (this.#latched.outcome) throw this.#latched.outcome;
				return;
			}
			if (this.#gaveUp) throw this.#gaveUp;
			await this.#epoch.changed();
		}
	}

	/** A dial starts: its session answers the connection credential from here, so the last one's answer is kept first. */
	dialing(): void {
		this.#sync();
		this.#record();
		this.#gaveUp = undefined;
		this.#dialed = undefined;
		this.#connected = undefined;
		this.#release();
		this.#notify();
	}

	/** The latest dial built `session`, even one that already closed: that close may be its answer. */
	connected(session: Session): void {
		this.#sync();
		this.#record();
		this.#release();
		const held = heldOf(session);
		if (held && held !== this.#held) this.#stops.push(held.changed.subscribe(() => this.#notify()));
		this.#held = held;
		this.#dialed = held?.connection;
		const connected: Connected = { credential: wireOf(session).auth.credential };
		this.#connected = connected;
		this.#unwatch = connected.credential.subscribe(() => this.#notify());
		void session.closed.then((end) => {
			// The close is the answer of a session still unanswered: the loop redials with the same credential.
			connected.closed = end ?? new Error("the session closed before the peer accepted its credential");
			if (this.#connected === connected) this.#notify();
		});
		this.#notify();
	}

	/** The loop stopped retrying its URL with `err`, which fails a set still waiting for a session. */
	gaveUp(err: Error): void {
		this.#gaveUp = err;
		this.#notify();
	}

	/** The tokens the source holds may have changed, such as a set the effect has yet to see. */
	refresh(): void {
		this.#notify();
	}

	/** The loop ended with `err`, which answers a set no session will. */
	#end(err: Error): void {
		this.#sync();
		this.#record();
		this.#closed ??= err;
		this.#release();
		for (const stop of this.#stops.splice(0)) stop();
		this.#notify();
	}

	/** Take the tokens the source holds now, before an effect would have. */
	#sync(): void {
		const tokens = this.#source.peek();
		if (tokens === this.#tokens || sameTokens(tokens, this.#tokens)) return;
		const kept = new Set(this.#tokens);
		// A token still waiting on its verdict stays fresh when the new set keeps it.
		const fresh = [...new Set(tokens)].filter(
			(token) => !kept.has(token) || (this.#fresh.includes(token) && this.#verdict(token) === undefined),
		);
		this.#fresh = fresh;
		this.#tokens = tokens;
		this.#latched = undefined;
	}

	#release(): void {
		this.#unwatch?.();
		this.#unwatch = undefined;
	}

	#notify(): void {
		this.#sync();
		this.#record();
		this.#epoch.update((n) => n + 1);
	}

	/** The connection credential the latest dial presented, when the tokens still name it. */
	#named(): string | undefined {
		const connection = this.#dialed;
		return connection !== undefined && this.#tokens.includes(connection) ? connection : undefined;
	}

	/** The peer's verdict on the connection credential: `null` once accepted, undefined while it has none or no session is live. */
	#credential(): Error | null | undefined {
		const connected = this.#connected;
		if (!connected) return undefined;
		// A session closed before the peer answered is cut off: its close is the answer.
		const own = connected.credential.peek();
		return own !== undefined ? own : connected.closed;
	}

	/** The peer's verdict on `token` of the latest set, undefined while it has none. */
	#verdict(token: string): Error | null | undefined {
		if (this.#held?.has(token)) return this.#held.verdict(token);
		// Not presented until the first dial.
		return this.#named() === token ? this.#credential() : undefined;
	}

	/** The answer to the latest set: the first refusal of any token it names, or `null` once every fresh token is granted. Undefined while one still waits. */
	#answer(): Error | null | undefined {
		if (this.#latched) return this.#latched.outcome;
		if (this.#closed) return this.#closed;
		const ended = this.#held?.ended(this.#tokens);
		if (ended) return ended;
		const credential = this.#named() === undefined ? undefined : this.#credential();
		if (credential) return credential;
		let waiting = false;
		for (const token of this.#fresh) {
			const verdict = this.#verdict(token);
			if (verdict) return verdict;
			if (verdict === undefined) waiting = true;
		}
		return waiting ? undefined : null;
	}

	/** Keep each verdict the latest set has so far, before a dial, a new session, or a dropped one takes away where it came from. */
	#record(): void {
		if (this.#latched) return;
		const outcome = this.#answer();
		if (outcome !== undefined) {
			if (outcome === null) this.#fresh = [];
			this.#latched = { outcome };
			return;
		}
		this.#fresh = this.#fresh.filter((token) => this.#verdict(token) !== null);
	}
}

function sameTokens(a: readonly string[], b: readonly string[]): boolean {
	return a.length === b.length && a.every((token, i) => token === b[i]);
}
