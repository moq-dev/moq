# `@moq/auth`

```ts
import { $strict } from "zod/v4/core";
import { $strip } from "zod/v4/core";
import { ZodMiniArray } from "@zod/mini";
import { ZodMiniBoolean } from "@zod/mini";
import { ZodMiniCatch } from "@zod/mini";
import { ZodMiniDefault } from "@zod/mini";
import { ZodMiniDiscriminatedUnion } from "@zod/mini";
import { ZodMiniEnum } from "@zod/mini";
import { ZodMiniInt } from "@zod/mini";
import { ZodMiniLiteral } from "@zod/mini";
import { ZodMiniNever } from "@zod/mini";
import { ZodMiniNumber } from "@zod/mini";
import { ZodMiniObject } from "@zod/mini";
import { ZodMiniOptional } from "@zod/mini";
import { ZodMiniPipe } from "@zod/mini";
import { ZodMiniRecord } from "@zod/mini";
import { ZodMiniString } from "@zod/mini";
import { ZodMiniTransform } from "@zod/mini";
import { ZodMiniUnion } from "@zod/mini";
import { ZodMiniUnknown } from "@zod/mini";
import { infer } from "@zod/mini";
import { output } from "@zod/mini";

export type Algorithm = infer<typeof AlgorithmSchema>;

export const AlgorithmSchema: ZodMiniEnum<{
	ES256: "ES256";
	ES384: "ES384";
	EdDSA: "EdDSA";
	HS256: "HS256";
	HS384: "HS384";
	HS512: "HS512";
	PS256: "PS256";
	PS384: "PS384";
	PS512: "PS512";
	RS256: "RS256";
	RS384: "RS384";
	RS512: "RS512";
}>;

export type AsymmetricKey = Exclude<Key, {
	kty: "oct";
}>;

export type Bytes = infer<typeof BytesSchema>;

export const BytesSchema: ZodMiniObject<{
	received: ZodMiniInt;
	sent: ZodMiniInt;
}, $strip>;

export type Claims = output<typeof ClaimsSchema>;

export const ClaimsSchema: ZodMiniPipe<ZodMiniObject<{
	exp: ZodMiniOptional<ZodMiniInt>;
	get: ZodMiniOptional<ZodMiniUnion<readonly [ZodMiniString<string>, ZodMiniArray<ZodMiniString<string>>]>>;
	iat: ZodMiniOptional<ZodMiniInt>;
	iss: ZodMiniOptional<ZodMiniUnknown>;
	jti: ZodMiniOptional<ZodMiniUnknown>;
	nbf: ZodMiniOptional<ZodMiniInt>;
	publish: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
	put: ZodMiniOptional<ZodMiniUnion<readonly [ZodMiniString<string>, ZodMiniArray<ZodMiniString<string>>]>>;
	root: ZodMiniDefault<ZodMiniString<string>>;
	sub: ZodMiniOptional<ZodMiniUnknown>;
	subscribe: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
}, $strict>, ZodMiniTransform<Omit<{
	exp?: number | undefined;
	get?: string | string[] | undefined;
	iat?: number | undefined;
	nbf?: number | undefined;
	publish?: string[] | undefined;
	put?: string | string[] | undefined;
	root: string;
	subscribe?: string[] | undefined;
}, "get" | "put"> & {
	publish?: string[];
	subscribe?: string[];
}, {
	exp?: number | undefined;
	get?: string | string[] | undefined;
	iat?: number | undefined;
	iss?: unknown;
	jti?: unknown;
	nbf?: number | undefined;
	publish?: string[] | undefined;
	put?: string | string[] | undefined;
	root: string;
	sub?: unknown;
	subscribe?: string[] | undefined;
}>>;

export type Grant = infer<typeof GrantSchema>;

export const GrantSchema: ZodMiniObject<{
	expires: ZodMiniOptional<ZodMiniInt>;
	label: ZodMiniOptional<ZodMiniString<string>>;
	mounts: ZodMiniOptional<ZodMiniRecord<ZodMiniString<string>, ZodMiniString<string>>>;
	peer: ZodMiniOptional<ZodMiniBoolean<boolean>>;
	publish: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
	revalidate: ZodMiniOptional<ZodMiniInt>;
	root: ZodMiniOptional<ZodMiniString<string>>;
	subscribe: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
	tier: ZodMiniOptional<ZodMiniString<string>>;
	upstream: ZodMiniOptional<ZodMiniBoolean<boolean>>;
}, $strip>;

export { InvalidPattern } from "@moq/pattern";

export const Key: {
	generate: (algorithm: Algorithm, kid?: string) => Promise<Key>;
	parse: (jwk: string) => Key;
	public: (key: Key) => PublicKey;
	sign: (key: Key, claims: {
		exp?: number;
		get?: string | string[];
		iat?: number;
		iss?: unknown;
		jti?: unknown;
		nbf?: number;
		publish?: string[];
		put?: string | string[];
		root?: string;
		sub?: unknown;
		subscribe?: string[];
	}) => Promise<string>;
	verify: (key: PublicKey | SymmetricKey, token: string) => Promise<Claims>;
};

export type Key = infer<typeof KeySchema>;

export type KeyId = infer<typeof KeyIdSchema>;

export const KeyIdSchema: ZodMiniString<string>;

export const KeySchema: ZodMiniDiscriminatedUnion<[ZodMiniObject<{
	alg: ZodMiniEnum<{
		ES256: "ES256";
		ES384: "ES384";
		EdDSA: "EdDSA";
		HS256: "HS256";
		HS384: "HS384";
		HS512: "HS512";
		PS256: "PS256";
		PS384: "PS384";
		PS512: "PS512";
		RS256: "RS256";
		RS384: "RS384";
		RS512: "RS512";
	}>;
	k: ZodMiniString<string>;
	key_ops: ZodMiniDefault<ZodMiniArray<ZodMiniEnum<{
		decrypt: "decrypt";
		encrypt: "encrypt";
		sign: "sign";
		verify: "verify";
	}>>>;
	kid: ZodMiniOptional<ZodMiniString<string>>;
	kty: ZodMiniLiteral<"oct">;
	scope: ZodMiniOptional<ZodMiniPipe<ZodMiniObject<{
		get: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
		publish: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
		put: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
		root: ZodMiniDefault<ZodMiniString<string>>;
		subscribe: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
	}, $strict>, ZodMiniTransform<Omit<{
		get?: string[] | undefined;
		publish?: string[] | undefined;
		put?: string[] | undefined;
		root: string;
		subscribe?: string[] | undefined;
	}, "get" | "put"> & {
		publish?: string[];
		subscribe?: string[];
	}, {
		get?: string[] | undefined;
		publish?: string[] | undefined;
		put?: string[] | undefined;
		root: string;
		subscribe?: string[] | undefined;
	}>>>;
}, $strip>, ZodMiniObject<{
	alg: ZodMiniEnum<{
		ES256: "ES256";
		ES384: "ES384";
		EdDSA: "EdDSA";
		HS256: "HS256";
		HS384: "HS384";
		HS512: "HS512";
		PS256: "PS256";
		PS384: "PS384";
		PS512: "PS512";
		RS256: "RS256";
		RS384: "RS384";
		RS512: "RS512";
	}>;
	d: ZodMiniOptional<ZodMiniString<string>>;
	dp: ZodMiniOptional<ZodMiniString<string>>;
	dq: ZodMiniOptional<ZodMiniString<string>>;
	e: ZodMiniString<string>;
	key_ops: ZodMiniDefault<ZodMiniArray<ZodMiniEnum<{
		decrypt: "decrypt";
		encrypt: "encrypt";
		sign: "sign";
		verify: "verify";
	}>>>;
	kid: ZodMiniOptional<ZodMiniString<string>>;
	kty: ZodMiniLiteral<"RSA">;
	n: ZodMiniString<string>;
	oth: ZodMiniOptional<ZodMiniNever>;
	p: ZodMiniOptional<ZodMiniString<string>>;
	q: ZodMiniOptional<ZodMiniString<string>>;
	qi: ZodMiniOptional<ZodMiniString<string>>;
	scope: ZodMiniOptional<ZodMiniPipe<ZodMiniObject<{
		get: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
		publish: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
		put: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
		root: ZodMiniDefault<ZodMiniString<string>>;
		subscribe: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
	}, $strict>, ZodMiniTransform<Omit<{
		get?: string[] | undefined;
		publish?: string[] | undefined;
		put?: string[] | undefined;
		root: string;
		subscribe?: string[] | undefined;
	}, "get" | "put"> & {
		publish?: string[];
		subscribe?: string[];
	}, {
		get?: string[] | undefined;
		publish?: string[] | undefined;
		put?: string[] | undefined;
		root: string;
		subscribe?: string[] | undefined;
	}>>>;
}, $strip>, ZodMiniObject<{
	alg: ZodMiniEnum<{
		ES256: "ES256";
		ES384: "ES384";
		EdDSA: "EdDSA";
		HS256: "HS256";
		HS384: "HS384";
		HS512: "HS512";
		PS256: "PS256";
		PS384: "PS384";
		PS512: "PS512";
		RS256: "RS256";
		RS384: "RS384";
		RS512: "RS512";
	}>;
	crv: ZodMiniEnum<{
		P-256: "P-256";
		P-384: "P-384";
	}>;
	d: ZodMiniOptional<ZodMiniString<string>>;
	key_ops: ZodMiniDefault<ZodMiniArray<ZodMiniEnum<{
		decrypt: "decrypt";
		encrypt: "encrypt";
		sign: "sign";
		verify: "verify";
	}>>>;
	kid: ZodMiniOptional<ZodMiniString<string>>;
	kty: ZodMiniLiteral<"EC">;
	scope: ZodMiniOptional<ZodMiniPipe<ZodMiniObject<{
		get: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
		publish: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
		put: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
		root: ZodMiniDefault<ZodMiniString<string>>;
		subscribe: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
	}, $strict>, ZodMiniTransform<Omit<{
		get?: string[] | undefined;
		publish?: string[] | undefined;
		put?: string[] | undefined;
		root: string;
		subscribe?: string[] | undefined;
	}, "get" | "put"> & {
		publish?: string[];
		subscribe?: string[];
	}, {
		get?: string[] | undefined;
		publish?: string[] | undefined;
		put?: string[] | undefined;
		root: string;
		subscribe?: string[] | undefined;
	}>>>;
	x: ZodMiniString<string>;
	y: ZodMiniString<string>;
}, $strip>, ZodMiniObject<{
	alg: ZodMiniEnum<{
		ES256: "ES256";
		ES384: "ES384";
		EdDSA: "EdDSA";
		HS256: "HS256";
		HS384: "HS384";
		HS512: "HS512";
		PS256: "PS256";
		PS384: "PS384";
		PS512: "PS512";
		RS256: "RS256";
		RS384: "RS384";
		RS512: "RS512";
	}>;
	crv: ZodMiniLiteral<"Ed25519">;
	d: ZodMiniOptional<ZodMiniString<string>>;
	key_ops: ZodMiniDefault<ZodMiniArray<ZodMiniEnum<{
		decrypt: "decrypt";
		encrypt: "encrypt";
		sign: "sign";
		verify: "verify";
	}>>>;
	kid: ZodMiniOptional<ZodMiniString<string>>;
	kty: ZodMiniLiteral<"OKP">;
	scope: ZodMiniOptional<ZodMiniPipe<ZodMiniObject<{
		get: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
		publish: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
		put: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
		root: ZodMiniDefault<ZodMiniString<string>>;
		subscribe: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
	}, $strict>, ZodMiniTransform<Omit<{
		get?: string[] | undefined;
		publish?: string[] | undefined;
		put?: string[] | undefined;
		root: string;
		subscribe?: string[] | undefined;
	}, "get" | "put"> & {
		publish?: string[];
		subscribe?: string[];
	}, {
		get?: string[] | undefined;
		publish?: string[] | undefined;
		put?: string[] | undefined;
		root: string;
		subscribe?: string[] | undefined;
	}>>>;
	x: ZodMiniString<string>;
}, $strip>], "kty">;

export const KeySet: {
	find: (set: KeySet, kid: string) => Key | undefined;
	parse: (jwks: string) => KeySet;
	public: (set: KeySet) => PublicKeySet;
	sign: (set: KeySet, claims: {
		exp?: number;
		get?: string | string[];
		iat?: number;
		iss?: unknown;
		jti?: unknown;
		nbf?: number;
		publish?: string[];
		put?: string | string[];
		root?: string;
		sub?: unknown;
		subscribe?: string[];
	}) => Promise<string>;
	verify: (set: KeySet, token: string) => Promise<Claims>;
};

export type KeySet = infer<typeof KeySetSchema>;

export const KeySetSchema: ZodMiniObject<{
	keys: ZodMiniArray<ZodMiniDiscriminatedUnion<[ZodMiniObject<{
		alg: ZodMiniEnum<{
			ES256: "ES256";
			ES384: "ES384";
			EdDSA: "EdDSA";
			HS256: "HS256";
			HS384: "HS384";
			HS512: "HS512";
			PS256: "PS256";
			PS384: "PS384";
			PS512: "PS512";
			RS256: "RS256";
			RS384: "RS384";
			RS512: "RS512";
		}>;
		k: ZodMiniString<string>;
		key_ops: ZodMiniDefault<ZodMiniArray<ZodMiniEnum<{
			decrypt: "decrypt";
			encrypt: "encrypt";
			sign: "sign";
			verify: "verify";
		}>>>;
		kid: ZodMiniOptional<ZodMiniString<string>>;
		kty: ZodMiniLiteral<"oct">;
		scope: ZodMiniOptional<ZodMiniPipe<ZodMiniObject<{
			get: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
			publish: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
			put: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
			root: ZodMiniDefault<ZodMiniString<string>>;
			subscribe: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
		}, $strict>, ZodMiniTransform<Omit<{
			get?: string[] | undefined;
			publish?: string[] | undefined;
			put?: string[] | undefined;
			root: string;
			subscribe?: string[] | undefined;
		}, "get" | "put"> & {
			publish?: string[];
			subscribe?: string[];
		}, {
			get?: string[] | undefined;
			publish?: string[] | undefined;
			put?: string[] | undefined;
			root: string;
			subscribe?: string[] | undefined;
		}>>>;
	}, $strip>, ZodMiniObject<{
		alg: ZodMiniEnum<{
			ES256: "ES256";
			ES384: "ES384";
			EdDSA: "EdDSA";
			HS256: "HS256";
			HS384: "HS384";
			HS512: "HS512";
			PS256: "PS256";
			PS384: "PS384";
			PS512: "PS512";
			RS256: "RS256";
			RS384: "RS384";
			RS512: "RS512";
		}>;
		d: ZodMiniOptional<ZodMiniString<string>>;
		dp: ZodMiniOptional<ZodMiniString<string>>;
		dq: ZodMiniOptional<ZodMiniString<string>>;
		e: ZodMiniString<string>;
		key_ops: ZodMiniDefault<ZodMiniArray<ZodMiniEnum<{
			decrypt: "decrypt";
			encrypt: "encrypt";
			sign: "sign";
			verify: "verify";
		}>>>;
		kid: ZodMiniOptional<ZodMiniString<string>>;
		kty: ZodMiniLiteral<"RSA">;
		n: ZodMiniString<string>;
		oth: ZodMiniOptional<ZodMiniNever>;
		p: ZodMiniOptional<ZodMiniString<string>>;
		q: ZodMiniOptional<ZodMiniString<string>>;
		qi: ZodMiniOptional<ZodMiniString<string>>;
		scope: ZodMiniOptional<ZodMiniPipe<ZodMiniObject<{
			get: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
			publish: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
			put: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
			root: ZodMiniDefault<ZodMiniString<string>>;
			subscribe: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
		}, $strict>, ZodMiniTransform<Omit<{
			get?: string[] | undefined;
			publish?: string[] | undefined;
			put?: string[] | undefined;
			root: string;
			subscribe?: string[] | undefined;
		}, "get" | "put"> & {
			publish?: string[];
			subscribe?: string[];
		}, {
			get?: string[] | undefined;
			publish?: string[] | undefined;
			put?: string[] | undefined;
			root: string;
			subscribe?: string[] | undefined;
		}>>>;
	}, $strip>, ZodMiniObject<{
		alg: ZodMiniEnum<{
			ES256: "ES256";
			ES384: "ES384";
			EdDSA: "EdDSA";
			HS256: "HS256";
			HS384: "HS384";
			HS512: "HS512";
			PS256: "PS256";
			PS384: "PS384";
			PS512: "PS512";
			RS256: "RS256";
			RS384: "RS384";
			RS512: "RS512";
		}>;
		crv: ZodMiniEnum<{
			P-256: "P-256";
			P-384: "P-384";
		}>;
		d: ZodMiniOptional<ZodMiniString<string>>;
		key_ops: ZodMiniDefault<ZodMiniArray<ZodMiniEnum<{
			decrypt: "decrypt";
			encrypt: "encrypt";
			sign: "sign";
			verify: "verify";
		}>>>;
		kid: ZodMiniOptional<ZodMiniString<string>>;
		kty: ZodMiniLiteral<"EC">;
		scope: ZodMiniOptional<ZodMiniPipe<ZodMiniObject<{
			get: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
			publish: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
			put: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
			root: ZodMiniDefault<ZodMiniString<string>>;
			subscribe: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
		}, $strict>, ZodMiniTransform<Omit<{
			get?: string[] | undefined;
			publish?: string[] | undefined;
			put?: string[] | undefined;
			root: string;
			subscribe?: string[] | undefined;
		}, "get" | "put"> & {
			publish?: string[];
			subscribe?: string[];
		}, {
			get?: string[] | undefined;
			publish?: string[] | undefined;
			put?: string[] | undefined;
			root: string;
			subscribe?: string[] | undefined;
		}>>>;
		x: ZodMiniString<string>;
		y: ZodMiniString<string>;
	}, $strip>, ZodMiniObject<{
		alg: ZodMiniEnum<{
			ES256: "ES256";
			ES384: "ES384";
			EdDSA: "EdDSA";
			HS256: "HS256";
			HS384: "HS384";
			HS512: "HS512";
			PS256: "PS256";
			PS384: "PS384";
			PS512: "PS512";
			RS256: "RS256";
			RS384: "RS384";
			RS512: "RS512";
		}>;
		crv: ZodMiniLiteral<"Ed25519">;
		d: ZodMiniOptional<ZodMiniString<string>>;
		key_ops: ZodMiniDefault<ZodMiniArray<ZodMiniEnum<{
			decrypt: "decrypt";
			encrypt: "encrypt";
			sign: "sign";
			verify: "verify";
		}>>>;
		kid: ZodMiniOptional<ZodMiniString<string>>;
		kty: ZodMiniLiteral<"OKP">;
		scope: ZodMiniOptional<ZodMiniPipe<ZodMiniObject<{
			get: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
			publish: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
			put: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
			root: ZodMiniDefault<ZodMiniString<string>>;
			subscribe: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
		}, $strict>, ZodMiniTransform<Omit<{
			get?: string[] | undefined;
			publish?: string[] | undefined;
			put?: string[] | undefined;
			root: string;
			subscribe?: string[] | undefined;
		}, "get" | "put"> & {
			publish?: string[];
			subscribe?: string[];
		}, {
			get?: string[] | undefined;
			publish?: string[] | undefined;
			put?: string[] | undefined;
			root: string;
			subscribe?: string[] | undefined;
		}>>>;
		x: ZodMiniString<string>;
	}, $strip>], "kty">>;
}, $strip>;

export type Operation = infer<typeof OperationSchema>;

export const OperationSchema: ZodMiniEnum<{
	decrypt: "decrypt";
	encrypt: "encrypt";
	sign: "sign";
	verify: "verify";
}>;

export { Pattern } from "@moq/pattern";

export const PatternListSchema: ZodMiniArray<ZodMiniString<string>>;

export { Patterns } from "@moq/pattern";

export type Peer = infer<typeof PeerSchema>;

export const PeerSchema: ZodMiniObject<{
	expires: ZodMiniOptional<ZodMiniInt>;
	fingerprint: ZodMiniString<string>;
	issuer: ZodMiniString<string>;
	name: ZodMiniString<string>;
}, $strip>;

export interface Permissions {
	publish: Patterns;
	subscribe: Patterns;
}

export type PublicKey = Omit<AsymmetricKey, "d" | "p" | "q" | "dp" | "dq" | "qi">;

export interface PublicKeySet {
	keys: PublicKey[];
}

export type Refusal = infer<typeof RefusalSchema>;

export const RefusalSchema: ZodMiniObject<{
	reason: ZodMiniPipe<ZodMiniString<string>, ZodMiniCatch<ZodMiniEnum<{
		expired: "expired";
		forbidden: "forbidden";
		malformed: "malformed";
		refused: "refused";
	}>>>;
	root: ZodMiniOptional<ZodMiniString<string>>;
	tier: ZodMiniOptional<ZodMiniString<string>>;
}, $strip>;

export type Request = infer<typeof RequestSchema>;

export const RequestSchema: ZodMiniDiscriminatedUnion<[ZodMiniObject<{
	alpn: ZodMiniOptional<ZodMiniString<string>>;
	auth: ZodMiniOptional<ZodMiniString<string>>;
	event: ZodMiniLiteral<"connect">;
	id: ZodMiniString<string>;
	implementation: ZodMiniOptional<ZodMiniString<string>>;
	local: ZodMiniOptional<ZodMiniString<string>>;
	node: ZodMiniString<string>;
	path: ZodMiniString<string>;
	query: ZodMiniOptional<ZodMiniString<string>>;
	remote: ZodMiniOptional<ZodMiniString<string>>;
	role: ZodMiniOptional<ZodMiniEnum<{
		publisher: "publisher";
		subscriber: "subscriber";
	}>>;
	server_name: ZodMiniOptional<ZodMiniString<string>>;
	tls: ZodMiniOptional<ZodMiniObject<{
		expires: ZodMiniOptional<ZodMiniInt>;
		fingerprint: ZodMiniString<string>;
		issuer: ZodMiniString<string>;
		name: ZodMiniString<string>;
	}, $strip>>;
	token: ZodMiniOptional<ZodMiniObject<{
		kind: ZodMiniInt;
		value: ZodMiniString<string>;
	}, $strip>>;
	transport: ZodMiniPipe<ZodMiniString<string>, ZodMiniCatch<ZodMiniEnum<{
		http: "http";
		iroh: "iroh";
		quic: "quic";
		rtmp: "rtmp";
		srt: "srt";
		tcp: "tcp";
		unix: "unix";
		unknown: "unknown";
		webrtc: "webrtc";
		websocket: "websocket";
		webtransport: "webtransport";
	}>>>;
}, $strip>, ZodMiniObject<{
	alpn: ZodMiniOptional<ZodMiniString<string>>;
	auth: ZodMiniString<string>;
	event: ZodMiniLiteral<"attach">;
	id: ZodMiniString<string>;
	implementation: ZodMiniOptional<ZodMiniString<string>>;
	local: ZodMiniOptional<ZodMiniString<string>>;
	node: ZodMiniString<string>;
	path: ZodMiniString<string>;
	query: ZodMiniOptional<ZodMiniString<string>>;
	remote: ZodMiniOptional<ZodMiniString<string>>;
	role: ZodMiniOptional<ZodMiniEnum<{
		publisher: "publisher";
		subscriber: "subscriber";
	}>>;
	server_name: ZodMiniOptional<ZodMiniString<string>>;
	tls: ZodMiniOptional<ZodMiniObject<{
		expires: ZodMiniOptional<ZodMiniInt>;
		fingerprint: ZodMiniString<string>;
		issuer: ZodMiniString<string>;
		name: ZodMiniString<string>;
	}, $strip>>;
	token: ZodMiniOptional<ZodMiniObject<{
		kind: ZodMiniInt;
		value: ZodMiniString<string>;
	}, $strip>>;
	transport: ZodMiniPipe<ZodMiniString<string>, ZodMiniCatch<ZodMiniEnum<{
		http: "http";
		iroh: "iroh";
		quic: "quic";
		rtmp: "rtmp";
		srt: "srt";
		tcp: "tcp";
		unix: "unix";
		unknown: "unknown";
		webrtc: "webrtc";
		websocket: "websocket";
		webtransport: "webtransport";
	}>>>;
}, $strip>, ZodMiniObject<{
	alpn: ZodMiniOptional<ZodMiniString<string>>;
	auth: ZodMiniOptional<ZodMiniString<string>>;
	event: ZodMiniLiteral<"revalidate">;
	id: ZodMiniString<string>;
	implementation: ZodMiniOptional<ZodMiniString<string>>;
	local: ZodMiniOptional<ZodMiniString<string>>;
	node: ZodMiniString<string>;
	path: ZodMiniString<string>;
	query: ZodMiniOptional<ZodMiniString<string>>;
	remote: ZodMiniOptional<ZodMiniString<string>>;
	role: ZodMiniOptional<ZodMiniEnum<{
		publisher: "publisher";
		subscriber: "subscriber";
	}>>;
	server_name: ZodMiniOptional<ZodMiniString<string>>;
	tls: ZodMiniOptional<ZodMiniObject<{
		expires: ZodMiniOptional<ZodMiniInt>;
		fingerprint: ZodMiniString<string>;
		issuer: ZodMiniString<string>;
		name: ZodMiniString<string>;
	}, $strip>>;
	token: ZodMiniOptional<ZodMiniObject<{
		kind: ZodMiniInt;
		value: ZodMiniString<string>;
	}, $strip>>;
	transport: ZodMiniPipe<ZodMiniString<string>, ZodMiniCatch<ZodMiniEnum<{
		http: "http";
		iroh: "iroh";
		quic: "quic";
		rtmp: "rtmp";
		srt: "srt";
		tcp: "tcp";
		unix: "unix";
		unknown: "unknown";
		webrtc: "webrtc";
		websocket: "websocket";
		webtransport: "webtransport";
	}>>>;
}, $strip>, ZodMiniObject<{
	alpn: ZodMiniOptional<ZodMiniString<string>>;
	auth: ZodMiniOptional<ZodMiniString<string>>;
	bytes: ZodMiniObject<{
		received: ZodMiniInt;
		sent: ZodMiniInt;
	}, $strip>;
	duration: ZodMiniNumber<number>;
	event: ZodMiniLiteral<"end">;
	id: ZodMiniString<string>;
	implementation: ZodMiniOptional<ZodMiniString<string>>;
	local: ZodMiniOptional<ZodMiniString<string>>;
	node: ZodMiniString<string>;
	path: ZodMiniString<string>;
	query: ZodMiniOptional<ZodMiniString<string>>;
	reason: ZodMiniString<string>;
	remote: ZodMiniOptional<ZodMiniString<string>>;
	role: ZodMiniOptional<ZodMiniEnum<{
		publisher: "publisher";
		subscriber: "subscriber";
	}>>;
	server_name: ZodMiniOptional<ZodMiniString<string>>;
	tls: ZodMiniOptional<ZodMiniObject<{
		expires: ZodMiniOptional<ZodMiniInt>;
		fingerprint: ZodMiniString<string>;
		issuer: ZodMiniString<string>;
		name: ZodMiniString<string>;
	}, $strip>>;
	token: ZodMiniOptional<ZodMiniObject<{
		kind: ZodMiniInt;
		value: ZodMiniString<string>;
	}, $strip>>;
	transport: ZodMiniPipe<ZodMiniString<string>, ZodMiniCatch<ZodMiniEnum<{
		http: "http";
		iroh: "iroh";
		quic: "quic";
		rtmp: "rtmp";
		srt: "srt";
		tcp: "tcp";
		unix: "unix";
		unknown: "unknown";
		webrtc: "webrtc";
		websocket: "websocket";
		webtransport: "webtransport";
	}>>>;
}, $strip>], "event">;

export type Role = infer<typeof RoleSchema>;

export const RoleSchema: ZodMiniEnum<{
	publisher: "publisher";
	subscriber: "subscriber";
}>;

export type Scope = output<typeof ScopeSchema>;

export const ScopeSchema: ZodMiniPipe<ZodMiniObject<{
	get: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
	publish: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
	put: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
	root: ZodMiniDefault<ZodMiniString<string>>;
	subscribe: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
}, $strict>, ZodMiniTransform<Omit<{
	get?: string[] | undefined;
	publish?: string[] | undefined;
	put?: string[] | undefined;
	root: string;
	subscribe?: string[] | undefined;
}, "get" | "put"> & {
	publish?: string[];
	subscribe?: string[];
}, {
	get?: string[] | undefined;
	publish?: string[] | undefined;
	put?: string[] | undefined;
	root: string;
	subscribe?: string[] | undefined;
}>>;

export { Segment } from "@moq/pattern";

export { Specificity } from "@moq/pattern";

export type SymmetricKey = Extract<Key, {
	kty: "oct";
}>;

export type Token = infer<typeof TokenSchema>;

export const TokenSchema: ZodMiniObject<{
	kind: ZodMiniInt;
	value: ZodMiniString<string>;
}, $strip>;

export type Transport = infer<typeof TransportSchema>;

export const TransportSchema: ZodMiniPipe<ZodMiniString<string>, ZodMiniCatch<ZodMiniEnum<{
	http: "http";
	iroh: "iroh";
	quic: "quic";
	rtmp: "rtmp";
	srt: "srt";
	tcp: "tcp";
	unix: "unix";
	unknown: "unknown";
	webrtc: "webrtc";
	websocket: "websocket";
	webtransport: "webtransport";
}>>>;

export function authorize(claims: Claims, path: string): Permissions;

export { compareSpecificity } from "@moq/pattern";

export function patterns(texts: readonly string[] | undefined): Patterns;

export function scopeAllows(scope: Scope, claims: Claims): boolean;
```
