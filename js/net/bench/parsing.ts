/** Measure malformed certificate and path rejection as their inputs grow. */
import { certificateHash } from "../src/connection/connect.ts";
import * as Path from "../src/path.ts";

let rejected = 0;

console.log("parser,input_bytes,ns_per_op");
for (const parser of ["pem", "path"] as const) {
	for (const count of [256, 1024, 4096, 16384]) {
		const input = parser === "pem" ? "-----BEGIN CERTIFICATE-----a".repeat(count) : "a/".repeat(count);
		// Materialize the string before timing, so rope flattening isn't mistaken for parsing.
		input.charCodeAt(input.length - 1);
		const reject = async () => {
			try {
				if (parser === "pem") await certificateHash(input);
				else Path.encode(input as Path.Valid);
			} catch (error) {
				const expected = parser === "pem" ? "armor" : "parts";
				if (!(error instanceof Error) || !error.message.includes(expected)) throw error;
				rejected++;
				return;
			}
			throw new Error(`${parser} accepted malformed input`);
		};

		// Calibrate independently so a slow baseline remains practical to compare.
		let runs = 0;
		const warmup = performance.now();
		do {
			await reject();
			runs++;
		} while (performance.now() - warmup < 20);
		let best = Number.POSITIVE_INFINITY;
		for (let rep = 0; rep < 5; rep++) {
			const start = performance.now();
			for (let i = 0; i < runs; i++) await reject();
			best = Math.min(best, ((performance.now() - start) * 1e6) / runs);
		}
		console.log(`${parser},${input.length},${best.toFixed(1)}`);
	}
}
if (rejected === 0) throw new Error("benchmark did no work");
