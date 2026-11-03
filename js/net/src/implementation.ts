import { VERSION } from "./version.ts";

/**
 * This library's name and version, the default each side advertises in SETUP.
 *
 * A User-Agent style product token. A wrapper prepends its own, outermost first:
 * `my-app/1.0 moq-net-js/0.4.2`. Run from source rather than a published build, it reads `moq-net-js/dev`.
 */
export const IMPLEMENTATION = `moq-net-js/${VERSION}`;
