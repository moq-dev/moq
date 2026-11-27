//! Widening a lite-07 subscription's ranges until they fit the message that carries them.
//!
//! A relay asks its upstream for the union of every viewer's ranges, and each viewer's
//! request fits one message while the union need not. Rather than give back the copy every
//! viewer reads through, the relay asks for a covering superset: the narrowest gaps close
//! first, so the fewest unwanted groups are asked for. Each viewer still reads only its own
//! ranges from the copy.

use rangemap::RangeSet;

#[cfg(test)]
use super::FillAsk;

use crate::{
	EncodeError,
	coding::{Form, varint},
	lite::{self, Anchor, Policy, Range, message::MAX_MESSAGE_SIZE},
	track::Position,
};

/// The most a SUBSCRIBE_UPDATE's fields besides its ranges take, size prefix included: nine
/// varints of at most nine bytes, and the priority byte.
const UPDATE_FIELDS: usize = 9 * 9 + 1;

/// The most one range takes besides its Range ID: its priority and order, each end's type,
/// then at most three varints of nine bytes for a start and two for an end.
const RANGE_FIELDS: usize = 2 + 2 + 5 * 9;

/// What ranges may take on the wire.
#[derive(Clone, Copy, Debug)]
pub(super) struct Budget {
	/// The bytes every range and its Range ID may take together.
	bytes: usize,
	form: Form,
	/// The bytes the largest Range ID any of the ranges may take.
	id: usize,
}

impl Budget {
	/// The budget for ranges a subscription takes Range IDs below `next` for, whose SUBSCRIBE
	/// carries `fields` bytes besides its ranges, `count` the most ranges it asks for.
	///
	/// Half a message, so that the ranges sent last, retired by Range ID, and the ranges
	/// replacing them fit one SUBSCRIBE_UPDATE however little the two share: a relay never
	/// narrows its ask in between, which would reset group streams still arriving. A range
	/// retuned in place costs no more than it does as it now reads, so retunes fit the same
	/// half as the ranges replacing the old ones. Never more
	/// than leaves room in a SUBSCRIBE for one range past it, which [`Self::fit_after`] takes.
	pub fn new(version: lite::Version, fields: usize, next: u64, count: usize) -> Self {
		let form = version.into();
		let half = (MAX_MESSAGE_SIZE - UPDATE_FIELDS) / 2;
		let id = varint::size(next.saturating_add(count as u64), form).unwrap_or(9);
		let room = MAX_MESSAGE_SIZE
			.saturating_sub(fields)
			.saturating_sub(RANGE_FIELDS + id);
		Self {
			bytes: half.min(room),
			form,
			id,
		}
	}

	/// What `range` costs, or `None` when it can't go out.
	fn cost(&self, range: &Range) -> Option<usize> {
		Some(lite::range_size(range, self.form)? + self.id)
	}

	/// As many of `ranges`, in order, as fit beside the `spent` ones, at least the first: one
	/// range past them still fits the message, which the budget leaves room for.
	pub fn fit_after<'a>(&self, spent: impl IntoIterator<Item = &'a Range>, ranges: Vec<Range>) -> Vec<Range> {
		let mut used = (spent.into_iter())
			.map(|range| self.cost(range).unwrap_or(usize::MAX))
			.fold(0, usize::saturating_add);
		let mut taken = Vec::with_capacity(ranges.len());
		for range in ranges {
			used = used.saturating_add(self.cost(&range).unwrap_or(usize::MAX));
			if used > self.bytes && !taken.is_empty() {
				break;
			}
			taken.push(range);
		}
		taken
	}

	fn span(&self, start: Position, end: Option<Position>, time: bool) -> usize {
		self.cost(&make_span(start, end, time)).unwrap_or(usize::MAX)
	}
}

fn span(start: Position, end: Option<Position>) -> Range {
	Range {
		start: Some(Anchor::Absolute(start)),
		end: end.map(Anchor::Absolute),
		..Range::OPEN
	}
}

fn make_span(start: Position, end: Option<Position>, time: bool) -> Range {
	match time {
		true => Range {
			start: Some(Anchor::Time(start.group)),
			end: end.map(|end| Anchor::Time(end.group)),
			..Range::OPEN
		},
		false => span(start, end),
	}
}

