/**
 * GOAWAY handling for the reconnect loop: the peer's drain signal and the policy for the
 * redirect it may name. Mirrors `Goaway` and `Redirect` in `moq_tokio::connection`.
 *
 * @module
 */
import { RefusedRedirect } from "../error.ts";
import * as Time from "../time.ts";

/** A peer's GOAWAY as a session received it. */
export interface Drain {
	/** Where to reconnect, including any credentials it needs. Empty means the same endpoint. */
	readonly uri: string;
	/** When the peer force-closes the session. Undefined when the wire carried none. */
	readonly timeout?: Time.Milli;
}

/**
 * What to do with the URI a peer names in its GOAWAY.
 *
 * - `same-host` (the default) follows it only onto the host already dialed, so a peer can move
 *   the connection between ports or schemes but not to another host.
 * - `follow` also lets the peer name the host. The name is dialed as written, so only use it
 *   with a relay trusted not to point the connection into the local network.
 * - `ignore` reconnects to the current URL whatever the peer names.
 *
 * A malformed or refused URI ends the connection with {@link RefusedRedirect}.
 */
export type Redirect = "follow" | "same-host" | "ignore";

/** How a reconnecting connection reacts to a peer's GOAWAY. */
export interface GoawayProps {
	/** What to do with the URI the peer names (default: `same-host`). */
	redirect?: Redirect;

	/**
	 * How long the old session keeps serving after the GOAWAY while the replacement dials
	 * (default: 10000ms). A cap: a shorter deadline from the peer wins, a longer one does not
	 * extend it.
	 */
	handover?: Time.Milli;
}

/** The handover cap when the caller names none. */
export const DEFAULT_HANDOVER = Time.Milli(10_000);

/**
 * How long a drained session may keep serving: the cap, lowered to the peer's deadline when
 * it named a positive one. Absence is not a zero-length handover.
 *
 * @internal
 */
export function handover(cap: Time.Milli, timeout?: Time.Milli): Time.Milli {
	return timeout !== undefined && timeout > 0 ? Time.Milli(Math.min(cap, timeout)) : cap;
}

/**
 * The URL a redirect is judged against. A WebSocket fallback that won the race is the host
 * we dialed; the primary URL was not, so same-host must not treat it as the current peer.
 *
 * @internal
 */
export function dialed(primary: URL, transport: "webtransport" | "websocket", websocket?: URL): URL {
	return transport === "websocket" && websocket ? websocket : primary;
}

/**
 * The URL a GOAWAY assigns: `undefined` keeps the current URL (the peer named none, or the
 * policy ignores it), and a URL replaces it. Throws {@link RefusedRedirect} for an explicit
 * URI the policy will not follow.
 *
 * `pinned` is a certificate pin on the connection, which can only verify the host it was
 * configured for, so it refuses a host change even under `follow`.
 *
 * @internal
 */
export function target(policy: Redirect, uri: string, current: URL, pinned: boolean): URL | undefined {
	if (uri === "" || policy === "ignore") return undefined;

	// The URI can carry credentials, so the error names the reason, never the URI.
	let next: URL;
	try {
		next = new URL(uri);
	} catch {
		throw new RefusedRedirect("the GOAWAY URI is malformed");
	}

	if (schemeTier(next.protocol) < schemeTier(current.protocol)) {
		throw new RefusedRedirect("the GOAWAY redirect downgrades the scheme");
	}

	// Only as far as the URL itself says: a name is dialed, never resolved here, so this
	// refuses a peer naming a local address outright, not one hiding it behind a hostname.
	// That gap is why `same-host` is the default.
	if (isLocal(next) && !isLocal(current)) {
		throw new RefusedRedirect("the GOAWAY redirect widens reachability to a local address");
	}

	// Host only, not the full authority: the port is what a peer legitimately moves us
	// across when it hands off to a sibling process on the same box.
	const sameHost = next.hostname === current.hostname;
	if (policy === "same-host" && !sameHost) {
		throw new RefusedRedirect("the GOAWAY redirect leaves the current host");
	}
	if (pinned && !sameHost) {
		throw new RefusedRedirect("the GOAWAY redirect leaves the host a certificate pin verifies");
	}

	return next;
}

/** Rank a scheme so a redirect cannot silently drop encryption; unknown schemes rank lowest. */
function schemeTier(protocol: string): number {
	switch (protocol) {
		case "https:":
		case "wss:":
		case "moqt:":
		case "moql:":
			return 2;
		case "http:":
		case "ws:":
		case "tcp:":
			return 1;
		default:
			return 0;
	}
}

/**
 * Whether a URL says it names something only reachable from this host or network. A judgement
 * about the URL, not about where a dial lands: `false` means "not local on its face".
 *
 * @internal
 */
export function isLocal(url: URL): boolean {
	const host = url.hostname.toLowerCase();
	// No host at all, e.g. a `unix:` socket path.
	if (host === "") return true;
	if (host === "localhost" || host.endsWith(".localhost")) return true;

	if (host.startsWith("[") && host.endsWith("]")) {
		const segments = parseIpv6(host.slice(1, -1));
		return segments !== undefined && isLocalV6(segments);
	}

	const v4 = parseIpv4(host);
	return v4 !== undefined && isLocalV4(v4);
}

function parseIpv4(host: string): number[] | undefined {
	const parts = host.split(".");
	if (parts.length !== 4) return undefined;
	const octets = parts.map((part) => (/^\d{1,3}$/.test(part) ? Number(part) : Number.NaN));
	return octets.every((octet) => octet <= 255) ? octets : undefined;
}

function isLocalV4([a, b, c, d]: number[]): boolean {
	return (
		a === 127 ||
		a === 10 ||
		(a === 172 && b >= 16 && b <= 31) ||
		(a === 192 && b === 168) ||
		(a === 169 && b === 254) ||
		(a === 0 && b === 0 && c === 0 && d === 0)
	);
}

/** Parse an IPv6 literal (without brackets) into its eight 16-bit segments. */
function parseIpv6(host: string): number[] | undefined {
	// Drop a zone id; it scopes the address without changing it.
	let text = host.split("%")[0] ?? "";

	// A trailing dotted quad (`::ffff:127.0.0.1`) spells the last two segments.
	const lastColon = text.lastIndexOf(":");
	const quad = parseIpv4(text.slice(lastColon + 1));
	if (quad) {
		const [a = 0, b = 0, c = 0, d = 0] = quad;
		const word = (hi: number, lo: number) => ((hi << 8) | lo).toString(16);
		text = `${text.slice(0, lastColon + 1)}${word(a, b)}:${word(c, d)}`;
	}

	const halves = text.split("::");
	if (halves.length > 2) return undefined;
	const words = (part: string) => (part === "" ? [] : part.split(":"));
	const head = words(halves[0] ?? "");
	const rest = halves.length === 2 ? words(halves[1] ?? "") : [];
	const missing = 8 - head.length - rest.length;
	if (halves.length === 2 ? missing < 0 : missing !== 0) return undefined;

	const all = [...head, ...Array<string>(missing).fill("0"), ...rest];
	const segments = all.map((word) => (/^[0-9a-f]{1,4}$/i.test(word) ? Number.parseInt(word, 16) : Number.NaN));
	return segments.some(Number.isNaN) ? undefined : segments;
}

function isLocalV6(segments: number[]): boolean {
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
