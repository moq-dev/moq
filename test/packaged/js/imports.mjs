// Import every entry point named on the command line, from the isolated
// consumer's own node_modules. Copied next to the consumer's package.json by
// test/packaged/js.sh so the specifiers resolve the way a real dependent's do.
//
// A missing file, an undeclared dependency, or a broken `exports` map all
// surface here as an unresolved specifier; anything the module does at import
// time surfaces as whatever it throws.
const specs = process.argv.slice(2);
if (specs.length === 0) {
	console.error("usage: node imports.mjs <specifier>...");
	process.exit(2);
}

let failed = 0;
for (const spec of specs) {
	try {
		const mod = await import(spec);
		const exported = Object.keys(mod).length;
		console.log(`  ok   ${spec} (${exported} exports)`);
	} catch (err) {
		failed += 1;
		console.error(`  FAIL ${spec}: ${err instanceof Error ? err.message : String(err)}`);
	}
}

process.exit(failed === 0 ? 0 : 1);