/// `demand`'s ranges and the `fills` groups, each widened only as far as both must be to fit
/// `budget`. A gap between fills closes across `avoid`, groups the fills leave out on
/// purpose, only once every other gap has.
///
/// Fails when even one range per kind of start would not fit, which only a broadcast path
/// or track name filling the message leaves.
pub(super) fn fit(
	demand: Vec<Range>,
	fills: &RangeSet<u64>,
	avoid: &RangeSet<u64>,
	budget: Budget,
) -> Result<(Vec<Range>, RangeSet<u64>), EncodeError> {
	// Fills take their policy where they go out, so the one here only keeps the spans' shape.
	let fill = |groups: &std::ops::Range<u64>| {
		(
			Position::group(groups.start),
			Some(Position::group(groups.end)),
			Policy::default(),
		)
	};
	let costs: Option<Vec<usize>> = demand
		.iter()
		.copied()
		.chain(fills.iter().map(|groups| {
			let (start, end, _) = fill(groups);
			span(start, end)
		}))
		.map(|range| budget.cost(&range))
		.collect();
	// One that can't go out fails when it is encoded, as it would unfitted.
	let Some(costs) = costs else {
		return Ok((demand, fills.clone()));
	};
	if costs.into_iter().fold(0, usize::saturating_add) <= budget.bytes {
		return Ok((demand, fills.clone()));
	}

	let mut time_spans = Vec::new();
	let mut pos_spans = Vec::new();
	let mut others = Vec::new();
	for range in demand {
		let policy = range.policy();
		match (range.start, range.end) {
			(Some(Anchor::Time(start)), Some(Anchor::Time(end))) => {
				time_spans.push((Position::group(start), Some(Position::group(end)), policy));
			}
			(Some(Anchor::Time(start)), None) => {
				time_spans.push((Position::group(start), None, policy));
			}
			(Some(Anchor::Absolute(start)), Some(Anchor::Absolute(end))) => {
				pos_spans.push((start, Some(end), policy));
			}
			(Some(Anchor::Absolute(start)), None) => {
				pos_spans.push((start, None, policy));
			}
			_ => others.push(range),
		}
	}
	let time_spans = Spans::new(time_spans, true, budget, |_| false);
	let pos_spans = Spans::new(pos_spans, false, budget, |_| false);
	let fills = Spans::new(fills.iter().map(fill).collect(), false, budget, |gap| {
		avoid.overlaps(&gap)
	});

	// The other ranges start where a live join's lag or the max age places them (or mix
	// anchor kinds), so no gap between them is measured: when even one span per list leaves
	// them no room, each kind collapses to the one range covering it.
	let cost = |ranges: &[Range]| -> usize {
		ranges
			.iter()
			.map(|range| budget.cost(range).unwrap_or(usize::MAX))
			.fold(0, usize::saturating_add)
	};
	if cost(&others)
		.saturating_add(time_spans.least())
		.saturating_add(pos_spans.least())
		.saturating_add(fills.least())
		> budget.bytes
	{
		others = collapse(others);
	}
	let mut total = cost(&others)
		.saturating_add(time_spans.total)
		.saturating_add(pos_spans.total)
		.saturating_add(fills.total);

	// Close the narrowest gaps first, across all lists, until everything fits.
	let mut gaps: Vec<_> = time_spans
		.gaps(0)
		.chain(pos_spans.gaps(1))
		.chain(fills.gaps(2))
		.collect();
	gaps.sort_unstable();
	let mut lists = [time_spans, pos_spans, fills];
	for (_, kind, index) in gaps {
		if total <= budget.bytes {
			break;
		}
		let list = &mut lists[usize::from(kind)];
		total -= list.total;
		list.join(index);
		total += list.total;
	}
	if total > budget.bytes {
		return Err(EncodeError::TooLarge);
	}

	let [time_spans, pos_spans, fills] = lists;
	let mut ranges: Vec<Range> = time_spans
		.ranges()
		.map(|(start, end, policy)| make_span(start, end, true).with_policy(policy))
		.chain(
			(pos_spans.ranges()).map(|(start, end, policy)| make_span(start, end, false).with_policy(policy)),
		)
		.collect();
	ranges.extend(others);
	let groups = fills
		.ranges()
		.map(|(start, end, _)| start.group..end.expect("fills are bounded").group);
	Ok((ranges, groups.collect()))
}

