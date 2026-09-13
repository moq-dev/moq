import { expect, test } from "bun:test";
import { checkIdentity, hex, nonce, unhex } from "./encoding.ts";
import {
	type Code,
	Credential,
	DOMAIN_DATAGRAM,
	DOMAIN_GROUP,
	type Domain,
	type Failure,
	isFailure,
	opaqueName,
	open,
	openCatalog,
	protect,
	protectCatalog,
} from "./index.ts";

const url = new URL("../../../drafts/moq-e2ee-01.json", import.meta.url);
const vectors = (await Bun.file(url).json()) as Vectors;

type CredentialJson = {
	profile: string;
	context: string;
	generation: number;
	kid: number;
	secret: string;
};

type Derivation = CredentialJson & {
	id: string;
	semantic_name: string;
	domain: Domain;
	physical_name: string;
	name_material: string;
	key: string;
};

type Payload = CredentialJson & {
	id: string;
	semantic_name: string;
	physical_name: string;
	domain: Domain;
	group: number;
	frame: number;
	nonce: string;
	plaintext: string;
	payload: string;
	payload_limit: number;
	key: string;
	header?: string;
};

type Negative = { id: string; error: string } & (
	| {
			operation: "open";
			credential: CredentialJson;
			physical_name: string;
			domain: Domain;
			group: number;
			frame: number;
			payload: string;
			payload_limit: number;
	  }
	| {
			operation: "protect";
			key: string;
			group: number;
			frame: number;
			plaintext_len: number;
			payload_limit: number;
			header?: string;
	  }
	| { operation: "identity"; group: string; frame: number | "NaN" | "Infinity" | "-Infinity" }
	| { operation: "credential"; credential: CredentialJson }
	| { operation: "bytes"; field: "context" | "semantic_name"; length: number }
);

type Vectors = {
	profile: string;
	credential: CredentialJson;
	derivation: Derivation[];
	naming: { id: string; semantic_name: string; physical_name: string; name_material: string }[];
	groups: Payload[];
	datagrams: Payload[];
	catalog: Payload[];
	negative: Negative[];
};

function credential(row: CredentialJson): Credential {
	return new Credential({
		profile: row.profile,
		context: unhex(row.context),
		generation: row.generation,
		kid: row.kid,
		secret: unhex(row.secret),
	});
}

function parseFrame(frame: number | "NaN" | "Infinity" | "-Infinity"): number {
	if (frame === "NaN") return Number.NaN;
	if (frame === "Infinity") return Number.POSITIVE_INFINITY;
	if (frame === "-Infinity") return Number.NEGATIVE_INFINITY;
	return frame;
}

async function expectFailure(code: Code, fn: () => unknown | Promise<unknown>): Promise<void> {
	try {
		await fn();
	} catch (error) {
		expect(isFailure(error)).toBe(true);
		expect((error as Failure).code).toBe(code);
		return;
	}
	throw new Error(`expected ${code}`);
}

test("profile matches the shared vectors", () => {
	expect(vectors.profile).toBe("moq-e2ee-01");
});

for (const row of vectors.derivation) {
	test(`derivation: ${row.id}`, async () => {
		const cred = credential(row);
		const physical = await opaqueName(cred, row.semantic_name);
		expect(physical).toBe(row.physical_name);
	});
}

for (const row of vectors.naming) {
	test(`naming: ${row.id}`, async () => {
		const cred = credential(vectors.credential);
		expect(await opaqueName(cred, row.semantic_name)).toBe(row.physical_name);
	});
}

for (const row of [...vectors.groups, ...vectors.datagrams, ...vectors.catalog]) {
	test(`payload: ${row.id}`, async () => {
		const cred = credential(row);
		const physical = await opaqueName(cred, row.semantic_name);
		expect(physical).toBe(row.physical_name);
		expect(hex(nonce(row.group, row.frame))).toBe(row.nonce);
		const sealed = await protect(cred, {
			physicalName: physical,
			domain: row.domain,
			group: row.group,
			frame: row.frame,
			plaintext: unhex(row.plaintext),
			payloadLimit: row.payload_limit,
		});
		expect(hex(sealed)).toBe(row.payload);
		const opened = await open(cred, {
			physicalName: physical,
			domain: row.domain,
			group: row.group,
			frame: row.frame,
			payload: sealed,
			payloadLimit: row.payload_limit,
		});
		expect(hex(opened)).toBe(row.plaintext);
	});
}

