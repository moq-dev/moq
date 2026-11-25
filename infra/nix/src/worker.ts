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

// Resumes a body R2 fails to finish. The response already sent its status and
// Content-Length, so the only other outcome is a reset connection, which Nix
// answers by disabling the whole cache for a minute. Resumes are capped per
// response so a persistently failing object can't loop; past the cap the
// stream errors as it would have.
const RESUMES = 5;

function resume(bucket: R2Bucket, key: string, object: R2ObjectBody): ReadableStream<Uint8Array> {
	let reader = object.body.getReader();
	let sent = 0;
	let resumes = 0;
	let cancelled = false;

	return new ReadableStream({
		async pull(controller) {
			for (;;) {
				let chunk: ReadableStreamReadResult<Uint8Array> | undefined;
				try {
					chunk = await reader.read();
				} catch (error) {
					if (resumes++ >= RESUMES) {
						console.error(`Nix cache body failed at byte ${sent}, out of resumes: ${key}`, error);
						return controller.error(error);
					}
					console.error(`Nix cache body read failed at byte ${sent}, resuming: ${key}`, error);
				}
				if (chunk?.done) return controller.close();
				if (chunk) {
					sent += chunk.value.byteLength;
					return controller.enqueue(chunk.value);
				}

				// Only the same object continues the stream: the etag guard turns
				// an overwrite into a body-less result, which ends the stream.
				try {
					const next = await bucket.get(key, {
						range: { offset: sent },
						onlyIf: { etagMatches: object.etag },
					});
					if (!next || !("body" in next))
						return controller.error(new Error(`object changed mid-read: ${key}`));
					if (cancelled) return next.body.cancel();
					reader = next.body.getReader();
				} catch (error) {
					console.error(`Nix cache resume failed at byte ${sent}: ${key}`, error);
				}
			}
		},
		cancel(reason) {
			cancelled = true;
			return reader.cancel(reason);
		},
	});
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
		let unavailable = false;
		for (const bucket of [env.RELEASE, env.CI]) {
			let object: R2ObjectBody | null;
			// A misconfigured binding is a deploy error, not a read failure.
			const get = bucket.get.bind(bucket);
			try {
				object = await get(key);
			} catch (error) {
				// R2 exposes no typed error, and its message text is not an interface,
				// so every failed read is one. Treat it as a miss, so the other bucket
				// can still serve the path and Nix otherwise builds it from source
				// without disabling the cache.
				console.error(`Nix cache read unavailable: ${key}`, error);
				unavailable = true;
				continue;
			}
			if (!object) continue;

			const headers = {
				"Content-Type": contentType(key),
				"Cache-Control": cacheControl(key),
				"Content-Length": object.size.toString(),
			};
			if (request.method === "HEAD") return new Response(null, { headers });

			// A plain stream drops Content-Length from the response, and with it
			// Nix's check for a short body. The fixed-length stream keeps it, and
			// errors, rather than ends, if the resumed bytes don't add up.
			const { readable, writable } = new FixedLengthStream(object.size);
			// A failed pipe already errored `readable`, which resets the client.
			resume(bucket, key, object)
				.pipeTo(writable)
				.catch(() => {});
			return new Response(readable, { headers });
		}

		// Keep intermediaries from caching a transient failure as missing.
		const headers = unavailable ? { "Cache-Control": "no-store" } : undefined;
		return new Response("Not Found", { status: 404, headers });
	},
};
