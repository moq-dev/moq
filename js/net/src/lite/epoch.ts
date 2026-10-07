import * as Epoch from "../epoch.ts";
import type { Reader, Writer } from "../stream.ts";
import { hasEpoch, type Version } from "./version.ts";

/** Encode an optional epoch as its 16 bytes, or empty for none. Older versions carry
 * nothing, so the peer sees a route or request of unknown identity. Mirrors rs/moq-net. */
export async function encodeEpoch(w: Writer, version: Version, epoch: Epoch.Valid | undefined): Promise<void> {
	if (!hasEpoch(version)) return;
	const bytes = epoch ? Epoch.toBytes(epoch) : new Uint8Array();
	await w.u53(bytes.byteLength);
	await w.write(bytes);
}

/** Decode an optional epoch: empty is none, anything but a UUIDv7 is refused. */
export async function decodeEpoch(r: Reader, version: Version): Promise<Epoch.Valid | undefined> {
	if (!hasEpoch(version)) return undefined;
	const size = await r.u53();
	if (size === 0) return undefined;
	return Epoch.fromBytes(await r.read(size));
}
