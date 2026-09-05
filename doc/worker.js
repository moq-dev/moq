// Pages that moved when the site was reorganized. Old links live on in blog
// posts, the agent skill, and search engines, so they redirect rather than 404.
// Keys are old paths without a trailing slash or `.html`; values are new paths.
const MOVED = {
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
	"/lib/rs/crate/web-transport": "/lib/rs/",
	"/lib/rs/crate/libmoq": "/lib/c/",
	"/lib/rs/crate/moq-boy": "/bin/demo",
	"/lib/js/env/web": "/lib/js/",
	"/lib/js/env/native": "/lib/js/",
	"/lib/js/@moq/boy": "/bin/demo",
	"/lib/js/@moq/demo": "/bin/demo",
	"/lib/py/moq-rs": "/lib/py/",
	"/lib/kt/moq": "/lib/kt/",
	"/lib/swift/moq": "/lib/swift/",
	"/lib/go/moq": "/lib/go/",
	"/lib/go/moq-ffi": "/lib/go/",
	"/lib/dart/moq": "/lib/dart/",
};

// Whole directories that were flattened one level.
const FLATTENED = [
	[/^\/lib\/rs\/crate\/([^/]+)$/, "/lib/rs/$1"],
	[/^\/lib\/js\/@moq\/([^/]+)$/, "/lib/js/$1"],
];

// Directories that became single pages, so the trailing-slash form has no index.
const COLLAPSED = ["/concept/standard"];

const MOVED_PERMANENTLY = 301;

function redirect(pathname) {
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

export default {
	async fetch(request, env) {
		const url = new URL(request.url);
		const target = redirect(url.pathname);
		if (target && target !== url.pathname) {
			url.pathname = target;
			return Response.redirect(url.toString(), MOVED_PERMANENTLY);
		}
		return env.ASSETS.fetch(request);
	},
};
