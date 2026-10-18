import * as z from "@zod/mini";
import { u53Schema } from "./catalog/integers";

/** An optional measurement, with JSON null decoded as unavailable like Rust Option. */
export const measurementSchema = z.optional(
	z.pipe(
		z.nullable(u53Schema),
		z.transform((value) => value ?? undefined),
	),
);
