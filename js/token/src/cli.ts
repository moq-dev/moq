#!/usr/bin/env node

import { readFileSync, writeFileSync } from "node:fs";
import * as base64 from "@hexagon/base64";
import { Command } from "commander";
import type { Algorithm } from "./algorithm.ts";
import type { Claims } from "./claims.ts";
import { generate } from "./generate.ts";
import type { Key, PublicKey } from "./key.ts";
import { load, loadPublic, sign, toPublicKey, verify } from "./key.ts";

const program = new Command();

program.name("moq-token").description("Generate, sign, and verify tokens for moq-relay").version("0.1.1");

program
	.command("generate")
	.description("Generate a new signing key")
	.requiredOption("--key <path>", "Path to save the key")
	.option("--algorithm <algorithm>", "Algorithm to use", "HS256")
	.option("--id <id>", "Key ID (randomly generated if not provided)")
	.option("--public <path>", "Path to save the public key (for asymmetric algorithms)")
	.option("--base64", "Output as base64url instead of JSON", false)
	.action(async (options) => {
		try {
			const algorithm = options.algorithm as Algorithm;
			const key = await generate(algorithm, options.id);

			const encodeKey = (k: object): string => {
				const json = JSON.stringify(k, null, 2);
				if (options.base64) {
					return base64.fromArrayBuffer(new TextEncoder().encode(json).buffer, true);
				}
				return json;
			};

			writeFileSync(options.key, encodeKey(key), "utf-8");
			console.log(`Generated ${algorithm} key: ${options.key}`);

			if (options.public && key.kty !== "oct") {
				const publicKey = toPublicKey(key);
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
	.option("--publish <path...>", "Publish permission patterns (can be specified multiple times)")
	.option("--subscribe <path...>", "Subscribe permission patterns (can be specified multiple times)")
	.option("--cluster", "Whether this is a cluster node", false)
	.option("--expires <timestamp>", "Expiration time as unix timestamp", parseUnixTimestamp)
	.option("--issued <timestamp>", "Issued time as unix timestamp", parseUnixTimestamp)
	.action(async (options) => {
		try {
			const keyEncoded = readFileSync(options.key, "utf-8");
			const key = load(keyEncoded);

			const claims: Claims = {
				root: options.root,
				...(options.publish && { put: options.publish }),
				...(options.subscribe && { get: options.subscribe }),
				...(options.cluster && { cluster: true }),
				...(options.expires && { exp: options.expires }),
				...(options.issued && { iat: options.issued }),
			};

			const token = await sign(key, claims);
			console.log(token);
		} catch (error) {
			console.error("Error signing token:", error instanceof Error ? error.message : error);
			process.exit(1);
		}
	});

program
	.command("verify")
	.description("Verify a token from stdin, writing the payload to stdout")
	.requiredOption("--key <path>", "Path to the key file")
	.option("--root <root>", "Root path to verify against", "")
	.action(async (options) => {
		try {
			const keyEncoded = readFileSync(options.key, "utf-8");

			// Try to load as public key first (for asymmetric), fall back to symmetric key
			let key: Key | PublicKey | undefined;
			try {
				key = loadPublic(keyEncoded);
			} catch {
				key = load(keyEncoded);
			}

			// Read token from stdin
			const token = readFileSync(0, "utf-8").trim();

			const claims = await verify(key, token, options.root);
			console.log(JSON.stringify(claims, null, 2));
		} catch (error) {
			console.error("Error verifying token:", error instanceof Error ? error.message : error);
			process.exit(1);
		}
	});

function parseUnixTimestamp(value: string): number {
	const timestamp = Number.parseInt(value, 10);
	if (Number.isNaN(timestamp)) {
		throw new Error("Expected unix timestamp");
	}
	return timestamp;
}

program.parse();
