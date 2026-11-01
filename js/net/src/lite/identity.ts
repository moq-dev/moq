import * as Identity from "../identity.ts";
import type { Reader, Writer } from "../stream.ts";
import { hasIdentity, type Version } from "./version.ts";

const encoder = new TextEncoder();
// `ignoreBOM` keeps a leading U+FEFF, which is identity text like any other, as Rust does.
const decoder = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true });

/** Encode the identity a request pins as its UTF-8 bytes, or empty for none. Older versions
 * carry nothing, so the peer resolves whichever identity wins the path. Mirrors rs/moq-net. */
export async function encodePin(w: Writer, version: Version, identity: Identity.Valid | undefined): Promise<void> {
	if (!hasIdentity(version)) return;
	const bytes = identity === undefined ? new Uint8Array() : encoder.encode(identity);
	await w.u53(bytes.byteLength);
	await w.write(bytes);
}

/** Decode the identity a request pins: empty is none, and anything but UTF-8 is refused. */
export async function decodePin(r: Reader, version: Version): Promise<Identity.Valid | undefined> {
	if (!hasIdentity(version)) return undefined;
	const size = await r.u53();
	if (size === 0) return undefined;
	return Identity.parse(decoder.decode(await r.read(size)));
}

/** Encode the identity and epoch an announcement serves. Every route carries an identity, so
 * lite-07 refuses to announce one without; older versions carry neither. */
export async function encodeRoute(
	w: Writer,
	version: Version,
	identity: Identity.Valid | undefined,
	epoch: bigint | undefined,
): Promise<void> {
	if (!hasIdentity(version)) return;
	if (identity === undefined) throw new Error("a lite-07 route needs an identity");
	const bytes = encoder.encode(identity);
	await w.u53(bytes.byteLength);
	await w.write(bytes);
	await w.u62(epoch ?? 0n);
}

/** Decode the identity and epoch an announcement serves; see {@link encodeRoute}. */
export async function decodeRoute(r: Reader, version: Version): Promise<{ identity?: Identity.Valid; epoch?: bigint }> {
	if (!hasIdentity(version)) return {};
	const size = await r.u53();
	const identity = Identity.parse(decoder.decode(await r.read(size)));
	return { identity, epoch: await r.u62() };
}
