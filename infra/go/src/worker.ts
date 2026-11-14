// Cloudflare Worker for go.moq.dev: a read-only Go module proxy (GOPROXY
// protocol) out of the go-moq-dev R2 bucket. sh/go/publish-ffi.sh and
// infra/go/backfill.sh write its layout:
//
//   /<module>/@v/list               known versions, one per line
//   /<module>/@v/<version>.info     {"Version", "Time"}
//   /<module>/@v/<version>.mod      the version's go.mod
//   /<module>/@v/<version>.zip      the module zip
//
// moq.dev's vanity page points `go get` here with a `mod` go-import tag, so a
// 404 is final: nothing falls back past this proxy, even with GOPROXY=direct.
// `@latest` is optional in the protocol, and the go command reads `list` instead.

interface Env {
	GO: R2Bucket;
}

// A module path, then @v/, then list or one escaped version's file.
const PATH = /^[^@]+\/@v\/(list|[^/]+\.(info|mod|zip))$/;

const CONTENT_TYPES: Record<string, string> = {
	list: "text/plain; charset=utf-8",
	info: "application/json",
	mod: "text/plain; charset=utf-8",
	zip: "application/zip",
};

export default {
	async fetch(request: Request, env: Env): Promise<Response> {
		if (request.method !== "GET" && request.method !== "HEAD") {
			return new Response("Method Not Allowed", { status: 405, headers: { Allow: "GET, HEAD" } });
		}

		const key = new URL(request.url).pathname.slice(1);
		if (!key) {
			return new Response("MoQ Go module proxy. See https://moq.dev/lib/go for usage.\n", {
				headers: { "Content-Type": "text/plain; charset=utf-8" },
			});
		}

		const match = PATH.exec(key);
		const object = match ? await env.GO.get(key) : null;
		if (!match || !object) {
			return new Response("Not Found", { status: 404, headers: { "Cache-Control": "no-store" } });
		}

		const kind = match[2] ?? "list";
		const headers = {
			"Content-Type": CONTENT_TYPES[kind],
			// A published version never changes, since sum.golang.org pins its first hash.
			// The list grows with every release.
			"Cache-Control": kind === "list" ? "public, max-age=60" : "public, max-age=31536000, immutable",
			"Content-Length": object.size.toString(),
		};
		return new Response(request.method === "HEAD" ? null : object.body, { headers });
	},
};