/// Disjoint spans in order, each the run of the input spans from one start to its chain's end.
///
/// A span merged from several goes out at the most urgent policy among them, so each still
/// gets its groups at least as soon as it asked: a policy costs the same bytes whatever it is.
struct Spans {
	spans: Vec<(Position, Option<Position>)>,
	/// The policy each span goes out at.
	policies: Vec<Policy>,
	/// Whether these spans carry inclusive Time anchors rather than exclusive Position anchors.
	time: bool,
	/// The last span of the chain starting at each chain's first, and the first of the chain
	/// ending at each chain's last.
	end: Vec<usize>,
	start: Vec<usize>,
	/// Whether the gap after each span closes only once every other gap has.
	last: Vec<bool>,
	total: usize,
	budget: Budget,
}

impl Spans {
	fn new(
		mut input: Vec<(Position, Option<Position>, Policy)>,
		time: bool,
		budget: Budget,
		last: impl Fn(std::ops::Range<u64>) -> bool,
	) -> Self {
		input.sort_unstable();
		// Spans that overlap or touch cost less as one, and ask for nothing more.
		let mut spans: Vec<(Position, Option<Position>)> = Vec::with_capacity(input.len());
		let mut policies: Vec<Policy> = Vec::with_capacity(input.len());
		for (start, end, policy) in input {
			let touches = |last: Option<Position>| match (last, time) {
				(None, _) => true,
				(Some(last), true) => last.group.saturating_add(1) >= start.group,
				(Some(last), false) => last >= start,
			};
			match (spans.last_mut(), policies.last_mut()) {
				(Some((_, last)), Some(top)) if touches(*last) => {
					*last = match (*last, end) {
						(Some(last), Some(end)) => Some(last.max(end)),
						_ => None,
					};
					*top = Policy::top([*top, policy]);
				}
				_ => {
					spans.push((start, end));
					policies.push(policy);
				}
			}
		}
		let last = spans
			.windows(2)
			.map(|pair| {
				let end = pair[0].1.expect("only the last is open").group;
				let gap_start = if time { end.saturating_add(1) } else { end };
				last(gap_start..pair[1].0.group)
			})
			.collect();
		let total = spans.iter().map(|(start, end)| budget.span(*start, *end, time)).sum();
		Self {
			time,
			end: (0..spans.len()).collect(),
			start: (0..spans.len()).collect(),
			spans,
			policies,
			last,
			total,
			budget,
		}
	}

	/// What the one span covering every span costs, or nothing when there are none.
	fn least(&self) -> usize {
		match (self.spans.first(), self.spans.last()) {
			(Some((start, _)), Some((_, end))) => self.budget.span(*start, *end, self.time),
			_ => 0,
		}
	}

	/// Every gap: how late it closes, `tag`, and the span before it.
	fn gaps(&self, tag: u8) -> impl Iterator<Item = ((bool, u64), u8, usize)> + '_ {
		self.spans.windows(2).enumerate().map(move |(index, pair)| {
			let end = pair[0].1.expect("only the last is open");
			let gap_start = if self.time {
				end.group.saturating_add(1)
			} else {
				end.group
			};
			(
				(self.last[index], pair[1].0.group.saturating_sub(gap_start)),
				tag,
				index,
			)
		})
	}

	/// Close the gap after span `index`, joining the chains on either side.
	fn join(&mut self, index: usize) {
		let first = self.start[index];
		let last = self.end[index + 1];
		self.total -= self.budget.span(self.spans[first].0, self.spans[index].1, self.time);
		self.total -= self.budget.span(self.spans[index + 1].0, self.spans[last].1, self.time);
		self.total += self.budget.span(self.spans[first].0, self.spans[last].1, self.time);
		self.end[first] = last;
		self.start[last] = first;
	}

	/// Each chain as the one span covering it, at the most urgent policy along it.
	fn ranges(&self) -> impl Iterator<Item = (Position, Option<Position>, Policy)> + '_ {
		let mut index = 0;
		std::iter::from_fn(move || {
			let first = index;
			let last = *self.end.get(first)?;
			index = last + 1;
			let policy = Policy::top(self.policies[first..=last].iter().copied());
			Some((self.spans[first].0, self.spans[last].1, policy))
		})
	}
}

