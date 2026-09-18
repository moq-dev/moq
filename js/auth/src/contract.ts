/**
 * The JSON a relay POSTs to an auth server per session event, and the grant it answers with.
 *
 * Mirrors `moq_auth::Request` and `moq_auth::Grant` in the Rust crate: a server written
 * in TypeScript validates a request and builds a grant with these schemas.
 *
 * @module
 */

import * as z from "@zod/mini";
import { PatternListSchema } from "./claims.ts";

/** How a session reached the relay; `http` is a one-shot request on the relay's web listener. */
export const TransportSchema = z.enum(["quic", "iroh", "websocket", "tcp", "unix", "http"]);
export type Transport = z.infer<typeof TransportSchema>;

/** The single direction a client declared at SETUP. */
export const RoleSchema = z.enum(["publisher", "subscriber"]);
export type Role = z.infer<typeof RoleSchema>;

/**
 * The verified client certificate a session presented, as facts for the server to
 * decide on. Presenting one admits nothing by itself.
 */
export const PeerSchema = z.object({
	/** The first SAN DNS name, else the CN, else the fingerprint, so it is never empty.
	 * Those three sources fold into one string a server cannot tell apart; match on
	 * `fingerprint` when identity must be exact. */
	name: z.string(),
	/** SHA-256 of the leaf certificate, hex. */
	fingerprint: z.string(),
	/** The certificate's notAfter, as unix seconds. */
	expires: z.optional(z.int()),
	/** The issuer's distinguished name. */
	issuer: z.string(),
});
export type Peer = z.infer<typeof PeerSchema>;

/** Byte totals for a session, both directions from the relay's point of view. */
export const BytesSchema = z.object({
	/** Bytes the relay sent to the peer. */
	sent: z.int().check(z.nonnegative()),
	/** Bytes the relay received from the peer. */
	received: z.int().check(z.nonnegative()),
});
export type Bytes = z.infer<typeof BytesSchema>;

const BaseRequestSchema = z.object({
	/** Random 128-bit hex, unique per session; the key every event for it shares. */
	id: z.string(),
	/** The operator's name for the relay asking. */
	node: z.string(),
	/** How the session reached the relay. */
	transport: TransportSchema,
	/** The peer's socket address, absent on a transport without one (a unix socket). */
	remote: z.optional(z.string()),
	/** The relay's socket address the session arrived on, absent likewise. */
	local: z.optional(z.string()),
	/** The SNI the client presented, when the transport carried TLS. */
	server_name: z.optional(z.string()),
	/** The negotiated application protocol, including the moq version. */
	alpn: z.optional(z.string()),
	/** The path exactly as dialed. */
	path: z.string(),
	/** The raw query string, without the leading `?`. */
	query: z.optional(z.string()),
	/** The direction the client declared at SETUP; absent means both. */
	role: z.optional(RoleSchema),
	/** The verified client certificate, when one was presented. */
	tls: z.optional(PeerSchema),
});

/**
 * Everything a relay knows about a session, sent to the auth server on every event.
 *
 * Nothing is parsed on the relay's behalf: the server keys policy on the raw `path`
 * and `query`, so no query parameter is special. The same shape carries every event;
 * an `end` adds what the session did.
 */
export const RequestSchema = z.discriminatedUnion("event", [
	z.extend(BaseRequestSchema, {
		/** A session was accepted and asks to be admitted. */
		event: z.literal("connect"),
	}),
	z.extend(BaseRequestSchema, {
		/** The grant asked to be re-checked on its cadence. */
		event: z.literal("revalidate"),
	}),
	z.extend(BaseRequestSchema, {
		/** The session closed. */
		event: z.literal("end"),
		/** Why it closed: `dropped`, `expired`, `refused`, or the session's own classification. */
		reason: z.string(),
		/** How long it was admitted, in seconds. */
		duration: z.number(),
		/** What it moved. */
		bytes: BytesSchema,
	}),
]);
export type Request = z.infer<typeof RequestSchema>;

/**
 * What a session may do, as the auth server answered.
 *
 * A 2xx carrying one of these admits; anything else refuses. A grant that names
 * nothing is a refusal too, and one that asks to be revalidated must say when it
 * expires, so an outage always has a bound the server chose.
 */
export const GrantSchema = z
	.object({
		/** Patterns the session may publish, relative to the root. */
		publish: z.optional(PatternListSchema),
		/** Patterns the session may subscribe to, relative to the root. */
		subscribe: z.optional(PatternListSchema),
		/** The path the patterns are relative to, replacing the dialed one. Absent means the dialed path. */
		root: z.optional(z.string()),
		/** When the session closes, as whole unix seconds. */
		expires: z.optional(z.int()),
		/** How long until the relay asks again, in whole seconds. Zero would be a tight loop. */
		revalidate: z.optional(z.int().check(z.positive())),
		/** An opaque label handed to stats, so traffic can be bucketed. */
		tier: z.optional(z.string()),
	})
	.check(
		z.refine((grant) => (grant.publish?.length ?? 0) > 0 || (grant.subscribe?.length ?? 0) > 0, {
			message: "a grant must name something",
		}),
		z.refine((grant) => grant.revalidate === undefined || grant.expires !== undefined, {
			message: "a grant that asks to be revalidated must expire",
		}),
	);
export type Grant = z.infer<typeof GrantSchema>;
