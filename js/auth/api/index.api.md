# `@moq/auth`

```ts
import { $strict } from "zod/v4/core";
import { $strip } from "zod/v4/core";
import { ZodMiniArray } from "zod/mini";
import { ZodMiniBoolean } from "zod/mini";
import { ZodMiniCatch } from "zod/mini";
import { ZodMiniDefault } from "zod/mini";
import { ZodMiniDiscriminatedUnion } from "zod/mini";
import { ZodMiniEnum } from "zod/mini";
import { ZodMiniInt } from "zod/mini";
import { ZodMiniLiteral } from "zod/mini";
import { ZodMiniNever } from "zod/mini";
import { ZodMiniNumber } from "zod/mini";
import { ZodMiniObject } from "zod/mini";
import { ZodMiniOptional } from "zod/mini";
import { ZodMiniPipe } from "zod/mini";
import { ZodMiniRecord } from "zod/mini";
import { ZodMiniString } from "zod/mini";
import { ZodMiniTransform } from "zod/mini";
import { ZodMiniUnion } from "zod/mini";
import { ZodMiniUnknown } from "zod/mini";
import { infer } from "zod/mini";
import { output } from "zod/mini";

export type Access = output<typeof AccessSchema>;

export const AccessSchema: ZodMiniUnion<readonly [ZodMiniArray<ZodMiniString<string>>, ZodMiniObject<{
	path: ZodMiniArray<ZodMiniString<string>>;
	rate: ZodMiniNumber<number>;
}, $strict>]>;

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

export const Claims: {
	authorize: (claims: Claims, path: string) => Permissions;
};

export type Claims = output<typeof ClaimsFieldsSchema>;

export const ClaimsFieldsSchema: ZodMiniObject<{
	expires: ZodMiniOptional<ZodMiniInt>;
	id: ZodMiniOptional<ZodMiniString<string>>;
	issued: ZodMiniOptional<ZodMiniInt>;
	notBefore: ZodMiniOptional<ZodMiniInt>;
	publish: ZodMiniOptional<ZodMiniUnion<readonly [ZodMiniArray<ZodMiniString<string>>, ZodMiniObject<{
		path: ZodMiniArray<ZodMiniString<string>>;
		rate: ZodMiniNumber<number>;
	}, $strict>]>>;
	root: ZodMiniDefault<ZodMiniString<string>>;
	subscribe: ZodMiniOptional<ZodMiniUnion<readonly [ZodMiniArray<ZodMiniString<string>>, ZodMiniObject<{
		path: ZodMiniArray<ZodMiniString<string>>;
		rate: ZodMiniNumber<number>;
	}, $strict>]>>;
}, $strict>;

export const ClaimsSchema: ZodMiniPipe<ZodMiniPipe<ZodMiniObject<{
	exp: ZodMiniOptional<ZodMiniInt>;
	get: ZodMiniOptional<ZodMiniUnion<readonly [ZodMiniString<string>, ZodMiniArray<ZodMiniString<string>>]>>;
	iat: ZodMiniOptional<ZodMiniInt>;
	iss: ZodMiniOptional<ZodMiniUnknown>;
	jti: ZodMiniOptional<ZodMiniString<string>>;
	nbf: ZodMiniOptional<ZodMiniInt>;
	publish: ZodMiniOptional<ZodMiniUnion<readonly [ZodMiniArray<ZodMiniPipe<ZodMiniString<string>, ZodMiniTransform<string, string>>>, ZodMiniObject<{
		path: ZodMiniArray<ZodMiniPipe<ZodMiniString<string>, ZodMiniTransform<string, string>>>;
		rate: ZodMiniNumber<number>;
	}, $strict>]>>;
	put: ZodMiniOptional<ZodMiniUnion<readonly [ZodMiniString<string>, ZodMiniArray<ZodMiniString<string>>]>>;
	root: ZodMiniOptional<ZodMiniString<string>>;
	sub: ZodMiniOptional<ZodMiniUnknown>;
	subscribe: ZodMiniOptional<ZodMiniUnion<readonly [ZodMiniArray<ZodMiniPipe<ZodMiniString<string>, ZodMiniTransform<string, string>>>, ZodMiniObject<{
		path: ZodMiniArray<ZodMiniPipe<ZodMiniString<string>, ZodMiniTransform<string, string>>>;
		rate: ZodMiniNumber<number>;
	}, $strict>]>>;
}, $strict>, ZodMiniTransform<{
	expires?: number | undefined;
	id?: string | undefined;
	issued?: number | undefined;
	notBefore?: number | undefined;
	publish?: string[] | {
		path: string[];
		rate: number;
	} | undefined;
	root?: string | undefined;
	subscribe?: string[] | {
		path: string[];
		rate: number;
	} | undefined;
}, {
	exp?: number | undefined;
	get?: string | string[] | undefined;
	iat?: number | undefined;
	iss?: unknown;
	jti?: string | undefined;
	nbf?: number | undefined;
	publish?: string[] | {
		path: string[];
		rate: number;
	} | undefined;
	put?: string | string[] | undefined;
	root?: string | undefined;
	sub?: unknown;
	subscribe?: string[] | {
		path: string[];
		rate: number;
	} | undefined;
}>>, ZodMiniObject<{
	expires: ZodMiniOptional<ZodMiniInt>;
	id: ZodMiniOptional<ZodMiniString<string>>;
	issued: ZodMiniOptional<ZodMiniInt>;
	notBefore: ZodMiniOptional<ZodMiniInt>;
	publish: ZodMiniOptional<ZodMiniUnion<readonly [ZodMiniArray<ZodMiniString<string>>, ZodMiniObject<{
		path: ZodMiniArray<ZodMiniString<string>>;
		rate: ZodMiniNumber<number>;
	}, $strict>]>>;
	root: ZodMiniDefault<ZodMiniString<string>>;
	subscribe: ZodMiniOptional<ZodMiniUnion<readonly [ZodMiniArray<ZodMiniString<string>>, ZodMiniObject<{
		path: ZodMiniArray<ZodMiniString<string>>;
		rate: ZodMiniNumber<number>;
	}, $strict>]>>;
}, $strict>>;

export type Grant = infer<typeof GrantSchema>;

export const GrantSchema: ZodMiniObject<{
	expires: ZodMiniOptional<ZodMiniInt>;
	label: ZodMiniOptional<ZodMiniString<string>>;
	link: ZodMiniOptional<ZodMiniEnum<{
		peer: "peer";
		upstream: "upstream";
	}>>;
	mounts: ZodMiniOptional<ZodMiniRecord<ZodMiniString<string>, ZodMiniString<string>>>;
	peer: ZodMiniOptional<ZodMiniBoolean<boolean>>;
	publish: ZodMiniOptional<ZodMiniPipe<ZodMiniUnion<readonly [ZodMiniArray<ZodMiniPipe<ZodMiniString<string>, ZodMiniTransform<string, string>>>, ZodMiniObject<{
		path: ZodMiniArray<ZodMiniPipe<ZodMiniString<string>, ZodMiniTransform<string, string>>>;
		rate: ZodMiniNumber<number>;
	}, $strict>]>, ZodMiniTransform<string[] | {
		path: string[];
		rate: number;
	}, string[] | {
		path: string[];
		rate: number;
	}>>>;
	revalidate: ZodMiniOptional<ZodMiniInt>;
	root: ZodMiniOptional<ZodMiniString<string>>;
	subscribe: ZodMiniOptional<ZodMiniPipe<ZodMiniUnion<readonly [ZodMiniArray<ZodMiniPipe<ZodMiniString<string>, ZodMiniTransform<string, string>>>, ZodMiniObject<{
		path: ZodMiniArray<ZodMiniPipe<ZodMiniString<string>, ZodMiniTransform<string, string>>>;
		rate: ZodMiniNumber<number>;
	}, $strict>]>, ZodMiniTransform<string[] | {
		path: string[];
		rate: number;
	}, string[] | {
		path: string[];
		rate: number;
	}>>>;
	tier: ZodMiniOptional<ZodMiniString<string>>;
}, $strict>;

export { InvalidPrefix } from "@moq/net/prefix";

export const Key: {
	generate: (algorithm: Algorithm, kid?: string) => Promise<Key>;
	parse: (jwk: string) => Key;
	public: (key: Key) => PublicKey;
	sign: (key: Key, claims: Claims) => Promise<string>;
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
		sign: "sign";
		verify: "verify";
	}>>>;
	kid: ZodMiniOptional<ZodMiniString<string>>;
	kty: ZodMiniLiteral<"oct">;
	scope: ZodMiniOptional<ZodMiniPipe<ZodMiniPipe<ZodMiniObject<{
		get: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
		publish: ZodMiniOptional<ZodMiniArray<ZodMiniPipe<ZodMiniString<string>, ZodMiniTransform<string, string>>>>;
		put: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
		root: ZodMiniDefault<ZodMiniString<string>>;
		subscribe: ZodMiniOptional<ZodMiniArray<ZodMiniPipe<ZodMiniString<string>, ZodMiniTransform<string, string>>>>;
	}, $strict>, ZodMiniTransform<Omit<{
		get?: string[] | undefined;
		publish?: string[] | undefined;
		put?: string[] | undefined;
		root: string;
		subscribe?: string[] | undefined;
	}, "get" | "put">, {
		get?: string[] | undefined;
		publish?: string[] | undefined;
		put?: string[] | undefined;
		root: string;
		subscribe?: string[] | undefined;
	}>>, ZodMiniTransform<Omit<{
		get?: string[] | undefined;
		publish?: string[] | undefined;
		put?: string[] | undefined;
		root: string;
		subscribe?: string[] | undefined;
	}, "get" | "put">, Omit<{
		get?: string[] | undefined;
		publish?: string[] | undefined;
		put?: string[] | undefined;
		root: string;
		subscribe?: string[] | undefined;
	}, "get" | "put">>>>;
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
	scope: ZodMiniOptional<ZodMiniPipe<ZodMiniPipe<ZodMiniObject<{
		get: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
		publish: ZodMiniOptional<ZodMiniArray<ZodMiniPipe<ZodMiniString<string>, ZodMiniTransform<string, string>>>>;
		put: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
		root: ZodMiniDefault<ZodMiniString<string>>;
		subscribe: ZodMiniOptional<ZodMiniArray<ZodMiniPipe<ZodMiniString<string>, ZodMiniTransform<string, string>>>>;
	}, $strict>, ZodMiniTransform<Omit<{
		get?: string[] | undefined;
		publish?: string[] | undefined;
		put?: string[] | undefined;
		root: string;
		subscribe?: string[] | undefined;
	}, "get" | "put">, {
		get?: string[] | undefined;
		publish?: string[] | undefined;
		put?: string[] | undefined;
		root: string;
		subscribe?: string[] | undefined;
	}>>, ZodMiniTransform<Omit<{
		get?: string[] | undefined;
		publish?: string[] | undefined;
		put?: string[] | undefined;
		root: string;
		subscribe?: string[] | undefined;
	}, "get" | "put">, Omit<{
		get?: string[] | undefined;
		publish?: string[] | undefined;
		put?: string[] | undefined;
		root: string;
		subscribe?: string[] | undefined;
	}, "get" | "put">>>>;
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
		sign: "sign";
		verify: "verify";
	}>>>;
	kid: ZodMiniOptional<ZodMiniString<string>>;
	kty: ZodMiniLiteral<"EC">;
	scope: ZodMiniOptional<ZodMiniPipe<ZodMiniPipe<ZodMiniObject<{
		get: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
		publish: ZodMiniOptional<ZodMiniArray<ZodMiniPipe<ZodMiniString<string>, ZodMiniTransform<string, string>>>>;
		put: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
		root: ZodMiniDefault<ZodMiniString<string>>;
		subscribe: ZodMiniOptional<ZodMiniArray<ZodMiniPipe<ZodMiniString<string>, ZodMiniTransform<string, string>>>>;
	}, $strict>, ZodMiniTransform<Omit<{
		get?: string[] | undefined;
		publish?: string[] | undefined;
		put?: string[] | undefined;
		root: string;
		subscribe?: string[] | undefined;
	}, "get" | "put">, {
		get?: string[] | undefined;
		publish?: string[] | undefined;
		put?: string[] | undefined;
		root: string;
		subscribe?: string[] | undefined;
	}>>, ZodMiniTransform<Omit<{
		get?: string[] | undefined;
		publish?: string[] | undefined;
		put?: string[] | undefined;
		root: string;
		subscribe?: string[] | undefined;
	}, "get" | "put">, Omit<{
		get?: string[] | undefined;
		publish?: string[] | undefined;
		put?: string[] | undefined;
		root: string;
		subscribe?: string[] | undefined;
	}, "get" | "put">>>>;
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
		sign: "sign";
		verify: "verify";
	}>>>;
	kid: ZodMiniOptional<ZodMiniString<string>>;
	kty: ZodMiniLiteral<"OKP">;
	scope: ZodMiniOptional<ZodMiniPipe<ZodMiniPipe<ZodMiniObject<{
		get: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
		publish: ZodMiniOptional<ZodMiniArray<ZodMiniPipe<ZodMiniString<string>, ZodMiniTransform<string, string>>>>;
		put: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
		root: ZodMiniDefault<ZodMiniString<string>>;
		subscribe: ZodMiniOptional<ZodMiniArray<ZodMiniPipe<ZodMiniString<string>, ZodMiniTransform<string, string>>>>;
	}, $strict>, ZodMiniTransform<Omit<{
		get?: string[] | undefined;
		publish?: string[] | undefined;
		put?: string[] | undefined;
		root: string;
		subscribe?: string[] | undefined;
	}, "get" | "put">, {
		get?: string[] | undefined;
		publish?: string[] | undefined;
		put?: string[] | undefined;
		root: string;
		subscribe?: string[] | undefined;
	}>>, ZodMiniTransform<Omit<{
		get?: string[] | undefined;
		publish?: string[] | undefined;
		put?: string[] | undefined;
		root: string;
		subscribe?: string[] | undefined;
	}, "get" | "put">, Omit<{
		get?: string[] | undefined;
		publish?: string[] | undefined;
		put?: string[] | undefined;
		root: string;
		subscribe?: string[] | undefined;
	}, "get" | "put">>>>;
	x: ZodMiniString<string>;
}, $strip>], "kty">;

export const KeySet: {
	find: (set: KeySet, kid: string) => Key | undefined;
	parse: (jwks: string) => KeySet;
	public: (set: KeySet) => PublicKeySet;
	sign: (set: KeySet, claims: Claims) => Promise<string>;
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
			sign: "sign";
			verify: "verify";
		}>>>;
		kid: ZodMiniOptional<ZodMiniString<string>>;
		kty: ZodMiniLiteral<"oct">;
		scope: ZodMiniOptional<ZodMiniPipe<ZodMiniPipe<ZodMiniObject<{
			get: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
			publish: ZodMiniOptional<ZodMiniArray<ZodMiniPipe<ZodMiniString<string>, ZodMiniTransform<string, string>>>>;
			put: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
			root: ZodMiniDefault<ZodMiniString<string>>;
			subscribe: ZodMiniOptional<ZodMiniArray<ZodMiniPipe<ZodMiniString<string>, ZodMiniTransform<string, string>>>>;
		}, $strict>, ZodMiniTransform<Omit<{
			get?: string[] | undefined;
			publish?: string[] | undefined;
			put?: string[] | undefined;
			root: string;
			subscribe?: string[] | undefined;
		}, "get" | "put">, {
			get?: string[] | undefined;
			publish?: string[] | undefined;
			put?: string[] | undefined;
			root: string;
			subscribe?: string[] | undefined;
		}>>, ZodMiniTransform<Omit<{
			get?: string[] | undefined;
			publish?: string[] | undefined;
			put?: string[] | undefined;
			root: string;
			subscribe?: string[] | undefined;
		}, "get" | "put">, Omit<{
			get?: string[] | undefined;
			publish?: string[] | undefined;
			put?: string[] | undefined;
			root: string;
			subscribe?: string[] | undefined;
		}, "get" | "put">>>>;
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
		scope: ZodMiniOptional<ZodMiniPipe<ZodMiniPipe<ZodMiniObject<{
			get: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
			publish: ZodMiniOptional<ZodMiniArray<ZodMiniPipe<ZodMiniString<string>, ZodMiniTransform<string, string>>>>;
			put: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
			root: ZodMiniDefault<ZodMiniString<string>>;
			subscribe: ZodMiniOptional<ZodMiniArray<ZodMiniPipe<ZodMiniString<string>, ZodMiniTransform<string, string>>>>;
		}, $strict>, ZodMiniTransform<Omit<{
			get?: string[] | undefined;
			publish?: string[] | undefined;
			put?: string[] | undefined;
			root: string;
			subscribe?: string[] | undefined;
		}, "get" | "put">, {
			get?: string[] | undefined;
			publish?: string[] | undefined;
			put?: string[] | undefined;
			root: string;
			subscribe?: string[] | undefined;
		}>>, ZodMiniTransform<Omit<{
			get?: string[] | undefined;
			publish?: string[] | undefined;
			put?: string[] | undefined;
			root: string;
			subscribe?: string[] | undefined;
		}, "get" | "put">, Omit<{
			get?: string[] | undefined;
			publish?: string[] | undefined;
			put?: string[] | undefined;
			root: string;
			subscribe?: string[] | undefined;
		}, "get" | "put">>>>;
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
			sign: "sign";
			verify: "verify";
		}>>>;
		kid: ZodMiniOptional<ZodMiniString<string>>;
		kty: ZodMiniLiteral<"EC">;
		scope: ZodMiniOptional<ZodMiniPipe<ZodMiniPipe<ZodMiniObject<{
			get: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
			publish: ZodMiniOptional<ZodMiniArray<ZodMiniPipe<ZodMiniString<string>, ZodMiniTransform<string, string>>>>;
			put: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
			root: ZodMiniDefault<ZodMiniString<string>>;
			subscribe: ZodMiniOptional<ZodMiniArray<ZodMiniPipe<ZodMiniString<string>, ZodMiniTransform<string, string>>>>;
		}, $strict>, ZodMiniTransform<Omit<{
			get?: string[] | undefined;
			publish?: string[] | undefined;
			put?: string[] | undefined;
			root: string;
			subscribe?: string[] | undefined;
		}, "get" | "put">, {
			get?: string[] | undefined;
			publish?: string[] | undefined;
			put?: string[] | undefined;
			root: string;
			subscribe?: string[] | undefined;
		}>>, ZodMiniTransform<Omit<{
			get?: string[] | undefined;
			publish?: string[] | undefined;
			put?: string[] | undefined;
			root: string;
			subscribe?: string[] | undefined;
		}, "get" | "put">, Omit<{
			get?: string[] | undefined;
			publish?: string[] | undefined;
			put?: string[] | undefined;
			root: string;
			subscribe?: string[] | undefined;
		}, "get" | "put">>>>;
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
			sign: "sign";
			verify: "verify";
		}>>>;
		kid: ZodMiniOptional<ZodMiniString<string>>;
		kty: ZodMiniLiteral<"OKP">;
		scope: ZodMiniOptional<ZodMiniPipe<ZodMiniPipe<ZodMiniObject<{
			get: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
			publish: ZodMiniOptional<ZodMiniArray<ZodMiniPipe<ZodMiniString<string>, ZodMiniTransform<string, string>>>>;
			put: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
			root: ZodMiniDefault<ZodMiniString<string>>;
			subscribe: ZodMiniOptional<ZodMiniArray<ZodMiniPipe<ZodMiniString<string>, ZodMiniTransform<string, string>>>>;
		}, $strict>, ZodMiniTransform<Omit<{
			get?: string[] | undefined;
			publish?: string[] | undefined;
			put?: string[] | undefined;
			root: string;
			subscribe?: string[] | undefined;
		}, "get" | "put">, {
			get?: string[] | undefined;
			publish?: string[] | undefined;
			put?: string[] | undefined;
			root: string;
			subscribe?: string[] | undefined;
		}>>, ZodMiniTransform<Omit<{
			get?: string[] | undefined;
			publish?: string[] | undefined;
			put?: string[] | undefined;
			root: string;
			subscribe?: string[] | undefined;
		}, "get" | "put">, Omit<{
			get?: string[] | undefined;
			publish?: string[] | undefined;
			put?: string[] | undefined;
			root: string;
			subscribe?: string[] | undefined;
		}, "get" | "put">>>>;
		x: ZodMiniString<string>;
	}, $strip>], "kty">>;
}, $strip>;

export type Link = infer<typeof LinkSchema>;

export const LinkSchema: ZodMiniEnum<{
	peer: "peer";
	upstream: "upstream";
}>;

export type Operation = infer<typeof OperationSchema>;

export const OperationSchema: ZodMiniEnum<{
	sign: "sign";
	verify: "verify";
}>;

export type Peer = infer<typeof PeerSchema>;

export const PeerSchema: ZodMiniObject<{
	chain: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
	expires: ZodMiniOptional<ZodMiniInt>;
	fingerprint: ZodMiniString<string>;
	issuer: ZodMiniString<string>;
	name: ZodMiniString<string>;
}, $strip>;

export interface Permissions {
	publish: Prefixes;
	subscribe: Prefixes;
}

export { Prefix } from "@moq/net/prefix";

export const PrefixListSchema: ZodMiniArray<ZodMiniString<string>>;

export { Prefixes } from "@moq/net/prefix";

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
	host: ZodMiniOptional<ZodMiniString<string>>;
	id: ZodMiniString<string>;
	implementation: ZodMiniOptional<ZodMiniString<string>>;
	local: ZodMiniOptional<ZodMiniString<string>>;
	node: ZodMiniString<string>;
	path: ZodMiniString<string>;
	query: ZodMiniOptional<ZodMiniString<string>>;
	relay: ZodMiniOptional<ZodMiniString<string>>;
	remote: ZodMiniOptional<ZodMiniString<string>>;
	role: ZodMiniOptional<ZodMiniEnum<{
		publisher: "publisher";
		subscriber: "subscriber";
	}>>;
	server_name: ZodMiniOptional<ZodMiniString<string>>;
	tls: ZodMiniOptional<ZodMiniObject<{
		chain: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
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
		quic: "quic";
		rtmp: "rtmp";
		srt: "srt";
		tcp: "tcp";
		tls: "tls";
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
	host: ZodMiniOptional<ZodMiniString<string>>;
	id: ZodMiniString<string>;
	implementation: ZodMiniOptional<ZodMiniString<string>>;
	local: ZodMiniOptional<ZodMiniString<string>>;
	node: ZodMiniString<string>;
	path: ZodMiniString<string>;
	query: ZodMiniOptional<ZodMiniString<string>>;
	relay: ZodMiniOptional<ZodMiniString<string>>;
	remote: ZodMiniOptional<ZodMiniString<string>>;
	role: ZodMiniOptional<ZodMiniEnum<{
		publisher: "publisher";
		subscriber: "subscriber";
	}>>;
	server_name: ZodMiniOptional<ZodMiniString<string>>;
	tls: ZodMiniOptional<ZodMiniObject<{
		chain: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
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
		quic: "quic";
		rtmp: "rtmp";
		srt: "srt";
		tcp: "tcp";
		tls: "tls";
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
	host: ZodMiniOptional<ZodMiniString<string>>;
	id: ZodMiniString<string>;
	implementation: ZodMiniOptional<ZodMiniString<string>>;
	local: ZodMiniOptional<ZodMiniString<string>>;
	node: ZodMiniString<string>;
	path: ZodMiniString<string>;
	query: ZodMiniOptional<ZodMiniString<string>>;
	relay: ZodMiniOptional<ZodMiniString<string>>;
	remote: ZodMiniOptional<ZodMiniString<string>>;
	role: ZodMiniOptional<ZodMiniEnum<{
		publisher: "publisher";
		subscriber: "subscriber";
	}>>;
	server_name: ZodMiniOptional<ZodMiniString<string>>;
	tls: ZodMiniOptional<ZodMiniObject<{
		chain: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
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
		quic: "quic";
		rtmp: "rtmp";
		srt: "srt";
		tcp: "tcp";
		tls: "tls";
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
	host: ZodMiniOptional<ZodMiniString<string>>;
	id: ZodMiniString<string>;
	implementation: ZodMiniOptional<ZodMiniString<string>>;
	local: ZodMiniOptional<ZodMiniString<string>>;
	node: ZodMiniString<string>;
	path: ZodMiniString<string>;
	query: ZodMiniOptional<ZodMiniString<string>>;
	reason: ZodMiniString<string>;
	relay: ZodMiniOptional<ZodMiniString<string>>;
	remote: ZodMiniOptional<ZodMiniString<string>>;
	role: ZodMiniOptional<ZodMiniEnum<{
		publisher: "publisher";
		subscriber: "subscriber";
	}>>;
	server_name: ZodMiniOptional<ZodMiniString<string>>;
	tls: ZodMiniOptional<ZodMiniObject<{
		chain: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
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
		quic: "quic";
		rtmp: "rtmp";
		srt: "srt";
		tcp: "tcp";
		tls: "tls";
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

export const Scope: {
	allows: (scope: Scope, claims: Claims) => boolean;
};

export type Scope = output<typeof ScopeSchema>;

export const ScopeSchema: ZodMiniPipe<ZodMiniPipe<ZodMiniObject<{
	get: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
	publish: ZodMiniOptional<ZodMiniArray<ZodMiniPipe<ZodMiniString<string>, ZodMiniTransform<string, string>>>>;
	put: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
	root: ZodMiniDefault<ZodMiniString<string>>;
	subscribe: ZodMiniOptional<ZodMiniArray<ZodMiniPipe<ZodMiniString<string>, ZodMiniTransform<string, string>>>>;
}, $strict>, ZodMiniTransform<Omit<{
	get?: string[] | undefined;
	publish?: string[] | undefined;
	put?: string[] | undefined;
	root: string;
	subscribe?: string[] | undefined;
}, "get" | "put">, {
	get?: string[] | undefined;
	publish?: string[] | undefined;
	put?: string[] | undefined;
	root: string;
	subscribe?: string[] | undefined;
}>>, ZodMiniTransform<Omit<{
	get?: string[] | undefined;
	publish?: string[] | undefined;
	put?: string[] | undefined;
	root: string;
	subscribe?: string[] | undefined;
}, "get" | "put">, Omit<{
	get?: string[] | undefined;
	publish?: string[] | undefined;
	put?: string[] | undefined;
	root: string;
	subscribe?: string[] | undefined;
}, "get" | "put">>>;

export type SymmetricKey = Extract<Key, {
	kty: "oct";
}>;

export type Token = infer<typeof TokenSchema>;

export type TokenId = output<typeof TokenIdSchema>;

export const TokenIdSchema: ZodMiniString<string>;

export const TokenSchema: ZodMiniObject<{
	kind: ZodMiniInt;
	value: ZodMiniString<string>;
}, $strip>;

export type Transport = infer<typeof TransportSchema>;

export const TransportSchema: ZodMiniPipe<ZodMiniString<string>, ZodMiniCatch<ZodMiniEnum<{
	http: "http";
	quic: "quic";
	rtmp: "rtmp";
	srt: "srt";
	tcp: "tcp";
	tls: "tls";
	unix: "unix";
	unknown: "unknown";
	webrtc: "webrtc";
	websocket: "websocket";
	webtransport: "webtransport";
}>>>;

export function prefixes(texts: readonly string[] | undefined): Prefixes;
```
