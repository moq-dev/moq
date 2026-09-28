#!/usr/bin/env node

import { closeSync, fchmodSync, openSync, readFileSync, writeFileSync } from "node:fs";
import * as base64 from "@hexagon/base64";
import { Command, Option } from "commander";
import type { Algorithm } from "./algorithm.ts";
import { authorize, type Claims, type Scope, ScopeSchema } from "./claims.ts";
import { Key } from "./key.ts";
import { encodeGrants } from "./wire.ts";

const program = new Command();

program.name("moq-auth").description("Generate, sign, and verify tokens for moq-relay").version("0.1.0");

program
	.command("generate")
	.description("Generate a new signing key")
	.requiredOption("--out <path>", "Path to save the key")
	.option("--algorithm <algorithm>", "Algorithm to use", "HS256")
	.option("--id <id>", "Key ID (randomly generated if not provided)")
	.option("--public <path>", "Path to save the public key (for asymmetric algorithms)")
	.option("--base64", "Output as base64url instead of JSON", false)
	.option("--root <root>", "Root path for the optional key scope", "")
	.option("--publish <pattern...>", "Publish patterns the key may grant (foo/** for a subtree)")
	.option("--subscribe <pattern...>", "Subscribe patterns the key may grant (foo/** for a subtree)")
	.action(async (options) => {
		try {
			const algorithm = options.algorithm as Algorithm;
			let key = await Key.generate(algorithm, options.id);
			if (options.publish || options.subscribe) {
				// Parse rather than cast, so a useless scope fails here like it does
				// in the Rust CLI instead of writing an unusable key to disk.
				const scope: Scope = ScopeSchema.parse({
					root: options.root,
					...(options.publish && { publish: options.publish }),
					...(options.subscribe && { subscribe: options.subscribe }),
				});
				key = { ...key, scope };
			}

			const encodeKey = (k: { scope?: Scope }): string => {
				// Written the legacy way when that says the same thing, so older readers load it.
				const json = JSON.stringify(k.scope ? { ...k, scope: encodeGrants(k.scope) } : k, null, 2);
				if (options.base64) {
					return base64.fromArrayBuffer(new TextEncoder().encode(json).buffer, true);
				}
				return json;
			};

			writePrivateFileSync(options.out, encodeKey(key));
			console.log(`Generated ${algorithm} key: ${options.out}`);

			if (options.public && key.kty !== "oct") {
				const publicKey = Key.public(key);
				writeFileSync(options.public, encodeKey(publicKey), "utf-8");
				console.log(`Generated public key: ${options.public}`);
			} else if (options.public && key.kty === "oct") {
				console.error("Warning: Cannot save public key for symmetric (oct) algorithm");
			}
		} catch (error) {
			console.error("Error generating key:", error instanceof Error ? error.message : error);
			process.exit(1);
		}
	});

program
	.command("sign")
	.description("Sign a token to stdout")
	.requiredOption("--key <path>", "Path to the key file")
	.option("--root <root>", "Root path for the token", "")
	.option("--publish <pattern...>", "Patterns the holder may publish (foo/** for a subtree)")
	.option("--subscribe <pattern...>", "Patterns the holder may subscribe to (foo/** for a subtree)")
	.option("--expires <timestamp>", "Expiration time as unix timestamp", parseUnixTimestamp)
	.option("--issued <timestamp>", "Issued time as unix timestamp", parseUnixTimestamp)
	.action(async (options) => {
		try {
			const keyEncoded = readFileSync(options.key, "utf-8");
			const key = Key.parse(keyEncoded);

			const claims: Claims = {
				root: options.root,
				...(options.publish && { publish: options.publish }),
				...(options.subscribe && { subscribe: options.subscribe }),
				...(options.expires && { exp: options.expires }),
				...(options.issued && { iat: options.issued }),
			};

			const token = await Key.sign(key, claims);
			console.log(token);
		} catch (error) {
			console.error("Error signing token:", error instanceof Error ? error.message : error);
			process.exit(1);
		}
	});

program
	.command("verify")
	.description("Verify a token, writing the payload to stdout")
	.requiredOption("--key <path>", "Path to the key file")
	.option("--in <path>", "Path to read the token from. Use - for stdin.", "-")
	.addOption(new Option("--root <root>", "Path to authorize the token against").hideHelp())
	.action(async (options) => {
		try {
			const parsed = Key.parse(readFileSync(options.key, "utf-8"));
			// EdDSA (and WebCrypto) cannot verify with a JWK that still carries `d`.
			const key = parsed.kty === "oct" ? parsed : Key.public(parsed);

			const token = readFileSync(options.in === "-" ? 0 : options.in, "utf-8").trim();

			const claims = await Key.verify(key, token);
			if (options.root !== undefined) {
				authorize(claims, options.root);
			}
			console.log(JSON.stringify(claims, null, 2));
		} catch (error) {
			console.error("Error verifying token:", error instanceof Error ? error.message : error);
			process.exit(1);
		}
	});

/**
 * Write a file holding private key material, restricted to the owner on Unix.
 *
 * The `mode` on open(2) only applies when the file is created, and is masked by the umask either
 * way. Chmod the handle before writing so overwriting an existing world-readable key tightens it
 * and the secret never sits in a readable file.
 */
function writePrivateFileSync(path: string, contents: string) {
	const fd = openSync(path, "w", 0o600);
	try {
		// Windows has no equivalent of the mode bits, so the file inherits the directory's ACL.
		if (process.platform !== "win32") {
			fchmodSync(fd, 0o600);
		}
		// writeFileSync loops until every byte lands, unlike writeSync's single short-write-prone call.
		writeFileSync(fd, contents);
	} finally {
		closeSync(fd);
	}
}

function parseUnixTimestamp(value: string): number {
	const timestamp = Number.parseInt(value, 10);
	if (Number.isNaN(timestamp)) {
		throw new Error("Expected unix timestamp");
	}
	return timestamp;
}

program.parse();
