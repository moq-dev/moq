// token.moq.dev trades a GitHub Actions OIDC token for a short-lived moq-bot
// installation token, so no repository holds the moq-bot key.
//
// A workflow asks for an ID token with this audience and POSTs it to a policy's
// path. The token is issued only when GitHub signed the ID token for a moq-dev
// repository's own policy workflow running on `main`, and it is scoped to that
// one repository with only the policy's permissions.

export interface Env {
	APP_CLIENT_ID: string;
	// The moq-bot private key as PKCS#8 PEM; see README.md for the conversion.
	APP_PRIVATE_KEY: string;
}

export const ISSUER = "https://token.actions.githubusercontent.com";
export const AUDIENCE = "token.moq.dev";
// The owner's numeric ID is checked too, since a deleted organization's name can be taken.
const OWNER = "moq-dev";
const OWNER_ID = "248218914";
const MAIN = "refs/heads/main";

export interface Policy {
	workflow: string;
	permissions: Record<string, "read" | "write">;
}

export const POLICIES: Record<string, Policy> = {
	// quest land's push: moves PR branches and fast-forwards main, including PRs
	// that change workflows.
	"/land": { workflow: ".github/workflows/land.yml", permissions: { contents: "write", workflows: "write" } },
};

export interface Claims {
	iss?: string;
	aud?: string | string[];
	exp?: number;
	nbf?: number;
	repository?: string;
	repository_id?: string;
	repository_owner?: string;
	repository_owner_id?: string;
	ref?: string;
	workflow_ref?: string;
	job_workflow_ref?: string;
}

export class Denied extends Error {}

/** The repository the claims may get a token for under `policy`, or Denied. */
export function authorize(claims: Claims, policy: Policy, now: number): { repository: string; id: number } {
	if (claims.iss !== ISSUER) throw new Denied("issuer");
	const audiences = Array.isArray(claims.aud) ? claims.aud : [claims.aud];
	if (!audiences.includes(AUDIENCE)) throw new Denied("audience");
	if (typeof claims.exp !== "number" || claims.exp <= now) throw new Denied("expired");
	if (typeof claims.nbf === "number" && claims.nbf > now + 60) throw new Denied("not yet valid");
	if (claims.repository_owner !== OWNER || claims.repository_owner_id !== OWNER_ID) throw new Denied("owner");
	if (!claims.repository?.startsWith(`${OWNER}/`) || !claims.repository_id) throw new Denied("repository");
	// A pushed branch's or a PR's workflow runs with another ref; pull_request_target
	// and push to main run main's own file, which only the gate or a bypass can change.
	if (claims.ref !== MAIN) throw new Denied("ref");
	const workflow = `${claims.repository}/${policy.workflow}@${MAIN}`;
	if (claims.workflow_ref !== workflow) throw new Denied("workflow");
	// A reusable workflow the policy workflow calls runs its jobs under the
	// caller's workflow_ref, naming itself in job_workflow_ref, which GitHub
	// documents only for such jobs; only a job defined in the policy workflow counts.
	if (claims.job_workflow_ref !== undefined && claims.job_workflow_ref !== workflow) throw new Denied("job workflow");
	return { repository: claims.repository, id: Number(claims.repository_id) };
}

