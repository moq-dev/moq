import { ProtocolViolation } from "../error.ts";
import * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";
import { decodeUtf8 } from "../util/utf8.ts";
import { type IetfVersion, Version } from "./version.ts";

/**
 * Convert the escaped moq-net path representation into IETF namespace tuple fields.
 *
 * Every `\` in a valid path escapes the `\` or `/` after it.
 */
function toTuple(namespace: Path.Valid): string[] {
	if (namespace === "") return [];

	const parts = [""];
	for (let i = 0; i < namespace.length; i++) {
		const ch = namespace[i];
		if (ch === "/") {
			parts.push("");
		} else if (ch === "\\") {
			parts[parts.length - 1] += namespace[++i];
		} else {
			parts[parts.length - 1] += ch;
		}
	}
	return parts;
}

/**
 * Convert IETF namespace tuple fields into the escaped moq-net path representation.
 *
 * Every `\` and `/` in a field is escaped, the only spelling a path accepts, so every path
 * returns from its own tuple unchanged. Throws when a field ends in `/`, which escapes to a
 * path with an empty segment, or when the separators and escapes push the path past
 * {@link Path.MAX_BYTES}.
 */
export function fromTuple(parts: string[]): Path.Valid {
	const escaped = parts.map((part) => part.replaceAll("\\", "\\\\").replaceAll("/", "\\/")).join("/");
	if (tooLong(escaped)) throw new ProtocolViolation(`namespace exceeds ${Path.MAX_BYTES} bytes as a path`);
	try {
		return Path.from(escaped);
	} catch (cause) {
		throw new ProtocolViolation("namespace field escapes to an empty path segment", { cause });
	}
}

/** Encode a moq-net namespace as an IETF namespace tuple. */
export async function encode(w: Writer, namespace: Path.Valid): Promise<void> {
	await writeFields(w, fields(namespace));
}

/** Encode a full track name: a namespace tuple then a track name, bounded together. */
export async function encodeTrack(w: Writer, namespace: Path.Valid, name: string): Promise<void> {
	const parts = fields(namespace);
	let size = utf8.encode(name).length;
	for (const part of parts) size += utf8.encode(part).length;
	if (size > MAX_BYTES) throw new Error(`full track name exceeds ${MAX_BYTES} bytes`);
	await writeFields(w, parts);
	await w.string(name);
}

function fields(namespace: Path.Valid): string[] {
	// The fields drop the path's separators and escapes, so a path within Path.MAX_BYTES fits.
	if (tooLong(namespace)) throw new Error(`namespace exceeds ${Path.MAX_BYTES} bytes`);
	const parts = toTuple(namespace);
	if (parts.length > Path.MAX_PARTS) throw new Error(`namespace exceeds ${Path.MAX_PARTS} parts`);
	return parts;
}

async function writeFields(w: Writer, parts: string[]): Promise<void> {
	await w.u53(parts.length);
	for (const part of parts) {
		await w.string(part);
	}
}

// A UTF-16 unit is at most 3 UTF-8 bytes, so only a long path needs encoding to measure.
function tooLong(path: string): boolean {
	return path.length > Path.MAX_BYTES / 3 && utf8.encode(path).length > Path.MAX_BYTES;
}

// Every draft caps a namespace, and a namespace plus track name, at 4096 bytes.
const MAX_BYTES = 4096;
const utf8 = new TextEncoder();

/** Decode an IETF namespace tuple into an escaped moq-net namespace. */
export async function decode(r: Reader): Promise<Path.Valid> {
	return (await decodeFields(r)).namespace;
}

/** Decode a full track name: a namespace tuple then a track name, bounded together. */
export async function decodeTrack(r: Reader): Promise<{ namespace: Path.Valid; name: string }> {
	const { namespace, size } = await decodeFields(r);
	const length = await r.u53();
	if (size + length > MAX_BYTES) throw new ProtocolViolation(`full track name exceeds ${MAX_BYTES} bytes`);
	return { namespace, name: decodeUtf8(await r.read(length)) };
}

async function decodeFields(r: Reader): Promise<{ namespace: Path.Valid; size: number }> {
	const count = await r.u53();

	// The IETF draft limits namespaces to 32 parts. Reject before reading them so a
	// hostile count can't make us buffer unbounded parts.
	if (count > Path.MAX_PARTS) {
		throw new ProtocolViolation(`namespace exceeds ${Path.MAX_PARTS} parts`);
	}

	const parts: string[] = [];
	let size = 0;
	for (let i = 0; i < count; i++) {
		const length = await r.u53();
		if (length === 0) throw new ProtocolViolation("empty namespace field");
		size += length;
		if (size > MAX_BYTES) throw new ProtocolViolation(`namespace exceeds ${MAX_BYTES} bytes`);
		parts.push(decodeUtf8(await r.read(length)));
	}
	return { namespace: fromTuple(parts), size };
}

/** Whether a request names an unimplemented session namespace or the forbidden period field. */
export function reserved(namespace: Path.Valid, version: IetfVersion): boolean {
	if (version < Version.DRAFT_22) return false;
	const first = toTuple(namespace)[0];
	return first === "." || first === ".session";
}
