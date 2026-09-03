import * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";

/** Convert the escaped moq-net path representation into IETF namespace tuple fields. */
export function toTuple(namespace: Path.Valid): string[] {
	if (namespace === "") return [];

	const parts = [""];
	for (let i = 0; i < namespace.length; i++) {
		const ch = namespace[i];
		if (ch === "/") {
			parts.push("");
		} else if (ch === "\\" && (namespace[i + 1] === "/" || namespace[i + 1] === "\\")) {
			parts[parts.length - 1] += namespace[++i];
		} else {
			parts[parts.length - 1] += ch;
		}
	}
	return parts;
}

/** Convert IETF namespace tuple fields into the escaped moq-net path representation. */
export function fromTuple(parts: string[]): Path.Valid {
	return parts.map((part) => part.replaceAll("\\", "\\\\").replaceAll("/", "\\/")).join("/") as Path.Valid;
}

/** Encode a moq-net namespace as an IETF namespace tuple. */
export async function encode(w: Writer, namespace: Path.Valid): Promise<void> {
	const parts = toTuple(namespace);

	// The IETF draft limits namespaces to 32 parts.
	if (parts.length > Path.MAX_PARTS) {
		throw new Error(`namespace exceeds ${Path.MAX_PARTS} parts`);
	}

	await w.u53(parts.length);
	for (const part of parts) {
		await w.string(part);
	}
}

/** Decode an IETF namespace tuple into an escaped moq-net namespace. */
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
	return fromTuple(parts);
}