function base64url(data: ArrayBuffer | Uint8Array | string): string {
	const bytes = typeof data === "string" ? new TextEncoder().encode(data) : new Uint8Array(data);
	let binary = "";
	for (const byte of bytes) binary += String.fromCharCode(byte);
	return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

function unbase64url(text: string): Uint8Array {
	const binary = atob(text.replace(/-/g, "+").replace(/_/g, "/"));
	return Uint8Array.from(binary, (char) => char.charCodeAt(0));
}

const RS256 = { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" };

export interface Jwks {
	keys: (JsonWebKey & { kid?: string })[];
}

/** The claims of a JWT that one of `jwks`'s keys signed with RS256, or Denied. */
export async function verify(token: string, jwks: Jwks): Promise<Claims> {
	const parts = token.split(".");
	if (parts.length !== 3) throw new Denied("malformed");
	const [header, payload, signature] = parts;
	const { alg, kid } = decode(header);
	if (alg !== "RS256") throw new Denied("algorithm");
	const jwk = jwks.keys.find((key) => key.kid === kid);
	if (!jwk) throw new Denied("unknown key");
	const key = await crypto.subtle.importKey("jwk", jwk, RS256, false, ["verify"]);
	const signed = new TextEncoder().encode(`${header}.${payload}`);
	let bytes: Uint8Array;
	try {
		bytes = unbase64url(signature);
	} catch {
		throw new Denied("malformed");
	}
	if (!(await crypto.subtle.verify(RS256, key, bytes, signed))) throw new Denied("signature");
	return decode(payload);
}

/** A JWT segment's JSON object, or Denied. */
function decode(segment: string) {
	try {
		const value = JSON.parse(new TextDecoder().decode(unbase64url(segment)));
		if (typeof value !== "object" || value === null) throw new Error("not an object");
		return value;
	} catch {
		throw new Denied("malformed");
	}
}

let cached: { jwks: Jwks; until: number } | undefined;

async function githubJwks(now: number): Promise<Jwks> {
	if (cached && cached.until > now) return cached.jwks;
	const response = await fetch(`${ISSUER}/.well-known/jwks`);
	if (!response.ok) throw new Error(`jwks: ${response.status}`);
	cached = { jwks: await response.json(), until: now + 600 };
	return cached.jwks;
}

async function appJwt(env: Env, now: number): Promise<string> {
	const der = unbase64url(
		env.APP_PRIVATE_KEY.replace(/-----[A-Z ]+-----/g, "")
			.replace(/\s+/g, "")
			.replace(/\+/g, "-")
			.replace(/\//g, "_"),
	);
	const key = await crypto.subtle.importKey("pkcs8", der, RS256, false, ["sign"]);
	const body = `${base64url(JSON.stringify({ alg: "RS256", typ: "JWT" }))}.${base64url(
		JSON.stringify({ iss: env.APP_CLIENT_ID, iat: now - 60, exp: now + 540 }),
	)}`;
	return `${body}.${base64url(await crypto.subtle.sign(RS256, key, new TextEncoder().encode(body)))}`;
}

async function github(path: string, jwt: string, body?: unknown): Promise<Response> {
	return fetch(`https://api.github.com${path}`, {
		method: body ? "POST" : "GET",
		headers: {
			Accept: "application/vnd.github+json",
			Authorization: `Bearer ${jwt}`,
			"User-Agent": "token.moq.dev",
			"X-GitHub-Api-Version": "2022-11-28",
		},
		body: body ? JSON.stringify(body) : undefined,
	});
}

export default {
	async fetch(request: Request, env: Env): Promise<Response> {
		const policy = POLICIES[new URL(request.url).pathname];
		if (!policy) return new Response("not found\n", { status: 404 });
		if (request.method !== "POST") return new Response("POST only\n", { status: 405 });
		const now = Math.floor(Date.now() / 1000);

		let repository: { repository: string; id: number };
		try {
			const bearer = request.headers.get("Authorization")?.match(/^Bearer (.+)$/i)?.[1];
			if (!bearer) throw new Denied("no bearer token");
			repository = authorize(await verify(bearer, await githubJwks(now)), policy, now);
		} catch (error) {
			if (error instanceof Denied) return new Response(`denied: ${error.message}\n`, { status: 403 });
			throw error;
		}

		const jwt = await appJwt(env, now);
		const installation = await github(`/repos/${repository.repository}/installation`, jwt);
		if (!installation.ok) return new Response("moq-bot is not installed on the repository\n", { status: 403 });
		const { id } = (await installation.json()) as { id: number };
		const token = await github(`/app/installations/${id}/access_tokens`, jwt, {
			repository_ids: [repository.id],
			permissions: policy.permissions,
		});
		if (!token.ok) return new Response(`github: ${token.status}\n`, { status: 502 });
		const { token: value, expires_at } = (await token.json()) as { token: string; expires_at: string };
		return Response.json({ token: value, expires_at });
	},
};
