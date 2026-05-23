// Cloudflare Worker for rpm.moq.dev. Serves a yum/dnf repository out of the
// rpm-moq-dev R2 bucket. Layout written by infra/rpm/publish.sh:
//
//   /moq-archive-keyring.gpg                        public signing key
//   /moq.repo                                       .repo file users drop into /etc/yum.repos.d/
//   /el9/x86_64/repodata/repomd.xml{,.asc}          signed metadata
//   /el9/x86_64/repodata/<hash>-primary.xml.gz      indices
//   /el9/x86_64/<binary>-<ver>-1.<arch>.rpm         actual packages

interface Env {
	RPM: R2Bucket;
}

const CONTENT_TYPES: Record<string, string> = {
	".rpm": "application/x-rpm",
	".xml": "application/xml",
	".gz": "application/gzip",
	".xz": "application/x-xz",
	".bz2": "application/x-bzip2",
	".gpg": "application/pgp-signature",
	".asc": "application/pgp-signature",
	".repo": "text/plain; charset=utf-8",
};

const CONTENT_TYPE_BY_NAME: Record<string, string> = {
	"repomd.xml": "application/xml",
};

// Repo metadata changes per release; .rpm blobs are versioned and immutable.
function cacheControl(key: string): string {
	if (key.endsWith(".rpm") || key.endsWith(".gpg")) {
		return "public, max-age=2592000, immutable";
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
			return new Response("MoQ rpm repository. See https://moq.dev/install/linux for usage.\n", {
				status: 200,
				headers: { "Content-Type": "text/plain; charset=utf-8" },
			});
		}

		const object = await env.RPM.get(key);
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
