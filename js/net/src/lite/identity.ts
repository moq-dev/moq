import { ProtocolViolation } from "../error.ts";
import * as Identity from "../identity.ts";
import * as Mint from "../mint.ts";
import type { Reader, Writer } from "../stream.ts";
import { decodeUtf8 } from "../util/utf8.ts";
import { hasIdentity, hasRoutes, type Version } from "./version.ts";

const encoder = new TextEncoder();

/** Encode the identity a request pins as its UTF-8 bytes and minted flag, or empty and unminted
 * for none. Older versions carry nothing, so the peer resolves whichever identity wins the path.
 * Mirrors rs/moq-net. */
export async function encodePin(w: Writer, version: Version, identity: Identity.Valid | undefined): Promise<void> {
	if (!hasIdentity(version)) return;
	if (identity !== undefined) return encode(w, identity);
	await w.u53(0);
	await w.bool(false);
}

/** Decode the identity a request pins: empty is none, and anything but UTF-8 of at most 256
 * bytes, or an empty one flagged minted, is refused. */
export async function decodePin(r: Reader, version: Version): Promise<Identity.Valid | undefined> {
	if (!hasIdentity(version)) return undefined;
	const bytes = await r.read(await r.u53());
	const minted = await r.bool();
	if (bytes.byteLength === 0 && !minted) return undefined;
	return decode(bytes, minted);
}

/** Encode the forwarding node a request names. An endpoint knows none, so it always sends Node
 * ID 0, which lets the publisher select one. Only lite-07 carries it. Mirrors rs/moq-net. */
export async function encodeNode(w: Writer, version: Version): Promise<void> {
	if (hasRoutes(version)) await w.u62(0n);
}

/** Decode the forwarding node a request names: a Node ID, and its Marked unless the ID is 0. An
 * endpoint forwards nothing, so whichever node the request names, it serves the request itself. */
export async function decodeNode(r: Reader, version: Version): Promise<void> {
	if (!hasRoutes(version)) return;
	if ((await r.u62()) === 0n) return;
	await r.bool();
}

/** Encode the identity, its minted flag, and the epoch an announcement serves. Every route
 * carries an identity, so lite-07 refuses to announce one without; older versions carry neither. */
export async function encodeRoute(
	w: Writer,
	version: Version,
	identity: Identity.Valid | undefined,
	epoch: bigint | undefined,
): Promise<void> {
	if (!hasIdentity(version)) return;
	if (identity === undefined) throw new Error("a lite-07 route needs an identity");
	await encode(w, identity);
	await w.u62(epoch ?? 0n);
}

/** Decode the identity and epoch an announcement serves; see {@link encodeRoute}. */
export async function decodeRoute(r: Reader, version: Version): Promise<{ identity?: Identity.Valid; epoch?: bigint }> {
	if (!hasIdentity(version)) return {};
	const bytes = await r.read(await r.u53());
	const identity = decode(bytes, await r.bool());
	return { identity, epoch: await r.u62() };
}

async function encode(w: Writer, identity: Identity.Valid): Promise<void> {
	const bytes = encoder.encode(Identity.text(identity));
	await w.u53(bytes.byteLength);
	await w.write(bytes);
	await w.bool(Identity.isMinted(identity));
}

// Anything but UTF-8 of 1 to 256 bytes is a PROTOCOL_VIOLATION, in every message that carries one.
function decode(bytes: Uint8Array, minted: boolean): Identity.Valid {
	let named: Identity.Named;
	try {
		named = Identity.parse(decodeUtf8(bytes));
	} catch (cause) {
		throw new ProtocolViolation(cause instanceof Error ? cause.message : String(cause));
	}
	return minted ? Mint.mint(named) : named;
}
