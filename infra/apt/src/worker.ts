// Cloudflare Worker for apt.moq.dev. Serves a flat-ish apt repository out of
// the apt-moq-dev R2 bucket. Layout written by infra/apt/publish.sh:
//
//   /moq-keyring.gpg                                  public signing key (binary/dearmored)
//   /dists/stable/InRelease                           signed metadata
//   /dists/stable/Release                             metadata
//   /dists/stable/Release.gpg                         detached signature
//   /dists/stable/main/binary-{amd64,arm64}/Packages{,.gz}
//   /pool/main/<binary>/<binary>_<ver>_<arch>.deb     actual packages

interface Env {
	APT: R2Bucket;
}

// Content-Type mapping. apt is picky about Release/InRelease being text/plain
// and .deb being the official MIME type, otherwise some proxies mangle them.
const CONTENT_TYPES: Record<string, string> = {
	".deb": "application/vnd.debian.binary-package",
	".gpg": "application/pgp-signature",
	".gz": "application/gzip",
	".xz": "application/x-xz",
	".bz2": "application/x-bzip2",
};

// Exact filenames whose Content-Type isn't determined by extension.
const CONTENT_TYPE_BY_NAME: Record<string, string> = {
	Release: "text/plain; charset=utf-8",
	InRelease: "text/plain; charset=utf-8",
	Packages: "text/plain; charset=utf-8",
	Sources: "text/plain; charset=utf-8",
};

// Cache long for the content-addressed package blobs: a given .deb under a
// given pool path never changes. Cache short for repo metadata signatures like
// Release.gpg, which get rewritten every release. The keyring sits in between:
// it's a fixed filename whose *contents* change if the signing key is ever
// rotated, so it must NOT be immutable -- a stale immutable copy would keep
// breaking `apt-get update` (NO_PUBKEY) for the whole cache lifetime. It's tiny
// and only fetched at first-time setup, so a modest TTL is plenty.
function cacheControl(key: string): string {
	if (key.endsWith(".deb") || key.includes("/pool/")) {
		return "public, max-age=2592000, immutable";
	}
	if (key === "moq-keyring.gpg") {
		return "public, max-age=3600";
	}
	return "public, max-age=300";
}

function contentType(key: string): string {
	const base = key.substring(key.lastIndexOf("/") + 1);
	if (base in CONTENT_TYPE_BY_NAME) {
		return CONTENT_TYPE_BY_NAME[base];
	}
	const dotIdx = base.lastIndexOf(".");
	const ext = dotIdx >= 0 ? base.substring(dotIdx).toLowerCase() : "";
	return CONTENT_TYPES[ext] ?? "application/octet-stream";
}

export default {
	async fetch(request: Request, env: Env): Promise<Response> {
		if (request.method !== "GET" && request.method !== "HEAD") {
			return new Response("Method Not Allowed", { status: 405 });
		}

		const url = new URL(request.url);
		const key = url.pathname.slice(1);

		if (!key) {
			return new Response("MoQ apt repository. See https://moq.dev/install/linux for usage.\n", {
				status: 200,
				headers: { "Content-Type": "text/plain; charset=utf-8" },
			});
		}

		const object = await env.APT.get(key);
		if (!object) {
			return new Response("Not Found", { status: 404 });
		}

		const headers = {
			"Content-Type": contentType(key),
			"Cache-Control": cacheControl(key),
			"Content-Length": object.size.toString(),
		};

		if (request.method === "HEAD") {
			return new Response(null, { headers });
		}

		return new Response(object.body, { headers });
	},
};