/// One range per kind of start covering every range of that kind: from the earliest start
/// to the latest end, at the most urgent policy among them.
fn collapse(ranges: Vec<Range>) -> Vec<Range> {
	let kind = |range: &Range| match range.start {
		None => 0,
		Some(Anchor::Time(_)) => 1,
		Some(Anchor::Absolute(_)) => 2,
		Some(Anchor::Live { .. }) => 3,
		Some(Anchor::Joined { .. }) => 4,
	};
	let mut kinds: Vec<Range> = Vec::new();
	for range in ranges {
		let Some(cover) = kinds.iter_mut().find(|cover| kind(cover) == kind(&range)) else {
			kinds.push(range);
			continue;
		};
		cover.start = match (cover.start, range.start) {
			(Some(Anchor::Time(a)), Some(Anchor::Time(b))) => Some(Anchor::Time(a.min(b))),
			(Some(Anchor::Absolute(a)), Some(Anchor::Absolute(b))) => Some(Anchor::Absolute(a.min(b))),
			// The join reaching furthest back, none past one with no lag limit, under the widest
			// max age, delivers what each of the others does.
			(Some(Anchor::Live { min: a, max: x }), Some(Anchor::Live { min: b, max: y })) => Some(Anchor::Live {
				min: a.zip(b).map(|(a, b)| a.max(b)),
				max: x.zip(y).map(|(x, y)| x.max(y)),
			}),
			// The join placed earliest, under the widest max age.
			(Some(Anchor::Joined { at: a, max: x }), Some(Anchor::Joined { at: b, max: y })) => Some(Anchor::Joined {
				at: a.min(b),
				max: x.zip(y).map(|(x, y)| x.max(y)),
			}),
			(start, _) => start,
		};
		cover.end = match (cover.end, range.end) {
			(Some(Anchor::Absolute(a)), Some(Anchor::Absolute(b))) => Some(Anchor::Absolute(a.max(b))),
			(Some(Anchor::Time(a)), Some(Anchor::Time(b))) => Some(Anchor::Time(a.max(b))),
			// Ends of different kinds can't be ordered without the latest group: follow the edge.
			_ => None,
		};
		*cover = cover.with_policy(Policy::top([cover.policy(), range.policy()]));
	}
	kinds
}

#[cfg(test)]
mod tests {
	use super::*;
	use rand::{RngExt, SeedableRng};

	fn budget(bytes: usize) -> Budget {
		Budget {
			bytes,
			form: lite::Version::Lite07.into(),
			id: 2,
		}
	}

	fn total(budget: Budget, ranges: &[Range], groups: &RangeSet<u64>) -> usize {
		let fills = FillAsk::default().wire(groups);
		ranges
			.iter()
			.copied()
			.chain(fills)
			.map(|range| budget.cost(&range).unwrap())
			.sum()
	}

	/// Whether `outer` asks for everything `inner` does, at least as urgently, judged without
	/// the latest group.
	fn covers(outer: &Range, inner: &Range) -> bool {
		let start = match (outer.start, inner.start) {
			(None, _) => true,
			(Some(Anchor::Absolute(outer)), Some(Anchor::Absolute(inner))) => outer <= inner,
			(Some(Anchor::Time(outer)), Some(Anchor::Time(inner))) => outer <= inner,
			_ => false,
		};
		let end = match (outer.end, inner.end) {
			(None, _) => true,
			(Some(Anchor::Absolute(outer)), Some(Anchor::Absolute(inner))) => outer >= inner,
			(Some(Anchor::Time(outer)), Some(Anchor::Time(inner))) => outer >= inner,
			_ => false,
		};
		start && end && outer.policy().covers(&inner.policy())
	}

	fn random(rng: &mut rand::rngs::StdRng) -> (Vec<Range>, RangeSet<u64>, RangeSet<u64>) {
		let group = |rng: &mut rand::rngs::StdRng| rng.random_range(0..2_000u64) << rng.random_range(0..30);
		let mut demand = Vec::new();
		for _ in 0..rng.random_range(0..300) {
			let start = group(rng);
			let end = (rng.random_bool(0.9)).then(|| Position::group(start + rng.random_range(1..50)));
			demand.push(match rng.random_range(0..10) {
				0 => Range {
					start: Some(Anchor::Time(rng.random_range(0..20))),
					end: end.map(Anchor::Absolute),
					..Range::OPEN
				},
				1 => Range {
					start: None,
					end: end.map(Anchor::Absolute),
					..Range::OPEN
				},
				_ => span(Position::group(start), end),
			});
		}
		for range in &mut demand {
			*range = range.with_policy(Policy {
				priority: rng.random_range(-1..=1),
				order: match rng.random_bool(0.5) {
					true => crate::track::Order::Ascending,
					false => crate::track::Order::Descending,
				},
			});
		}
		let mut fills = RangeSet::new();
		let mut avoid = RangeSet::new();
		for _ in 0..rng.random_range(0..300) {
			let start = group(rng);
			let groups = start..start + rng.random_range(1..20);
			match rng.random_bool(0.8) {
				true => fills.insert(groups),
				false => avoid.insert(groups),
			}
		}
		for groups in avoid.iter() {
			fills.remove(groups.clone());
		}
		(demand, fills, avoid)
	}

