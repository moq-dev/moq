//! Widening a lite-07 subscription's ranges until they fit the message that carries them.
//!
//! A relay asks its upstream for the union of every viewer's ranges, and each viewer's
//! request fits one message while the union need not. Rather than give back the copy every
//! viewer reads through, the relay asks for a covering superset: the narrowest gaps close
//! first, so the fewest unwanted groups are asked for. Each viewer still reads only its own
//! ranges from the copy.

use rangemap::RangeSet;

use crate::{
	EncodeError,
	coding::{Form, varint},
	lite::{self, Anchor, Range, message::MAX_MESSAGE_SIZE},
	track::Position,
};

/// The most a SUBSCRIBE_UPDATE's fields besides its ranges take, size prefix included: nine
/// varints of at most nine bytes, and the priority byte.
const UPDATE_FIELDS: usize = 9 * 9 + 1;

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
	/// narrows its ask in between, which would reset group streams still arriving.
	pub fn new(version: lite::Version, fields: usize, next: u64, count: usize) -> Self {
		let form = version.into();
		let half = (MAX_MESSAGE_SIZE - UPDATE_FIELDS) / 2;
		Self {
			bytes: half.min(MAX_MESSAGE_SIZE.saturating_sub(fields)),
			form,
			id: varint::size(next.saturating_add(count as u64), form).unwrap_or(9),
		}
	}

	/// What `range` costs, or `None` when it can't go out.
	fn cost(&self, range: &Range) -> Option<usize> {
		Some(lite::range_size(range, self.form)? + self.id)
	}

	fn span(&self, start: Position, end: Option<Position>) -> usize {
		self.cost(&span(start, end)).unwrap_or(usize::MAX)
	}
}

