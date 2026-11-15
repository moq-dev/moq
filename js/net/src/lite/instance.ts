import { ProtocolViolation } from "../error.ts";
import * as Instance from "../instance.ts";
import * as Mint from "../mint.ts";
import * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";
import { hasInstance, hasRoutes, type Version } from "./version.ts";

const encoder = new TextEncoder();
// `ignoreBOM` keeps a leading U+FEFF, which is instance text like any other, as Rust does.
const decoder = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true });

/** Encode the instance a request pins as its UTF-8 bytes and minted flag, or empty and unminted
 * for none. Older versions carry nothing, so the peer resolves whichever instance wins the path.
 * Mirrors rs/moq-net. */
export async function encodePin(w: Writer, version: Version, instance: Instance.Valid | undefined): Promise<void> {
	if (!hasInstance(version)) return;
	if (instance !== undefined) return encode(w, instance);
	await w.u53(0);
	await w.bool(false);
}

/** Decode the instance a request pins: empty is none, and anything but UTF-8, or an empty one
 * flagged minted, is refused. */
export async function decodePin(r: Reader, version: Version): Promise<Instance.Valid | undefined> {
	if (!hasInstance(version)) return undefined;
	const bytes = await r.read(await r.u53());
	const minted = await r.bool();
	if (bytes.byteLength === 0 && !minted) return undefined;
	return decode(bytes, minted);
}

/** Encode the publisher node a request names, 0 letting the publisher select one, then how many
 * leading segments of the request's path make up the prefix of the route it follows, 0 with no
 * node. Only lite-07 carries them, so an older peer always selects, and a node set here is
 * dropped. Mirrors rs/moq-net. */
export async function encodeNode(w: Writer, version: Version, node: bigint, depth = 0): Promise<void> {
	if (!hasRoutes(version)) return;
	// A route needs its publisher.
	if (node === 0n && depth !== 0) throw new Error("a route depth without its node");
	await w.u62(node);
	await w.u53(depth);
}

/** Decode the publisher node a request for `broadcast` names, and its route's depth; see
 * {@link encodeNode}. A depth past the path's segments names no prefix of it. */
export async function decodeNode(
	r: Reader,
	version: Version,
	broadcast: Path.Valid,
): Promise<{ node: bigint; depth: number }> {
	if (!hasRoutes(version)) return { node: 0n, depth: 0 };
	const node = await r.u62();
	const depth = await r.u53();
	if ((node === 0n && depth !== 0) || depth > Path.parts(broadcast).length)
		throw new ProtocolViolation(`route depth ${depth} names no prefix of ${broadcast}`);
	return { node, depth };
}

/** Encode the instance, its minted flag, and the epoch an announcement serves. Every route
 * carries an instance, so lite-07 refuses to announce one without; older versions carry neither. */
export async function encodeRoute(
	w: Writer,
	version: Version,
	instance: Instance.Valid | undefined,
	epoch: bigint | undefined,
): Promise<void> {
	if (!hasInstance(version)) return;
	if (instance === undefined) throw new Error("a lite-07 route needs an instance");
	await encode(w, instance);
	await w.u62(epoch ?? 0n);
}

/** Decode the instance and epoch an announcement serves; see {@link encodeRoute}. */
export async function decodeRoute(r: Reader, version: Version): Promise<{ instance?: Instance.Valid; epoch?: bigint }> {
	if (!hasInstance(version)) return {};
	const bytes = await r.read(await r.u53());
	const instance = decode(bytes, await r.bool());
	return { instance, epoch: await r.u62() };
}

async function encode(w: Writer, instance: Instance.Valid): Promise<void> {
	const bytes = encoder.encode(Instance.text(instance));
	await w.u53(bytes.byteLength);
	await w.write(bytes);
	await w.bool(Instance.isMinted(instance));
}

/** Decode lite-07's instance and epoch, with `instance` undefined when they are well framed but
 * the instance is not one this node can use, which withdraws only the claim naming it. Mirrors
 * rs/moq-net. */
export async function decodeForwarded(r: Reader): Promise<{ instance?: Instance.Valid; epoch: bigint }> {
	const bytes = await r.read(await r.u53());
	const minted = await r.bool();
	const epoch = await r.u62();
	const text = utf8(bytes);
	if (text === undefined || text.length === 0 || !text.isWellFormed()) return { epoch };
	const named = Instance.parse(text);
	return { instance: minted ? Mint.mint(named) : named, epoch };
}

/** `bytes` as UTF-8 text, or undefined when they are not UTF-8. */
export function utf8(bytes: Uint8Array): string | undefined {
	try {
		return decoder.decode(bytes);
	} catch {
		return undefined;
	}
}

function decode(bytes: Uint8Array, minted: boolean): Instance.Valid {
	const named = Instance.parse(decoder.decode(bytes));
	return minted ? Mint.mint(named) : named;
}
