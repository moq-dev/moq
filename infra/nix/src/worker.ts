// Cloudflare Worker for nix.moq.dev, the project's Nix binary cache. Serves two
// R2 buckets as one cache, written by `just infra nix push` through `nix copy`:
//
//   nix-moq-dev      released packages, kept forever
//   nix-ci-moq-dev   CI dev shells, expired by a lifecycle rule
//
//   /nix-cache-info             cache metadata, answered here
//   /<hash>.narinfo             signed path metadata, pointing into nar/
//   /nar/<filehash>.nar.zst     compressed archive, named by its own hash

interface Env {
	RELEASE: R2Bucket;
	CI: R2Bucket;
}

// Priority 41 puts this right after cache.nixos.org (40), so Nix only asks here
// for paths nixpkgs doesn't serve. Answered here rather than from a bucket so
// the two can't disagree.
const CACHE_INFO = "StoreDir: /nix/store\nWantMassQuery: 1\nPriority: 41\n";

function contentType(key: string): string {
	if (key.endsWith(".narinfo")) return "text/x-nix-narinfo";
	if (key.startsWith("nar/")) return "application/x-nix-nar";
	return "application/octet-stream";
}

// A NAR is named by the hash of its bytes, so it never changes. A narinfo
// stays short so a path the CI lifecycle rule deletes stops being advertised
// soon after.
function cacheControl(key: string): string {
	if (key.startsWith("nar/")) {
		return "public, max-age=31536000, immutable";
	}
	return "public, max-age=300";
}

export default {
	async fetch(request: Request, env: Env): Promise<Response> {
		if (request.method !== "GET" && request.method !== "HEAD") {
			return new Response("Method Not Allowed", { status: 405 });
		}

		const key = new URL(request.url).pathname.slice(1);

		if (key === "nix-cache-info") {
			return new Response(request.method === "HEAD" ? null : CACHE_INFO, {
				headers: { "Content-Type": "text/x-nix-cache-info", "Cache-Control": "public, max-age=3600" },
			});
		}

		if (!key) {
			return new Response("MoQ Nix binary cache. See https://moq.dev/setup/install for usage.\n", {
				headers: { "Content-Type": "text/plain; charset=utf-8" },
			});
		}

		// Releases first, since they never expire. A NAR is named by its hash, so
		// whichever bucket holds it serves the same bytes.
		for (const bucket of [env.RELEASE, env.CI]) {
			const object = await bucket.get(key);
			if (!object) continue;

			const headers = {
				"Content-Type": contentType(key),
				"Cache-Control": cacheControl(key),
				"Content-Length": object.size.toString(),
			};
			return new Response(request.method === "HEAD" ? null : object.body, { headers });
		}

		return new Response("Not Found", { status: 404 });
	},
};
