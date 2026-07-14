import type * as Catalog from "@moq/hang/catalog";
import type { Rendition as BaseRendition } from "../rendition";

export * from "./encoder";
export * from "./types";

/** A registered audio rendition on a Broadcast. See {@link BaseRendition}. */
export type Rendition = BaseRendition<Catalog.AudioConfig>;
