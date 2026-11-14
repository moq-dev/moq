import * as z from "zod/mini";

// A missing counter reads as zero, so a newer relay can add counters and an older one can omit them.
const counter = z._default(z.number(), 0);
// A missing nested object reads as all zeros. Every field defaults, so `{}` is a valid input,
// which the generic shape cannot show the type checker.
const zeros = <S extends z.core.$ZodLooseShape>(shape: S) => z.prefault(z.object(shape), {} as never);

// Copies each legacy counter name to its canonical one when only the legacy is present, so an
// older relay still reads correctly; the canonical value wins when both are.
const legacy = <S extends z.core.$ZodType>(names: Record<string, string>, schema: S) =>
	z.pipe(
		z.transform((input: unknown) => {
			if (typeof input !== "object" || input === null) return input;
			const out: Record<string, unknown> = { ...input };
			for (const [old, name] of Object.entries(names)) {
				if (out[name] === undefined && out[old] !== undefined) out[name] = out[old];
			}
			return out;
		}),
		schema,
	);

/** Payload a subscriber skipped for falling behind its delay budget. */
export const ContentSchema = zeros({ bytes: counter, frames: counter, groups: counter });

/** Cumulative traffic counters for one slice of a node's traffic; a live count is `*_started - *_ended`. */
export const TrafficSchema = legacy(
	{
		announced: "announces_started",
		announced_closed: "announces_ended",
		broadcasts: "broadcasts_started",
		broadcasts_closed: "broadcasts_ended",
		subscriptions: "subscriptions_started",
		subscriptions_closed: "subscriptions_ended",
	},
	zeros({
		announces_started: counter,
		announces_ended: counter,
		announced_bytes: counter,
		broadcasts_started: counter,
		broadcasts_ended: counter,
		subscriptions_started: counter,
		subscriptions_ended: counter,
		fetches: counter,
		bytes: counter,
		frames: counter,
		groups: counter,
		datagrams: counter,
		inflated: counter,
		stale: ContentSchema,
	}),
);

/** Sessions refused before admission, by reason. */
export const RefusalsSchema = zeros({
	refused: counter,
	unavailable: counter,
	request: counter,
	forbidden: counter,
	membership: counter,
	expired: counter,
});

/** Admitted sessions that ended abnormally, by kind. */
export const FailuresSchema = zeros({
	idle_timeout: counter,
	transport: counter,
	application: counter,
	expired: counter,
});

/** Sessions and requests under one stats label; live sessions are `sessions_started - sessions_ended`. */
export const PresenceSchema = legacy(
	{ sessions: "sessions_started", sessions_closed: "sessions_ended" },
	zeros({
		sessions_refused: RefusalsSchema,
		sessions_failed: FailuresSchema,
		requests: counter,
		sessions_started: counter,
		sessions_ended: counter,
		sent_bytes: counter,
		received_bytes: counter,
		announces_peak: counter,
		subscriptions_peak: counter,
		wire_tx: counter,
		wire_rx: counter,
		routes_refused: counter,
	}),
);

/** One tier's cumulative counters for a whole node: egress, ingress, and sessions. */
export const TotalsSchema = zeros({ publisher: TrafficSchema, subscriber: TrafficSchema, sessions: PresenceSchema });

/** Egress and ingress for one part of a prefix or one broadcast path. */
export const UsageSchema = zeros({ publisher: TrafficSchema, subscriber: TrafficSchema });

/** One tier's counters for a broadcast prefix: its rollup, the broadcast at exactly it, and each direct child. */
export const PrefixSchema = zeros({
	publisher: TrafficSchema,
	subscriber: TrafficSchema,
	self: UsageSchema,
	children: z._default(z.record(z.string(), UsageSchema), {}),
});

/** One tier's counters for a bucket: its totals for the run, then live per-path traffic and per-label sessions. */
export const BucketSchema = zeros({
	publisher: TrafficSchema,
	subscriber: TrafficSchema,
	sessions: PresenceSchema,
	paths: z._default(z.record(z.string(), UsageSchema), {}),
	labels: z._default(z.record(z.string(), PresenceSchema), {}),
});

// Keyed by tier label, `""` for the default tier.
const tiers = <T extends z.ZodMiniType>(entry: T) => z.record(z.string(), entry);

/** A frame off the totals track, keyed by tier label. */
export const TotalsFrameSchema = tiers(TotalsSchema);
/** A frame off a presence track, keyed by tier label. */
export const PresenceFrameSchema = tiers(PresenceSchema);
/** A frame off a prefix track, keyed by tier label. */
export const PrefixFrameSchema = tiers(PrefixSchema);
/** A frame off a bucket track: the run's epoch and each tier's counters. */
export const BucketFrameSchema = z.object({
	epoch: z.string().check(
		z.minLength(1),
		z.refine((epoch) => !epoch.includes("/"), "an epoch holds no `/`"),
	),
	tiers: z._default(tiers(BucketSchema), {}),
});

/** Payload counters for skipped content. */
export type Content = z.infer<typeof ContentSchema>;
/** Cumulative traffic counters. */
export type Traffic = z.infer<typeof TrafficSchema>;
/** Session refusals by reason. */
export type Refusals = z.infer<typeof RefusalsSchema>;
/** Session failures by kind. */
export type Failures = z.infer<typeof FailuresSchema>;
/** Sessions and requests under one stats label. */
export type Presence = z.infer<typeof PresenceSchema>;
/** One tier's node-wide totals. */
export type Totals = z.infer<typeof TotalsSchema>;
/** Egress and ingress for one path or prefix part. */
export type Usage = z.infer<typeof UsageSchema>;
/** One tier's counters for a broadcast prefix. */
export type Prefix = z.infer<typeof PrefixSchema>;
/** One tier's counters for a bucket. */
export type Bucket = z.infer<typeof BucketSchema>;
/** A frame off the totals track. */
export type TotalsFrame = z.infer<typeof TotalsFrameSchema>;
/** A frame off a presence track. */
export type PresenceFrame = z.infer<typeof PresenceFrameSchema>;
/** A frame off a prefix track. */
export type PrefixFrame = z.infer<typeof PrefixFrameSchema>;
/** A frame off a bucket track. */
export type BucketFrame = z.infer<typeof BucketFrameSchema>;
