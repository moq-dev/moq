// Compile-time only: the generated bindings must satisfy what the baseline consumer assumes.
import type { Uniffi } from "../baseline/uniffi.ts";
import * as generated from "./dist/index.ts";

export const check: Uniffi<generated.MoqClientConfig, generated.MoqClientTls> = generated;
