/**
 * This library's name and version, the default each side advertises in SETUP.
 *
 * A User-Agent style product token. A wrapper prepends its own, outermost first:
 * `my-app/1.0 moq-net-js/0.4.2`. `implementation.test.ts` fails when it drifts from
 * `package.json`, so a version bump updates both.
 */
export const IMPLEMENTATION = "moq-net-js/0.4.2";
