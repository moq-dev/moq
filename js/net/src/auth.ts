/**
 * In-band authorization: present tokens to the peer and learn what they grant.
 *
 * Each side of a moq-lite-07 session presents the credential its connection already carried
 * (the URL, or nothing) right after setup, and learns the {@link Grant} it earned. Older
 * versions and moq-transport carry no AUTH exchange: the grant stays undefined. There a
 * {@link Credential.answered} rejects with {@link Unsupported} on moq-lite, and on
 * moq-transport the token is kept, with no grant, for the announcements whose route names it.
 * {@link Handle.grant} is the union of every token this side presented, and {@link Handle.add}
 * presents another without reconnecting, returning its {@link Credential} at once.
 *
 * A {@link Handle} passed to `connect` as `auth`, or the one a reconnecting `Connection` owns,
 * outlives its sessions: each presents every token it holds, so a route naming one of its
 * {@link Credential}s keeps it across a reconnect. Only the peer's verdict ends such a token; a
 * session ending leaves it for the next. Mirrors the Rust `moq_net::auth`.
 *
 * @module
 */

import type { Wall } from "@moq/clock";
import type { Getter } from "@moq/signals";
import type { SessionCode } from "./error.ts";
import type * as Path from "./path.ts";

export { Duplicate, Handle, Unsupported } from "./auth_session.ts";

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
	/** When the grant lapses on the `@moq/clock` wall clock, or undefined for never. */
	expires?: Wall;
}

/**
 * A token this side presented, which a route can name, on every session of its handle.
 * {@link Credential.close} withdraws it.
 */
export interface Credential {
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
	/**
	 * Resolves once a peer first accepts the token. Rejects with {@link Unsupported} when a
	 * session neither carries AUTH nor lets a request carry a token (moq-lite without AUTH),
	 * with the peer's refusal as an `Error.Session`, with the handle's end, or once the token is
	 * withdrawn before an answer. On moq-transport without AUTH it resolves at once with no grant:
	 * only a request whose route names it carries it.
	 */
	readonly answered: PromiseLike<void>;
	/** Withdraw the token, removing its grant from the union and remaking any request that carries it without it. */
	close(): void;
}

/**
 * A token the peer presented, waiting for an answer. Answer every one.
 *
 * @internal Answering tokens is for direct peer sessions, which have no consumer yet.
 */
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

/**
 * A grant issued to one of the peer's tokens.
 *
 * @internal See {@link Request}.
 */
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

/**
 * The tokens the peer presents, for an application that answers them itself.
 *
 * @internal See {@link Request}.
 */
export interface Requests {
	/** The next token the peer presents, or undefined once the handle ends. */
	next(): Promise<Request | undefined>;
	/** Refuse every later token. */
	close(): void;
}
