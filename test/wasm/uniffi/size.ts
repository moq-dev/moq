/**
 * Attribute a `.wasm` file's bytes to sections, and its code to crates by function name.
 *
 *     bun uniffi/size.ts <module.wasm>...
 *
 * A generic function counts toward the crate its demangled path starts with, so
 * `<core::option::Option<moq_mux::Clock>>::unwrap_or_default` is `core`. That
 * undercounts crates whose code is mostly monomorphized elsewhere; read it as a
 * coarse share, which is all the report claims.
 */
import { gzipSync } from "bun";

function leb(bytes: Uint8Array, at: { pos: number }): number {
	let result = 0;
	let shift = 0;
	for (;;) {
		const byte = bytes[at.pos++];
		result += (byte & 0x7f) * 2 ** shift;
		if (!(byte & 0x80)) return result;
		shift += 7;
	}
}

function name(bytes: Uint8Array, at: { pos: number }): string {
	const length = leb(bytes, at);
	const text = new TextDecoder().decode(bytes.subarray(at.pos, at.pos + length));
	at.pos += length;
	return text;
}

const SECTIONS = [
	"custom",
	"type",
	"import",
	"function",
	"table",
	"memory",
	"global",
	"export",
	"start",
	"element",
	"code",
	"data",
	"datacount",
];

/** The crate a demangled function path belongs to, or a bucket for what has none. */
function crate(fn: string): string {
	const path = fn.replace(/^<+/, "");
	const match = /^([A-Za-z_][A-Za-z0-9_]*)(\[[0-9a-f]+\])?::/.exec(path);
	if (match) return match[1];
	if (fn.startsWith("__wbg") || fn.startsWith("__wbindgen")) return "(wasm-bindgen)";
	if (fn.startsWith("uniffi_") || fn.startsWith("ffi_")) return "(uniffi exports)";
	return "(other)";
}

function analyze(file: string, bytes: Uint8Array<ArrayBuffer>) {
	const at = { pos: 8 };
	const sections: Record<string, number> = {};
	const bodies: number[] = [];
	let imported = 0;
	const names = new Map<number, string>();
	// Every section but the custom ones, which a shipped module can drop.
	const kept: Uint8Array[] = [bytes.subarray(0, 8)];
	while (at.pos < bytes.length) {
		const start = at.pos;
		const id = bytes[at.pos++];
		const size = leb(bytes, at);
		const end = at.pos + size;
		let label = SECTIONS[id] ?? `unknown${id}`;
		if (id === 0) {
			const inner = { pos: at.pos };
			label = `custom:${name(bytes, inner)}`;
			if (label === "custom:name") {
				while (inner.pos < end) {
					const kind = bytes[inner.pos++];
					const length = leb(bytes, inner);
					const stop = inner.pos + length;
					if (kind === 1) {
						for (let count = leb(bytes, inner); count > 0; count--) {
							const index = leb(bytes, inner);
							names.set(index, name(bytes, inner));
						}
					}
					inner.pos = stop;
				}
			}
		} else if (id === 2) {
			const inner = { pos: at.pos };
			for (let count = leb(bytes, inner); count > 0; count--) {
				name(bytes, inner);
				name(bytes, inner);
				const kind = bytes[inner.pos++];
				if (kind === 0) {
					imported++;
					leb(bytes, inner);
				} else if (kind === 1) {
					inner.pos++;
					const flags = leb(bytes, inner);
					leb(bytes, inner);
					if (flags & 1) leb(bytes, inner);
				} else if (kind === 2) {
					const flags = leb(bytes, inner);
					leb(bytes, inner);
					if (flags & 1) leb(bytes, inner);
				} else if (kind === 3) {
					inner.pos += 2;
				}
			}
		} else if (id === 10) {
			const inner = { pos: at.pos };
			for (let count = leb(bytes, inner); count > 0; count--) {
				const length = leb(bytes, inner);
				bodies.push(length);
				inner.pos += length;
			}
		}
		sections[label] = (sections[label] ?? 0) + size;
		if (id !== 0) kept.push(bytes.subarray(start, end));
		at.pos = end;
	}

	const crates = new Map<string, number>();
	bodies.forEach((length, index) => {
		const owner = crate(names.get(imported + index) ?? "");
		crates.set(owner, (crates.get(owner) ?? 0) + length);
	});
	const code = sections.code ?? 0;
	return {
		file,
		bytes: bytes.length,
		gzip: gzipSync(bytes).length,
		stripped_gzip: gzipSync(Buffer.concat(kept)).length,
		sections,
		named: names.size > 0,
		crates: [...crates]
			.sort((a, b) => b[1] - a[1])
			.map(([owner, size]) => ({ crate: owner, bytes: size, share: size / code })),
	};
}

const reports = [];
for (const file of process.argv.slice(2))
	reports.push(analyze(file, new Uint8Array(await Bun.file(file).arrayBuffer())));
if (process.env.SIZE_JSON) {
	console.log(JSON.stringify(reports, null, 2));
} else {
	for (const report of reports) {
		console.log(
			`${report.file}: ${report.bytes} B, ${report.gzip} B gzip, ${report.stripped_gzip} B gzip without custom sections`,
		);
		for (const [label, size] of Object.entries(report.sections).sort((a, b) => b[1] - a[1]))
			console.log(`  ${String(size).padStart(9)}  ${label}`);
		if (!report.named) console.log("  (no name section: crates unattributed)");
		for (const { crate: owner, bytes, share } of report.crates.slice(0, 20))
			console.log(`  ${(share * 100).toFixed(1).padStart(5)}%  ${String(bytes).padStart(8)}  ${owner}`);
	}
}
