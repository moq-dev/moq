import * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";

export async function encode(w: Writer, namespace: Path.Valid): Promise<void> {
	const parts = Path.parts(namespace);

	// The IETF draft limits namespaces to 32 parts.
	if (parts.length > Path.MAX_PARTS) {
		throw new Error(`namespace exceeds ${Path.MAX_PARTS} parts`);
	}

	await w.u53(parts.length);
	for (const part of parts) {
		await w.string(part);
	}
}

export async function decode(r: Reader): Promise<Path.Valid> {
	const count = await r.u53();

	// The IETF draft limits namespaces to 32 parts. Reject before reading them so a
	// hostile count can't make us buffer unbounded parts.
	if (count > Path.MAX_PARTS) {
		throw new Error(`namespace exceeds ${Path.MAX_PARTS} parts`);
	}

	const parts: string[] = [];
	for (let i = 0; i < count; i++) {
		parts.push(await r.string());
	}
	return Path.from(...parts);
}