fn span(start: Position, end: Option<Position>) -> Range {
	Range {
		start: Some(Anchor::Absolute(start)),
		end: end.map(Anchor::Absolute),
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
	let fill = |groups: &std::ops::Range<u64>| (Position::group(groups.start), Some(Position::group(groups.end)));
	let costs: Option<Vec<usize>> = demand
		.iter()
		.copied()
		.chain(fills.iter().map(|groups| {
			let (start, end) = fill(groups);
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

	let mut spans = Vec::new();
	let mut others = Vec::new();
	for range in demand {
		match range.start {
			Some(Anchor::Absolute(start)) => spans.push((
				start,
				match range.end {
					Some(Anchor::Absolute(end)) => Some(end),
					_ => None,
				},
			)),
			_ => others.push(range),
		}
	}
	let spans = Spans::new(spans, budget, |_| false);
	let fills = Spans::new(fills.iter().map(fill).collect(), budget, |gap| avoid.overlaps(&gap));

	// The other ranges count back from the latest group or start at the oldest, so no gap
	// between them is measured: when even one span per list leaves them no room, each kind
	// collapses to the one range covering it.
	let cost = |ranges: &[Range]| -> usize {
		ranges
			.iter()
			.map(|range| budget.cost(range).unwrap_or(usize::MAX))
			.fold(0, usize::saturating_add)
	};
	if cost(&others)
		.saturating_add(spans.least())
		.saturating_add(fills.least())
		> budget.bytes
	{
		others = collapse(others);
	}
	let mut total = cost(&others).saturating_add(spans.total).saturating_add(fills.total);

	// Close the narrowest gaps first, of either list, until everything fits.
	let mut gaps: Vec<_> = spans.gaps(false).chain(fills.gaps(true)).collect();
	gaps.sort_unstable();
	let mut lists = [spans, fills];
	for (_, fill, index) in gaps {
		if total <= budget.bytes {
			break;
		}
		let list = &mut lists[usize::from(fill)];
		total -= list.total;
		list.join(index);
		total += list.total;
	}
	if total > budget.bytes {
		return Err(EncodeError::TooLarge);
	}

	let [spans, fills] = lists;
	let mut ranges: Vec<Range> = spans.ranges().map(|(start, end)| span(start, end)).collect();
	ranges.extend(others);
	let groups = fills.ranges().map(|(start, end)| start.group..end.expect("fills are bounded").group);
	Ok((ranges, groups.collect()))
}

/// Disjoint spans in order, each the run of the input spans from one start to its chain's end.
struct Spans {
	spans: Vec<(Position, Option<Position>)>,
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
		mut input: Vec<(Position, Option<Position>)>,
		budget: Budget,
		last: impl Fn(std::ops::Range<u64>) -> bool,
	) -> Self {
		input.sort_unstable();
		// Spans that overlap or touch cost less as one, and ask for nothing more.
		let mut spans: Vec<(Position, Option<Position>)> = Vec::with_capacity(input.len());
		for (start, end) in input {
			match spans.last_mut() {
				Some((_, last)) if last.is_none_or(|last| last >= start) => {
					*last = match (*last, end) {
						(Some(last), Some(end)) => Some(last.max(end)),
						_ => None,
					};
				}
				_ => spans.push((start, end)),
			}
		}
		let last = spans
			.windows(2)
			.map(|pair| last(pair[0].1.expect("only the last is open").group..pair[1].0.group))
			.collect();
		let total = spans.iter().map(|(start, end)| budget.span(*start, *end)).sum();
		Self {
			end: (0..spans.len()).collect(),
			start: (0..spans.len()).collect(),
			spans,
			last,
			total,
			budget,
		}
	}

	/// What the one span covering every span costs, or nothing when there are none.
	fn least(&self) -> usize {
		match (self.spans.first(), self.spans.last()) {
			(Some((start, _)), Some((_, end))) => self.budget.span(*start, *end),
			_ => 0,
		}
	}

	/// Every gap: how late it closes, `tag`, and the span before it.
	fn gaps(&self, tag: bool) -> impl Iterator<Item = ((bool, u64), bool, usize)> + '_ {
		self.spans.windows(2).enumerate().map(move |(index, pair)| {
			let end = pair[0].1.expect("only the last is open");
			((self.last[index], pair[1].0.group.saturating_sub(end.group)), tag, index)
		})
	}

	/// Close the gap after span `index`, joining the chains on either side.
	fn join(&mut self, index: usize) {
		let first = self.start[index];
		let last = self.end[index + 1];
		self.total -= self.budget.span(self.spans[first].0, self.spans[index].1);
		self.total -= self.budget.span(self.spans[index + 1].0, self.spans[last].1);
		self.total += self.budget.span(self.spans[first].0, self.spans[last].1);
		self.end[first] = last;
		self.start[last] = first;
	}

	/// Each chain as the one span covering it.
	fn ranges(&self) -> impl Iterator<Item = (Position, Option<Position>)> + '_ {
		let mut index = 0;
		std::iter::from_fn(move || {
			let first = index;
			let last = *self.end.get(first)?;
			index = last + 1;
			Some((self.spans[first].0, self.spans[last].1))
		})
	}
}

/// One range per kind of start covering every range of that kind: from the earliest start
/// to the latest end.
fn collapse(ranges: Vec<Range>) -> Vec<Range> {
	let kind = |range: &Range| match range.start {
		None => 0,
		Some(Anchor::Relative(_)) => 1,
		Some(Anchor::Time(_)) => 2,
		Some(Anchor::Absolute(_)) => 3,
	};
	let mut kinds: Vec<Range> = Vec::new();
	for range in ranges {
		let Some(cover) = kinds.iter_mut().find(|cover| kind(cover) == kind(&range)) else {
			kinds.push(range);
			continue;
		};
		cover.start = match (cover.start, range.start) {
			(Some(Anchor::Relative(a)), Some(Anchor::Relative(b))) => Some(Anchor::Relative(a.max(b))),
			(Some(Anchor::Time(a)), Some(Anchor::Time(b))) => Some(Anchor::Time(a.min(b))),
			(Some(Anchor::Absolute(a)), Some(Anchor::Absolute(b))) => Some(Anchor::Absolute(a.min(b))),
			(start, _) => start,
		};
		cover.end = match (cover.end, range.end) {
			(Some(Anchor::Absolute(a)), Some(Anchor::Absolute(b))) => Some(Anchor::Absolute(a.max(b))),
			(Some(Anchor::Time(a)), Some(Anchor::Time(b))) => Some(Anchor::Time(a.max(b))),
			// Ends of different kinds can't be ordered without the latest group: follow the edge.
			_ => None,
		};
	}
	kinds
}
