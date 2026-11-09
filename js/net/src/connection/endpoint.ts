/**
 * Where a URL dials, parsed once so every decision about it reads the host the transport
 * connects to: GOAWAY redirect checks, the dial itself, and what logs show.
 *
 * Mirrors `rs/moq-net/src/reconnect/redirect.rs`.
 *
 * @module
 */

/** A host as a dial reads it: an IPv4 or IPv6 literal, or a name left to DNS. */
export type Host =
	| { readonly kind: "ipv4"; readonly octets: readonly number[] }
	| { readonly kind: "ipv6"; readonly segments: readonly number[] }
	| { readonly kind: "name"; readonly name: string };

/** Where a URL dials: its scheme, its host, and its port, which defaults by scheme. */
export interface Endpoint {
	/** The scheme without its colon, as written (`moql`, not the `https` a browser dials it over). */
	readonly scheme: string;
	readonly host: Host;
	readonly port: number;
	/**
	 * The URL a browser transport dials. A raw QUIC `moqt:` or `moql:` URL is reached over
	 * WebTransport with its scheme swapped for https (draft-ietf-moq-transport-18 §3.1.3); the
	 * CONNECT carries the scheme, so a server refuses the raw one.
	 */
	readonly url: URL;
}

/**
 * Parse where `url` dials. Throws a `TypeError` for a URL with no host, which no transport
 * here can dial.
 *
 * Only the special schemes (`http`, `https`, `ws`, `wss`) have their host parsed by the URL
 * parser; any other scheme keeps it opaque, so `moql://127.1` reads `127.1` while the dial,
 * made over https, connects to 127.0.0.1. Reading the host back through https makes both the
 * same: an address in any spelling the dial accepts is that address here.
 */
export function parse(url: URL): Endpoint {
	if (url.host === "") throw new TypeError(`${url.protocol} URL has no host`);
	const scheme = url.protocol.slice(0, -1);
	const special = SPECIAL.has(scheme);
	// Rebuilt rather than assigned, since the URL setter refuses a change from a non-special
	// scheme to a special one.
	const dial = special ? url : new URL(`https:${url.href.slice(url.protocol.length)}`);
	const port = url.port === "" ? defaultPort(scheme) : Number(url.port);
	return { scheme, host: parseHost(dial.hostname), port, url: dial };
}

const SPECIAL = new Set(["http", "https", "ws", "wss"]);

function defaultPort(scheme: string): number {
	return scheme === "http" || scheme === "ws" ? 80 : 443;
}

// The URL parser leaves a special scheme's host canonical: an IPv4 address as a dotted quad,
// an IPv6 one bracketed in lowercase hex without a zone or dotted tail, and a name lowercased.
function parseHost(hostname: string): Host {
	if (hostname.startsWith("[")) {
		const segments = parseIpv6(hostname.slice(1, -1));
		if (!segments) throw new TypeError(`unparsable IPv6 host: ${hostname}`);
		return { kind: "ipv6", segments };
	}
	const parts = hostname.split(".");
	if (parts.length === 4 && parts.every((part) => /^\d{1,3}$/.test(part))) {
		return { kind: "ipv4", octets: parts.map(Number) };
	}
	return { kind: "name", name: hostname };
}

function parseIpv6(text: string): number[] | undefined {
	const halves = text.split("::");
	const words = (part: string) => (part === "" ? [] : part.split(":"));
	const head = words(halves[0] ?? "");
	const rest = halves.length === 2 ? words(halves[1] ?? "") : [];
	const all = [...head, ...Array<string>(8 - head.length - rest.length).fill("0"), ...rest];
	const segments = all.map((word) => Number.parseInt(word, 16));
	return segments.length === 8 && !segments.some(Number.isNaN) ? segments : undefined;
}

/** Whether two endpoints dial the same host, whatever their scheme or port. */
export function sameHost(a: Endpoint, b: Endpoint): boolean {
	// The dial URL's host is canonical, so equal hosts spell it the same.
	return a.url.hostname === b.url.hostname;
}

/**
 * Formats an endpoint for logs and errors as scheme, host, and port only.
 *
 * A URL can carry a credential in its userinfo, query, or path (raw QUIC has no headers, so a
 * token can ride as a path segment). Dropping all three rather than redacting known-secret
 * spellings is deliberate: a denylist only covers the credentials that exist today.
 */
export function show(endpoint: Endpoint): string {
	return `${endpoint.scheme}://${endpoint.url.hostname}:${endpoint.port}`;
}

/**
 * Whether an endpoint says it names something only reachable from this host or network. A
 * judgement about the address it spells, not about where a dial lands: a name is dialed,
 * never resolved here, so `false` means "not local on its face", never "verified remote".
 */
export function isLocal({ host }: Endpoint): boolean {
	switch (host.kind) {
		case "ipv4":
			return isLocalV4(host.octets);
		case "ipv6":
			return isLocalV6(host.segments);
		case "name": {
			// A trailing dot names the same host from the root.
			const name = host.name.endsWith(".") ? host.name.slice(0, -1) : host.name;
			return name === "localhost" || name.endsWith(".localhost");
		}
	}
}

function isLocalV4([a, b, c, d]: readonly number[]): boolean {
	return (
		a === 127 ||
		a === 10 ||
		(a === 172 && b >= 16 && b <= 31) ||
		(a === 192 && b === 168) ||
		(a === 169 && b === 254) ||
		(a === 0 && b === 0 && c === 0 && d === 0)
	);
}

function isLocalV6(segments: readonly number[]): boolean {
	// An IPv4-mapped address reaches the same host as the v4 it wraps.
	if (segments.slice(0, 5).every((s) => s === 0) && segments[5] === 0xffff) {
		const [hi = 0, lo = 0] = segments.slice(6);
		return isLocalV4([hi >> 8, hi & 0xff, lo >> 8, lo & 0xff]);
	}
	const first = segments[0] ?? 0;
	const zeroPrefix = segments.slice(0, 7).every((s) => s === 0);
	const last = segments[7] ?? 0;
	return (
		// Loopback (::1) and unspecified (::).
		(zeroPrefix && (last === 1 || last === 0)) ||
		// Unique local (fc00::/7) and link local (fe80::/10).
		(first & 0xfe00) === 0xfc00 ||
		(first & 0xffc0) === 0xfe80
	);
}
