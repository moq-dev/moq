// Pages that moved when the site was reorganized. Old links live on in blog
// posts, the agent skill, and search engines, so they redirect rather than 404.
// Keys are old paths without a trailing slash or `.html`; values are new paths.
const MOVED: Record<string, string> = {
	"/setup/linux": "/setup/install",
	"/setup/windows": "/setup/install",
	"/setup/demo/web": "/bin/demo",
	"/setup/demo/boy": "/bin/demo",
	"/demo": "/bin/demo",
	"/demo/moq-boy": "/bin/demo",
	"/bin/web": "/bin/demo",
	"/concept/layer": "/concept/",
	"/concept/layer/quic": "/concept/transport",
	"/concept/layer/web-transport": "/concept/transport",
	"/concept/layer/web-socket": "/concept/transport",
	"/concept/layer/iroh": "/concept/transport",
	"/concept/layer/moq-lite": "/concept/moq-lite",
	"/concept/layer/hang": "/concept/hang",
	"/concept/standard/moq-transport": "/concept/standard",
	"/concept/standard/msf": "/concept/standard",
	"/concept/standard/loc": "/concept/standard",
	"/concept/standard/interop": "/concept/standard",
	"/lib/rs/env": "/lib/rs/",
	"/lib/rs/env/native": "/lib/rs/",
	"/lib/rs/env/wasm": "/lib/rs/",
	"/lib/rs/crate": "/lib/rs/",
	"/lib/rs/crate/moq-native": "/lib/rs/",
	"/lib/rs/crate/moq-tokio": "/lib/rs/",
	"/lib/rs/crate/web-transport": "/lib/rs/",
	"/lib/rs/crate/libmoq": "/lib/c/",
	"/lib/rs/crate/moq-boy": "/bin/demo",
	"/lib/rs/crate/moq-room": "/lib/js/room",
	"/lib/rs/moq-room": "/lib/js/room",
	"/lib/js/env/web": "/lib/js/",
	"/lib/js/env/native": "/lib/js/",
	"/lib/js/@moq/boy": "/bin/demo",
	"/lib/js/api/boy": "/bin/demo",
	"/lib/js/api/boy/element": "/bin/demo",
	"/lib/js/api/signals/solid": "/lib/js/api/signals",
	"/lib/js/@moq/demo": "/bin/demo",
	"/lib/py/moq-rs": "/lib/py/",
	"/lib/kt/moq": "/lib/kt/",
	"/lib/swift/moq": "/lib/swift/",
	"/lib/go/moq": "/lib/go/",
	"/lib/go/moq-ffi": "/lib/go/",
	"/lib/dart/moq": "/lib/dart/",
};

// Whole directories that were flattened one level.
const FLATTENED: [RegExp, string][] = [
	[/^\/lib\/rs\/crate\/([^/]+)$/, "/lib/rs/$1"],
	[/^\/lib\/js\/@moq\/([^/]+)$/, "/lib/js/$1"],
];

// Directories that became single pages, so the trailing-slash form has no index.
const COLLAPSED = ["/concept/standard"];

const MOVED_PERMANENTLY = 301;

interface Env {
	ASSETS: { fetch(request: Request): Promise<Response> };
}

function redirect(pathname: string): string | undefined {
	const key =
		pathname
			.replace(/\/index(?:\.html)?$/, "")
			.replace(/\.html$/, "")
			.replace(/\/$/, "") || "/";
	if (COLLAPSED.includes(key) && key !== pathname) return key;
	if (key in MOVED) return MOVED[key];
	for (const [pattern, target] of FLATTENED) {
		if (pattern.test(key)) return key.replace(pattern, target);
	}
	return undefined;
}

// The quality an `Accept` header gives a media type: the most specific range that
// matches, as RFC 9110 orders them, or 0 when none does.
function quality(accept: string, type: string): number {
	const [group] = type.split("/");
	let best = { rank: -1, q: 0 };
	for (const item of accept.split(",")) {
		const [range, ...params] = item.trim().toLowerCase().split(";");
		const rank = range === type ? 2 : range === `${group}/*` ? 1 : range === "*/*" ? 0 : -1;
		if (rank <= best.rank) continue;
		const q = params.map((p) => p.trim()).find((p) => p.startsWith("q="));
		best = { rank, q: q === undefined ? 1 : Number(q.slice(2)) || 0 };
	}
	return best.q;
}

// An agent asks for Markdown by naming it, at least as high as HTML. A wildcard
// such as curl's `*/*` is no ask, so it still gets HTML.
export function prefersMarkdown(accept: string): boolean {
	if (!/(^|,)\s*text\/markdown\s*(;|,|$)/i.test(accept)) return false;
	const markdown = quality(accept, "text/markdown");
	return markdown > 0 && markdown >= quality(accept, "text/html");
}

// The Markdown twin the site builds for a page, or undefined for anything that is not a page.
function twin(pathname: string): string | undefined {
	// A directory's index is `dir.md`, and the home page has none.
	if (pathname.endsWith("/")) return pathname === "/" ? undefined : `${pathname.slice(0, -1)}.md`;
	const leaf = pathname.slice(pathname.lastIndexOf("/") + 1);
	if (leaf.endsWith(".html")) return `${pathname.slice(0, -".html".length)}.md`;
	return leaf.includes(".") ? undefined : `${pathname}.md`;
}

async function serve(request: Request, env: Env): Promise<Response> {
	const url = new URL(request.url);
	if (request.method === "GET" || request.method === "HEAD") {
		const path = prefersMarkdown(request.headers.get("Accept") ?? "") && twin(url.pathname);
		if (path) {
			url.pathname = path;
			const response = await env.ASSETS.fetch(new Request(url, request));
			// Only a missing twin falls through: a 304 revalidates the Markdown the client holds.
			if (response.status !== 404) return response;
		}
	}
	return env.ASSETS.fetch(request);
}

export default {
	async fetch(request: Request, env: Env): Promise<Response> {
		const url = new URL(request.url);
		const target = redirect(url.pathname);
		if (target && target !== url.pathname) {
			url.pathname = target;
			return Response.redirect(url.toString(), MOVED_PERMANENTLY);
		}
		// Every page has two representations, so a cache must key on `Accept`.
		const served = await serve(request, env);
		const response = new Response(served.body, served);
		response.headers.append("Vary", "Accept");
		return response;
	},
};
