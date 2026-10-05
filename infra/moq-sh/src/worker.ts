// Cloudflare Worker for moq.sh. Serves install.sh at every path, with the
// newest moq-cli release baked in as its default version.
import script from "../install.sh";

interface Env {
	VERSION: string;
}

export default {
	async fetch(request: Request, env: Env): Promise<Response> {
		if (request.method !== "GET" && request.method !== "HEAD") {
			return new Response("Method Not Allowed", { status: 405 });
		}

		if (!/^\d+\.\d+\.\d+$/.test(env.VERSION ?? "")) {
			return new Response("moq.sh was deployed without a moq-cli version\n", { status: 500 });
		}

		// Browsers get the script too, so anyone can read what they pipe to sh.
		return new Response(request.method === "HEAD" ? null : script.replace("@MOQ_VERSION@", env.VERSION), {
			headers: {
				"Content-Type": "text/plain; charset=utf-8",
				"Cache-Control": "public, max-age=300",
			},
		});
	},
};
