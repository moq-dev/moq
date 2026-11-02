import * as Instance from "../instance.ts";
import type { Reader, Writer } from "../stream.ts";
import { hasInstance, type Version } from "./version.ts";

const encoder = new TextEncoder();
// `ignoreBOM` keeps a leading U+FEFF, which is instance text like any other, as Rust does.
const decoder = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true });

/** Encode the instance a request pins as its UTF-8 bytes, or empty for none. Older versions
 * carry nothing, so the peer resolves whichever instance wins the path. Mirrors rs/moq-net. */
export async function encodePin(w: Writer, version: Version, instance: Instance.Valid | undefined): Promise<void> {
	if (!hasInstance(version)) return;
	const bytes = instance === undefined ? new Uint8Array() : encoder.encode(instance);
	await w.u53(bytes.byteLength);
	await w.write(bytes);
}

/** Decode the instance a request pins: empty is none, and anything but UTF-8 is refused. */
export async function decodePin(r: Reader, version: Version): Promise<Instance.Valid | undefined> {
	if (!hasInstance(version)) return undefined;
	const size = await r.u53();
	if (size === 0) return undefined;
	return Instance.parse(decoder.decode(await r.read(size)));
}

/** Encode the instance and epoch an announcement serves. Every route carries an instance, so
 * lite-07 refuses to announce one without; older versions carry neither. */
export async function encodeRoute(
	w: Writer,
	version: Version,
	instance: Instance.Valid | undefined,
	epoch: bigint | undefined,
): Promise<void> {
	if (!hasInstance(version)) return;
	if (instance === undefined) throw new Error("a lite-07 route needs an instance");
	const bytes = encoder.encode(instance);
	await w.u53(bytes.byteLength);
	await w.write(bytes);
	await w.u62(epoch ?? 0n);
}

/** Decode the instance and epoch an announcement serves; see {@link encodeRoute}. */
export async function decodeRoute(r: Reader, version: Version): Promise<{ instance?: Instance.Valid; epoch?: bigint }> {
	if (!hasInstance(version)) return {};
	const size = await r.u53();
	const instance = Instance.parse(decoder.decode(await r.read(size)));
	return { instance, epoch: await r.u62() };
}
