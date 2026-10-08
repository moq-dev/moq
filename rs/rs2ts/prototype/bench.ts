import { U64 } from "../../../js/net/src/util/u64";
import { zigzag as generatedZigzag } from "./generated";
import { add, zigzag } from "./runtime";

const iterations = 500_000;
let sink: U64 | bigint = 0n;
const rows = [];

for (const range of ["small", "wide"] as const) {
	const values = Array.from(
		{ length: 256 },
		(_, i) => new U64(range === "small" ? 0 : (0x12345678 + i * 0x800000) >>> 0, (i * 0x1020304) >>> 0),
	);
	const bigValues = values.map((v) => v.toBigInt());
	const delta = U64.fromNumber(17);
	const runs = [
		{
			name: "handwritten halves",
			run: (i: number) => {
				sink = zigzag(add(values[i & 255], delta));
			},
			samples: [] as number[],
		},
		{
			name: "generated halves",
			run: (i: number) => {
				sink = generatedZigzag(add(values[i & 255], delta));
			},
			samples: [] as number[],
		},
		{
			name: "bigint",
			run: (i: number) => {
				const sum = bigValues[i & 255] + 17n;
				if (sum >= 1n << 64n) throw new RangeError("overflow");
				const signed = BigInt.asIntN(64, sum);
				sink = BigInt.asUintN(64, (signed << 1n) ^ (signed >> 63n));
			},
			samples: [] as number[],
		},
	];
	for (let round = 0; round < 9; round++) {
		// Rotate order so each representation runs first, second, and third.
		for (let n = 0; n < runs.length; n++) {
			const entry = runs[(n + round) % runs.length];
			const start = performance.now();
			for (let i = 0; i < iterations; i++) entry.run(i);
			if (round >= 2) entry.samples.push(((performance.now() - start) * 1e6) / iterations);
		}
	}
	for (const entry of runs) {
		entry.samples.sort((a, b) => a - b);
		rows.push({
			range,
			mapping: entry.name,
			"min ns": entry.samples[0].toFixed(1),
			"median ns": entry.samples[3].toFixed(1),
			"max ns": entry.samples[6].toFixed(1),
		});
	}
}
console.table(rows);
console.log("sink:", String(sink));

for (const entrypoint of ["runtime", "generated"]) {
	const build = await Bun.build({
		entrypoints: [`${import.meta.dir}/${entrypoint}.ts`],
		minify: true,
		target: "browser",
	});
	if (!build.success) throw new AggregateError(build.logs, "runtime bundle failed");
	const bytes = new Uint8Array(await build.outputs[0].arrayBuffer());
	console.log(JSON.stringify({ entrypoint, bytes: bytes.byteLength, gzip_bytes: Bun.gzipSync(bytes).byteLength }));
}
