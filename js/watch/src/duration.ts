import { Time } from "@moq/net";

/**
 * Parse a duration attribute: `"300ms"`, `"30s"`, or a bare `"0"`.
 *
 * A bare number other than zero is rejected rather than guessed at. Most players measure these in
 * seconds while we store milliseconds, so assuming a unit would turn a value copied from another
 * player into a 1000x error. Zero is exempt because `0ms` and `0s` are the same value.
 *
 * Returns undefined when the value doesn't parse, leaving the fallback to the caller.
 *
 * @internal
 */
export function parseDuration(value: string): Time.Milli | undefined {
	const match = /^(\d+(?:\.\d+)?)(ms|s)?$/.exec(value.trim());
	if (!match) return undefined;

	const amount = Number.parseFloat(match[1]);
	if (!Number.isFinite(amount)) return undefined;
	if (match[2] === undefined) return amount === 0 ? Time.Milli.zero : undefined;

	return Time.Milli(match[2] === "s" ? amount * 1000 : amount);
}

/**
 * Render a duration back out as an attribute value, in the canonical unit.
 *
 * Not rounded: the reflected attribute is parsed straight back into the signal, so rounding here
 * would rewrite the value behind the caller (`1.5ms` becoming `2ms`, and `0.4ms` becoming zero,
 * which also switches buffered playback off).
 *
 * @internal
 */
export function formatDuration(value: Time.Milli): string {
	return `${value}ms`;
}
