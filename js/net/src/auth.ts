/**
 * In-band authorization: present tokens to the peer and learn what they grant.
 *
 * Each side of a moq-lite-06 session presents the credential its connection already
 * carried (the URL, or nothing) right after setup, and learns the {@link Grant} it earned.
 * {@link Auth.grant} is the union of every token this side presented, and {@link Auth.add}
 * presents another without reconnecting. Mirrors the Rust `moq_net::auth`.
 *
 * @module
 */

import { type Getter, Signal } from "@moq/signals";
import type { SessionCode } from "./error.ts";
import type * as Path from "./path.ts";

/**
 * What a peer lets this side do, in this side's own paths (relative to the session).
 *
 * An empty {@link Path.Patterns} grants nothing; `**` grants everything.
 */
export interface Grant {
	/** The broadcasts this side may publish to the peer. */
	publish: Path.Patterns;
	/** The broadcasts this side may subscribe to from the peer. */
	subscribe: Path.Patterns;
	/** When the grant lapses, in `Date.now()` milliseconds, or undefined for never. */
	expires?: number;
}

/** Whether two grants allow the same paths and lapse together. */
export function grantsEqual(a: Grant | undefined, b: Grant | undefined): boolean {
	if (a === undefined || b === undefined) return a === b;
	return a.publish.equals(b.publish) && a.subscribe.equals(b.subscribe) && a.expires === b.expires;
}

/** The session or peer carries no AUTH exchange, or takes no tokens in band. */
export class Unsupported extends Error {
	constructor(message = "auth unsupported") {
		super(message);
		this.name = "Unsupported";
	}
}

/** A token this side presented. {@link Token.close} withdraws it. */
export interface Token {
	/** This token's own grant: undefined until the peer answers, and again once it ends. */
	readonly grant: Getter<Grant | undefined>;
	/** Settles when the token ends: the peer's revocation, or its withdrawal. */
	readonly closed: PromiseLike<Error | null>;
	/** Withdraw the token, removing its grant from the union. */
	close(): void;
}

/** A token the peer presented, waiting for an answer. Answer every one. */
export interface Request {
	/** The token the peer presented. Empty means the credential its connection carried. */
	readonly token: Uint8Array;
	/**
	 * Grant the token until the returned {@link Issued} is revoked or closed.
	 *
	 * The session only tells the peer; the origins this side serves and accepts are
	 * what enforce the grant, and revoking before `expires` is the acceptor's job.
	 */
	accept(grant: Grant): Issued;
	/** Refuse the token with a session code and a reason for the peer. */
	reject(code: SessionCode, reason: string): void;
}

/** A grant issued to one of the peer's tokens. */
export interface Issued {
	/** Replace the grant, such as with a lowered expiry. */
	update(grant: Grant): void;
	/** Revoke the grant with a session code and a reason for the peer. */
	revoke(code: SessionCode, reason: string): void;
	/** End the grant without a reason. */
	close(): void;
	/** Settles once the peer withdraws the token or its stream ends. */
	readonly closed: PromiseLike<Error | null>;
}

/** The tokens the peer presents, for an application that answers them itself. */
export interface Requests {
	/** The next token the peer presents, or undefined once the session ends. */
	next(): Promise<Request | undefined>;
	/** Refuse every later token. */
	close(): void;
}

/** A session's tokens and grants. See the module docs. */
export interface Auth {
	/** The union of every grant this side holds: undefined until the peer first answers. */
	readonly grant: Getter<Grant | undefined>;

	/**
	 * Present another token, resolving once the peer answers it. Rejects with
	 * {@link Unsupported} when the session or the peer takes no tokens in band, or with the
	 * peer's refusal as an `Error.Session`.
	 */
	add(token: string | Uint8Array): Promise<Token>;

	/**
	 * Answer every token the peer presents, instead of the default: granting what this side
	 * publishes and consumes for the connection's own credential and refusing any other.
	 *
	 * Call it as soon as connect or accept resolves, in the same task: whoever answers is
	 * decided once, right after. Throws once that has happened.
	 */
	requests(): Requests;
}

/** The {@link Auth} of a session without AUTH: no grant, and no tokens. */
export class None implements Auth {
	readonly grant: Getter<Grant | undefined> = new Signal<Grant | undefined>(undefined);

	add(_token: string | Uint8Array): Promise<Token> {
		return Promise.reject(new Unsupported());
	}

	requests(): Requests {
		return { next: async () => undefined, close: () => {} };
	}
}