test("catalog primitives round-trip JSON and compressed representations", async () => {
	const json = vectors.catalog.find((row) => row.id === "catalog-json");
	const compressed = vectors.catalog.find((row) => row.id === "catalog-compressed");
	if (!json || !compressed) throw new Error("missing catalog vectors");
	const cred = credential(json);
	const protectedJson = await protectCatalog({
		credential: cred,
		semanticName: json.semantic_name,
		plaintext: unhex(json.plaintext),
	});
	expect(protectedJson.physicalName).toBe(json.physical_name);
	expect(hex(protectedJson.payload)).toBe(json.payload);
	expect(
		hex(await openCatalog({ credential: cred, semanticName: json.semantic_name, payload: protectedJson.payload })),
	).toBe(json.plaintext);

	const credZ = credential(compressed);
	const protectedZ = await protectCatalog({
		credential: credZ,
		semanticName: compressed.semantic_name,
		plaintext: unhex(compressed.plaintext),
	});
	expect(protectedZ.physicalName).toBe(compressed.physical_name);
	expect(hex(protectedZ.payload)).toBe(compressed.payload);
});

test("catalog rendition keys are physical names", () => {
	const json = vectors.catalog.find((row) => row.id === "catalog-json");
	const video = vectors.naming.find((row) => row.semantic_name === "video");
	if (!json || !video) throw new Error("missing catalog/video naming");
	const catalog = JSON.parse(new TextDecoder().decode(unhex(json.plaintext))) as {
		video?: { renditions?: Record<string, unknown> };
	};
	expect(Object.keys(catalog.video?.renditions ?? {})).toEqual([video.physical_name]);
});

for (const row of vectors.negative) {
	test(`negative: ${row.id}`, async () => {
		await expectFailure(row.error as Code, async () => {
			switch (row.operation) {
				case "open": {
					const cred = credential(row.credential);
					await open(cred, {
						physicalName: row.physical_name,
						domain: row.domain,
						group: row.group,
						frame: row.frame,
						payload: unhex(row.payload),
						payloadLimit: row.payload_limit,
					});
					break;
				}
				case "protect": {
					const datagram = row.header !== undefined;
					const cred = credential(datagram ? vectors.datagrams[0] : vectors.credential);
					const physical = await opaqueName(cred, datagram ? vectors.datagrams[0].semantic_name : "video");
					await protect(cred, {
						physicalName: physical,
						domain: datagram ? DOMAIN_DATAGRAM : DOMAIN_GROUP,
						group: row.group,
						frame: row.frame,
						plaintext: new Uint8Array(row.plaintext_len),
						payloadLimit: row.payload_limit,
					});
					break;
				}
				case "identity":
					checkIdentity(Number(row.group), parseFrame(row.frame));
					break;
				case "credential":
					credential(row.credential);
					break;
				case "bytes": {
					const oversized = new Uint8Array(row.length);
					if (row.field === "context") {
						new Credential({
							profile: vectors.credential.profile,
							context: oversized,
							generation: vectors.credential.generation,
							kid: vectors.credential.kid,
							secret: unhex(vectors.credential.secret),
						});
					} else {
						await opaqueName(credential(vectors.credential), "x".repeat(row.length));
					}
					break;
				}
				default:
					throw new Error(`unknown negative operation: ${JSON.stringify(row)}`);
			}
		});
	});
}

test("datagram oversize uses the vector header budget", async () => {
	const row = vectors.datagrams[0];
	const negative = vectors.negative.find((n) => n.id === "datagram-oversize");
	if (negative?.operation !== "protect") throw new Error("missing datagram-oversize");
	const cred = credential(row);
	const physical = await opaqueName(cred, row.semantic_name);
	await protect(cred, {
		physicalName: physical,
		domain: DOMAIN_DATAGRAM,
		group: row.group,
		frame: 0,
		plaintext: new Uint8Array(row.payload_limit - 16),
		payloadLimit: row.payload_limit,
	});
	await expectFailure("oversize", () =>
		protect(cred, {
			physicalName: physical,
			domain: DOMAIN_DATAGRAM,
			group: row.group + 1,
			frame: 0,
			plaintext: new Uint8Array(negative.plaintext_len),
			payloadLimit: negative.payload_limit,
		}),
	);
});