	/// What fits stays as asked; what doesn't is widened to a covering superset that fits,
	/// never asking for a group the fills avoid while a narrower choice remains.
	#[test]
	fn widens_only_what_does_not_fit() {
		let mut rng = rand::rngs::StdRng::seed_from_u64(0x666974);
		for step in 0..2_000 {
			let (demand, fills, avoid) = random(&mut rng);
			let budget = budget(rng.random_range(200..4_000));
			let before = total(budget, &demand, &fills);
			let (ranges, groups) = fit(demand.clone(), &fills, &avoid, budget).unwrap();
			if before <= budget.bytes {
				assert_eq!((&ranges, &groups), (&demand, &fills), "step {step}");
				continue;
			}
			assert!(total(budget, &ranges, &groups) <= budget.bytes, "step {step}");
			for range in &demand {
				assert!(
					ranges.iter().any(|outer| covers(outer, range)),
					"step {step}: {range:?}"
				);
			}
			for run in fills.iter() {
				assert!(groups.gaps(run).next().is_none(), "step {step}: {run:?}");
			}
		}
	}

	/// Two fitted sets fit one SUBSCRIBE_UPDATE between them however little they share, at its
	/// widest fields, and either fits a SUBSCRIBE.
	#[test]
	fn any_edit_between_fitted_ranges_fits_one_message() {
		use crate::coding::Encode as _;
		let version = lite::Version::Lite07;
		let widest = |ranges| lite::SubscribeUpdate {
			wanted_group: Some(u64::MAX - 1),
			priority: i8::MIN,
			max_delay: Some(std::time::Duration::from_millis(u64::MAX - 1)),
			ranges,
			order: crate::track::Order::Descending,
		};
		let empty = lite::RangeEdit::Edit {
			remove: Vec::new(),
			add: Vec::new(),
			retune: Vec::new(),
		};
		assert!(widest(empty).encode_bytes(version).unwrap().len() <= UPDATE_FIELDS);

		// Sets of many short ranges and of fewer long ones, alternately, so the IDs one retires
		// outweigh what the other's ranges leave.
		let mut rng = rand::rngs::StdRng::seed_from_u64(0x65646974);
		let wide = |rng: &mut rand::rngs::StdRng, next: u64, bits: u32| {
			let ranges: Vec<Range> = (0..10_000)
				.map(|_| {
					let start = rng.random_range(0..1u64 << bits);
					span(Position::group(start), Some(Position::group(start + 1)))
				})
				.collect();
			let budget = Budget::new(version, 0, next, ranges.len());
			fit(ranges, &RangeSet::new(), &RangeSet::new(), budget).unwrap().0
		};
		let mut live = lite::LiveRanges::new(version, &wide(&mut rng, 0, 14));
		for round in 0..4 {
			let next = wide(&mut rng, live.next(), [50, 14][round % 2]);
			let edit = live.edit(&next).unwrap();
			widest(edit).encode_bytes(version).unwrap();
		}
	}

