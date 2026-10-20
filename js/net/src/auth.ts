/**
 * In-band authorization: present tokens to the peer and learn what they grant.
 *
 * Each side of a moq-lite-07 session, and of a moq-transport draft-17+ session when both
 * sides negotiate MoQ Auth, presents the credential its connection already carried (the
 * URL, or nothing) right after setup, and learns the {@link Grant} it earned. Older
 * versions, and peers that do not negotiate it, carry no AUTH exchange: the grant stays
 * undefined. There {@link Handle.add} rejects with {@link Unsupported} on moq-lite, and on
 * moq-transport keeps the token, with no grant, for the announcements whose route names it.
 * {@link Handle.grant} is the union of every token this side presented, and {@link Handle.add}
 * presents another without reconnecting, returning its {@link Auth}. Mirrors the Rust
 * `moq_net::auth`.
 *
 * @module
 */

import { type Getter, Signal } from "@moq/signals";
import { SessionCode } from "./error.ts";
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
	/** When the grant lapses, as `@moq/clock` wall time in Unix milliseconds, or undefined for never. */
	expires?: number;
}

/** Whether two grants allow the same paths and lapse together. */
export function grantsEqual(a: Grant | undefined, b: Grant | undefined): boolean {
	if (a === undefined || b === undefined) return a === b;
	return a.publish.equals(b.publish) && a.subscribe.equals(b.subscribe) && a.expires === b.expires;
}

/** The session or peer carries no AUTH exchange, or takes no tokens in band. */
export class Unsupported extends Error {
	/** The moq-lite session code for this condition. */
	readonly code = SessionCode.Unsupported;
	constructor(message = "auth unsupported") {
		super(message);
		this.name = "Unsupported";
	}
}

/** A token this side presented, which a route can name. {@link Auth.close} withdraws it. */
export interface Auth {
	/** This token's own grant: undefined until the peer answers, and again once it ends. */
	readonly grant: Getter<Grant | undefined>;
	/** Settles when the token ends: the peer's revocation, or its withdrawal. */
	readonly closed: PromiseLike<Error | null>;
	/** Withdraw the token, removing its grant from the union and remaking any request that carries it without it. */
	close(): void;
}

/** A token the peer presented, waiting for an answer. Answer every one. */
export interface Request {
	/** The token the peer presented. Empty means the credential its connection carried. */
	readonly token: Uint8Array;
	/** Settles when the peer withdraws this unanswered token or closes the session. */
	readonly closed: PromiseLike<Error | null>;
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
export interface Handle {
	/** The union of every grant this side holds: undefined until the peer first answers. */
	readonly grant: Getter<Grant | undefined>;

	/**
	 * Present another token, resolving once the peer answers it. Rejects with
	 * {@link Unsupported} when the session or the peer takes no tokens in band, or with the
	 * peer's refusal as an `Error.Session`. On moq-transport without AUTH it resolves at
	 * once with no grant: only a request whose route names it carries it.
	 */
	add(token: string | Uint8Array): Promise<Auth>;

	/**
	 * Answer every token the peer presents, instead of the default: granting what this side
	 * publishes and consumes for the connection's own credential and refusing any other.
	 *
	 * Call it as soon as connect or accept resolves, in the same task: whoever answers is
	 * decided once, right after. Throws once that has happened.
	 *
	 * The peer's stream credit bounds how many tokens wait unanswered, since each holds its
	 * stream open until answered or withdrawn. A withdrawn token leaves the queue.
	 */
	requests(): Requests;
}

/** The {@link Handle} of a session without AUTH: no grant, and no tokens. */
export class None implements Handle {
	/** Always undefined: no token is ever granted. */
	readonly grant: Getter<Grant | undefined> = new Signal<Grant | undefined>(undefined);

	/** Always rejects with {@link Unsupported}. */
	add(_token: string | Uint8Array): Promise<Auth> {
		return Promise.reject(new Unsupported());
	}

	/** A stream that never yields a request. */
	requests(): Requests {
		return { next: async () => undefined, close: () => {} };
	}
}
