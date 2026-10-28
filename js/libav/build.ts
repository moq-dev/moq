// Materializes dist/, the publishable @moq/libav package that the workspace resolves.
//
// With Nix, dist/ is a copy of the `moq-libav` build: a copy, since Vite refuses to serve
// /nix/store, while the `result` out-link keeps the build from being garbage collected.
// Without Nix (Windows), it is the published npm tarball of this exact version instead,
// which lags package.nix until the next release.
import { chmodSync, cpSync, readdirSync, rmSync } from "node:fs";
import { join, resolve, sep } from "node:path";

const dir = import.meta.dir;
const dist = join(dir, "dist");

rmSync(dist, { recursive: true, force: true });

if (Bun.which("nix")) {
	const nix = Bun.spawnSync(["nix", "build", "../..#moq-libav", "--out-link", "result"], {
		cwd: dir,
		stdio: ["inherit", "inherit", "inherit"],
	});
	if (nix.exitCode !== 0) process.exit(nix.exitCode || 1);

	cpSync(join(dir, "result"), dist, { recursive: true, dereference: true });
	// The store is read-only, and so is the copy, which the next build couldn't remove.
	chmodSync(dist, 0o755);
	for (const entry of readdirSync(dist, { recursive: true, withFileTypes: true })) {
		chmodSync(join(entry.parentPath, entry.name), entry.isDirectory() ? 0o755 : 0o644);
	}
} else {
	const { name, version } = (await Bun.file(join(dir, "package.json")).json()) as { name: string; version: string };
	const url = `https://registry.npmjs.org/${name}/${version}`;

	const meta = await fetch(url);
	if (meta.status === 404) {
		throw new Error(
			`${name}@${version} is not published to npm yet, and building it needs Nix, which is not on PATH. Install on a host with Nix until that version is released.`,
		);
	}
	if (!meta.ok) throw new Error(`failed to fetch ${url}: ${meta.status} ${meta.statusText}`);
	const { tarball, integrity } = ((await meta.json()) as { dist: { tarball: string; integrity: string } }).dist;

	const response = await fetch(tarball);
	if (!response.ok) throw new Error(`failed to fetch ${tarball}: ${response.status} ${response.statusText}`);
	const bytes = new Uint8Array(await response.arrayBuffer());

	const [algorithm, expected] = integrity.split("-", 2);
	if (algorithm !== "sha512") throw new Error(`unsupported integrity for ${tarball}: ${integrity}`);
	const actual = new Bun.CryptoHasher("sha512").update(bytes).digest("base64");
	if (actual !== expected) throw new Error(`integrity mismatch for ${tarball}: expected ${expected}, got ${actual}`);

	// npm tarballs nest everything under package/.
	for (const [path, file] of await new Bun.Archive(bytes).files()) {
		const out = resolve(dist, path.replace(/^package\//, ""));
		if (!path.startsWith("package/") || !out.startsWith(dist + sep)) {
			throw new Error(`unexpected path in ${tarball}: ${path}`);
		}
		await Bun.write(out, file);
	}
	console.log(`unpacked ${name}@${version} from npm into dist/; Nix is not on PATH`);
}