	/// An update retuning every live range it keeps, each to another priority and order and
	/// each live join to the widest max age, fits one message beside the ranges it adds: a
	/// retune takes no more than the range it names as it now reads, which the half message
	/// for the new ranges already counts, so retunes need no budget of their own.
	#[test]
	fn an_edit_retuning_every_live_range_fits_one_message() {
		use crate::coding::Encode as _;
		let version = lite::Version::Lite07;
		let widest = |ranges| lite::SubscribeUpdate {
			wanted_group: Some(u64::MAX - 1),
			priority: i8::MIN,
			max_delay: Some(std::time::Duration::from_millis(u64::MAX - 1)),
			ranges,
			order: crate::track::Order::Descending,
		};
		let join = |lag: u64| Range {
			start: Some(Anchor::Live {
				min: Some(std::time::Duration::from_millis(lag)),
				max: Some(std::time::Duration::ZERO),
			}),
			..Range::OPEN
		};
		let urgent = Policy {
			priority: i8::MIN,
			order: crate::track::Order::Ascending,
		};
		// The policy alone, so every live range is kept, then the max age too, so each kept one
		// grows and fewer fit.
		for widen in [false, true] {
			let old: Vec<Range> = (0..20_000).map(join).collect();
			let old = Budget::new(version, 0, 0, old.len()).fit_after([], old);
			let mut live = lite::LiveRanges::new(version, &old);
			let retuned = old.iter().map(|range| {
				let mut range = range.with_policy(urgent);
				if let (true, Some(Anchor::Live { max, .. })) = (widen, &mut range.start) {
					*max = Some(std::time::Duration::from_millis(u64::MAX - 1));
				}
				range
			});
			let next: Vec<Range> = retuned.chain((20_000..40_000).map(join)).collect();
			let next = Budget::new(version, 0, live.next(), next.len()).fit_after([], next);
			let edit = live.edit(&next).unwrap();
			let lite::RangeEdit::Edit { retune, .. } = &edit else {
				panic!("lite-07 edits ranges");
			};
			assert!(retune.len() * 2 >= old.len(), "widen {widen}: most live ranges retuned");
			let size = widest(edit).encode_bytes(version).unwrap().len();
			assert!(size <= MAX_MESSAGE_SIZE, "widen {widen}: {size}");
		}
	}

	/// The range [`Budget::fit_after`] takes past a full budget still fits the message beside
	/// the fields a SUBSCRIBE carries, however much of it those take.
	#[test]
	fn a_range_past_a_full_budget_fits_the_message() {
		let version = lite::Version::Lite07;
		let small = span(Position::group(0), Some(Position::group(1)));
		let probe = span(Position::group(u64::MAX - 2), Some(Position::group(u64::MAX - 1)));
		for fields in [0, MAX_MESSAGE_SIZE / 2, MAX_MESSAGE_SIZE - 100] {
			let budget = Budget::new(version, fields, 0, 1_000);
			let cost = |ranges: &[Range]| ranges.iter().map(|range| budget.cost(range).unwrap()).sum::<usize>();
			// As many as the budget holds.
			let spent = vec![small; budget.bytes / cost(&[small])];
			let taken = budget.fit_after(&spent, vec![probe]);
			assert_eq!(taken, [probe], "fields {fields}");
			assert!(
				fields + cost(&spent) + cost(&taken) <= MAX_MESSAGE_SIZE,
				"fields {fields}: overflows by {}",
				fields + cost(&spent) + cost(&taken) - MAX_MESSAGE_SIZE
			);
		}
	}

	/// Each kind of start collapses to the one range covering it once nothing else fits.
	#[test]
	fn collapses_ranges_with_no_gap_to_close() {
		let times: Vec<Range> = (0..200)
			.map(|back| Range {
				start: Some(Anchor::Time(1_000 - back)),
				end: Some(Anchor::Absolute(Position::group(1_000 + back))),
				..Range::OPEN
			})
			.collect();
		let (ranges, _) = fit(times.clone(), &RangeSet::new(), &RangeSet::new(), budget(100)).unwrap();
		assert_eq!(
			ranges,
			[Range {
				start: Some(Anchor::Time(801)),
				end: Some(Anchor::Absolute(Position::group(1_199))),
				..Range::OPEN
			}]
		);
		assert!(matches!(
			fit(times, &RangeSet::new(), &RangeSet::new(), budget(4)),
			Err(EncodeError::TooLarge)
		));
	}

	/// The narrowest gaps close first, so the fewest unwanted groups are asked for.
	#[test]
	fn closes_the_narrowest_gaps_first() {
		let groups = |runs: &[std::ops::Range<u64>]| runs.iter().cloned().collect::<RangeSet<u64>>();
		let fills = groups(&[0..1, 2..3, 100..101, 103..104, 1_000..1_001]);
		let fits = total(budget(0), &[], &groups(&[0..3, 100..104, 1_000..1_001]));
		let (_, got) = fit(Vec::new(), &fills, &RangeSet::new(), budget(fits)).unwrap();
		assert_eq!(got, groups(&[0..3, 100..104, 1_000..1_001]));
		// A gap the fills avoid closes last, however narrow.
		let mut avoid = RangeSet::new();
		avoid.insert(1..2);
		let (_, got) = fit(Vec::new(), &fills, &avoid, budget(fits)).unwrap();
		assert_eq!(got, groups(&[0..1, 2..104, 1_000..1_001]));
	}
}
