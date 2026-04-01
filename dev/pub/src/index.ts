interface Env {
	VIDEO: R2Bucket;
}

const CONTENT_TYPES: Record<string, string> = {
	".mp4": "video/mp4",
	".webm": "video/webm",
	".mkv": "video/x-matroska",
	".mov": "video/quicktime",
	".ts": "video/mp2t",
};

export default {
	async fetch(request: Request, env: Env): Promise<Response> {
		if (request.method !== "GET" && request.method !== "HEAD") {
			return new Response("Method Not Allowed", { status: 405 });
		}

		const url = new URL(request.url);
		const key = url.pathname.slice(1); // Remove leading slash

		if (!key) {
			return new Response("Not Found", { status: 404 });
		}

		const object = await env.VIDEO.get(key);
		if (!object) {
			return new Response("Not Found", { status: 404 });
		}

		const dotIdx = key.lastIndexOf(".");
		const ext = dotIdx >= 0 ? key.substring(dotIdx).toLowerCase() : "";
		const contentType = CONTENT_TYPES[ext] ?? "application/octet-stream";

		const headers = {
			"Content-Type": contentType,
			"Cache-Control": "public, max-age=2592000",
			"Content-Length": object.size.toString(),
		};

		if (request.method === "HEAD") {
			return new Response(null, { headers });
		}

		return new Response(object.body, { headers });
	},
};
