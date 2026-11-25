// Cloudflare Worker for nix.moq.dev, the project's Nix binary cache. Serves two
// private Backblaze B2 buckets as one cache, written by `just infra nix push`
// through `nix copy`:
//
//   nix-moq-dev      released packages, kept forever
//   nix-ci-moq-dev   CI dev shells, expired by a lifecycle rule
//
//   /nix-cache-info             cache metadata, answered here
//   /<hash>.narinfo             signed path metadata, pointing into nar/
//   /nar/<filehash>.nar.zst     compressed archive, named by its own hash
//
// Each read is signed with the bucket's read-only key, so nobody can list the
// buckets or download from B2 directly, where egress is billed. Egress from B2
// to Cloudflare is free.

import { AwsClient } from "aws4fetch";

// B2's console scopes a key to one bucket or to all of them, so each bucket
// has its own read-only key.
interface Env {
	B2_RELEASE_ACCESS_KEY_ID: string;
	B2_RELEASE_SECRET_ACCESS_KEY: string;
	B2_CI_ACCESS_KEY_ID: string;
	B2_CI_SECRET_ACCESS_KEY: string;
}

const ENDPOINT = "https://s3.us-east-005.backblazeb2.com";

// A missing secret throws here: a deploy error, not a read failure. No
// retries: a failed read is a miss now, not seconds of backoff while Nix waits.
function client(accessKeyId: string, secretAccessKey: string): AwsClient {
	return new AwsClient({ accessKeyId, secretAccessKey, service: "s3", region: "us-east-005", retries: 0 });
}

// Releases first, since they never expire. A NAR is named by its hash, so
// whichever bucket holds it serves the same bytes.
function buckets(env: Env): { name: string; client: AwsClient }[] {
	return [
		{ name: "nix-moq-dev", client: client(env.B2_RELEASE_ACCESS_KEY_ID, env.B2_RELEASE_SECRET_ACCESS_KEY) },
		{ name: "nix-ci-moq-dev", client: client(env.B2_CI_ACCESS_KEY_ID, env.B2_CI_SECRET_ACCESS_KEY) },
	];
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

// B2 answers a throttle or an outage this way; Nix should build from source
// rather than disable the cache, which it does on a server error.
function unavailable(status: number): boolean {
	return status === 429 || status >= 500;
}

// Resumes a body B2 fails to finish. The response already sent its status and
// Content-Length, so the only other outcome is a reset connection, which Nix
// answers by disabling the whole cache for a minute. Resumes are capped per
// response so a persistently failing object can't loop; past the cap the
// stream errors as it would have.
const RESUMES = 5;

function resume(
	client: AwsClient,
	url: string,
	key: string,
	first: { body: ReadableStream<Uint8Array>; size: number; etag: string },
): ReadableStream<Uint8Array> {
	let reader = first.body.getReader();
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
					// Every byte already arrived; only the end of the stream was lost.
					if (sent === first.size) return controller.close();
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

				// Only the same object continues the stream: the etag guard turns an
				// overwrite into 412, which ends the stream instead of splicing bytes.
				try {
					const next = await client.fetch(url, {
						headers: { Range: `bytes=${sent}-`, "If-Match": first.etag },
					});
					if (next.status === 412) {
						await next.body?.cancel();
						return controller.error(new Error(`object changed mid-read: ${key}`));
					}
					if (next.status !== 206 || !next.body) {
						await next.body?.cancel();
						throw new Error(`ranged read answered ${next.status}`);
					}
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

		let failed = false;
		for (const { name: bucket, client } of buckets(env)) {
			const url = `${ENDPOINT}/${bucket}/${key}`;
			let response: Response;
			try {
				response = await client.fetch(url, { method: request.method });
			} catch (error) {
				console.error(`Nix cache read unavailable: ${bucket}/${key}`, error);
				failed = true;
				continue;
			}
			if (response.status === 404 || unavailable(response.status)) {
				await response.body?.cancel();
				// Treat an unavailable read as a miss, so the other bucket can still
				// serve the path and Nix otherwise builds it from source without
				// disabling the cache.
				if (response.status !== 404) {
					console.error(`Nix cache read unavailable: ${bucket}/${key} answered ${response.status}`);
					failed = true;
				}
				continue;
			}

			// Anything else, such as a rejected key, is a deploy error.
			const size = Number(response.headers.get("Content-Length"));
			const etag = response.headers.get("ETag");
			if (response.status !== 200 || !Number.isSafeInteger(size) || !etag) {
				await response.body?.cancel();
				throw new Error(`${bucket}/${key} answered ${response.status} without a usable length and etag`);
			}

			const headers = {
				"Content-Type": contentType(key),
				"Cache-Control": cacheControl(key),
				"Content-Length": size.toString(),
			};
			if (request.method === "HEAD" || !response.body) return new Response(null, { headers });

			// A plain stream drops Content-Length from the response, and with it
			// Nix's check for a short body. The fixed-length stream keeps it, and
			// errors, rather than ends, if the resumed bytes don't add up.
			const { readable, writable } = new FixedLengthStream(size);
			// A failed pipe already errored `readable`, which resets the client.
			resume(client, url, key, { body: response.body, size, etag })
				.pipeTo(writable)
				.catch(() => {});
			return new Response(readable, { headers });
		}

		// Keep intermediaries from caching a transient failure as missing.
		const headers = failed ? { "Cache-Control": "no-store" } : undefined;
		return new Response("Not Found", { status: 404, headers });
	},
};
