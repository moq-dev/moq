import { type Auth, type Handle, Unsupported } from "../auth.ts";
import type { Established } from "./established.ts";

/** What one dial presents, decided from its URL, the offered versions, and the tokens. @internal */
export interface Credential {
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
export function credential(url: URL, tokens: readonly string[], setupToken: boolean): Credential {
	const jwt = url.searchParams.get("jwt");
	if (jwt) return { url, connection: jwt };
	const first = tokens[0];
	if (first === undefined) return { url };
	return { url: setupToken ? url : withJwt(url, first), setup: first, connection: first };
}

/** `url` with `jwt` appended to its query. @internal */
export function withJwt(url: URL, jwt: string): URL {
	const next = new URL(url);
	next.searchParams.append("jwt", jwt);
	return next;
}

/** The tokens to hold on their own AUTH streams, each once. @internal */
export function extra(credential: Credential, tokens: readonly string[]): string[] {
	return [...new Set(tokens)].filter((token) => token !== credential.connection);
}

interface Entry {
	auth?: Auth;
	pending: boolean;
	retired: boolean;
}

/**
 * The configured tokens one session holds on their own AUTH streams.
 *
 * A change presents the new tokens at once and withdraws the ones they replace only after
 * every new one has its answer, so the grant never narrows in between.
 *
 * @internal
 */
export class Held {
	#handle: Handle;
	#credential: Credential;
	#entries = new Map<string, Entry>();
	#retiring: Entry[] = [];

	constructor(handle: Handle, credential: Credential) {
		this.#handle = handle;
		this.#credential = credential;
	}

	/** Hold exactly `tokens`, other than the connection credential. */
	update(tokens: readonly string[]) {
		const next = new Map<string, Entry>();
		for (const token of extra(this.#credential, tokens)) {
			next.set(token, this.#entries.get(token) ?? this.#present(token));
		}
		for (const [token, entry] of this.#entries) {
			if (!next.has(token)) this.#retiring.push(entry);
		}
		this.#entries = next;
		this.#settle();
	}

	#present(token: string): Entry {
		const entry: Entry = { pending: true, retired: false };
		// Registered synchronously, so a request sent right after waits on its answer.
		this.#handle
			.add(token)
			.then(
				(auth) => {
					entry.auth = auth;
					// Replaced while waiting, and every replacement has answered since.
					if (entry.retired) auth.close();
				},
				(err: unknown) => {
					if (err instanceof Unsupported) console.debug("the peer takes no tokens in band");
					else console.warn("a configured token was refused:", err);
				},
			)
			.finally(() => {
				entry.pending = false;
				this.#settle();
			});
		return entry;
	}

	#settle() {
		if ([...this.#entries.values()].some((entry) => entry.pending)) return;
		for (const entry of this.#retiring) {
			entry.retired = true;
			entry.auth?.close();
		}
		this.#retiring = [];
	}
}

// The tokens each connection holds, so a reconnecting wrapper can update them.
const held = new WeakMap<Established, Held>();

/** Present `tokens` on `connection`, beside the credential its dial carried. @internal */
export function hold(connection: Established, credential: Credential, tokens: readonly string[]): Held {
	const tokensHeld = new Held(connection.auth, credential);
	tokensHeld.update(tokens);
	held.set(connection, tokensHeld);
	return tokensHeld;
}

/** The tokens `connection` holds, if it was dialed with any. @internal */
export function heldOf(connection: Established): Held | undefined {
	return held.get(connection);
}
