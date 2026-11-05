/** Sweep compressed publishers x plain readers, reporting one shared inflated copy per group. */
import { gzipSync } from "node:zlib";
import { fanout, tails } from "./compression-fanout.ts";

console.log("publishers,readers,phase,us,size_bytes");
for (const row of fanout())
	console.log([row.publishers, row.readers, row.phase, row.us.toFixed(1), row.size_bytes].join(","));

// This entry is an application importing only its track consumer. Synchronous model writes
// share the module, so its bundle includes the encoder as well as the decoder.
const bundle = await Bun.build({
	entrypoints: [new URL("./fixtures/compression-consumer.ts", import.meta.url).pathname],
	minify: true,
	target: "browser",
});
if (!bundle.success) throw new AggregateError(bundle.logs, "consumer bundle failed");
const bytes = new Uint8Array(await bundle.outputs[0].arrayBuffer());
console.log("measurement,size_bytes");
console.log(`consumer_bundle,${bytes.length}`);
console.log(`consumer_bundle_gzip,${gzipSync(bytes).length}`);

console.log("publishers,readers,frame_bytes,compressed,ns_per_frame,size_bytes");
for (const row of tails())
	console.log(
		[
			row.publishers,
			row.readers,
			row.frame_bytes,
			row.compressed,
			row.ns_per_frame.toFixed(0),
			row.size_bytes,
		].join(","),
	);
