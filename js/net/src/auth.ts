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
 * presents another without reconnecting, returning its {@link Auth}.
 *
 * A {@link Handle} passed to `connect` as `auth`, or the one a reconnecting `Connection` owns,
 * outlives its sessions: each presents every token it holds, so a route naming one of its
 * {@link Auth}s keeps it across a reconnect. Only the peer's verdict ends such a token; a
 * session ending leaves it for the next. Mirrors the Rust `moq_net::auth`.
 *
 * @module
 */

import type { Getter } from "@moq/signals";
import type { SessionCode } from "./error.ts";
import type * as Path from "./path.ts";

export { Duplicate, grantsEqual, Handle, Unsupported } from "./auth_session.ts";

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

/**
 * A token this side presented, which a route can name, on every session of its handle.
 * {@link Auth.close} withdraws it.
 */
export interface Auth {
	/**
	 * This token's own grant on the live session: undefined until its peer answers, while no
	 * session is live, and again once it ends.
	 */
	readonly grant: Getter<Grant | undefined>;
	/**
	 * Settles when the token ends for good: the peer's refusal or revocation, `null` when the
	 * peer closed it without one or it was withdrawn, or the handle's end. A session ending is
	 * not one: the next session presents it again.
	 */
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
	/** The next token the peer presents, or undefined once the handle ends. */
	next(): Promise<Request | undefined>;
	/** Refuse every later token. */
	close(): void;
}
