//! Checks the pattern algebra against brute-force matching over every small pattern
//! and path.
//!
//! `contains`, `overlaps`, `rebase`, `specificity`, and union reduction all have a
//! definition in terms of `matches`; this enumerates the small cases and holds each to
//! it. Two alphabets: one exercises the segment structure (globstar head and tail
//! interplay needs three-segment patterns and six-segment paths for every witness to
//! fit), the other exercises partial segments against multi-byte parts.

use moq_net::path::{Pattern, Patterns, Segment};

struct Alphabet {
	segments: Vec<Segment>,
	max_pattern: usize,
	parts: &'static [&'static str],
	max_path: usize,
}

fn literal(text: &str) -> Segment {
	Segment::Literal(text.into())
}

fn partial(prefix: &str, suffix: &str) -> Segment {
	Segment::Partial {
		prefix: prefix.into(),
		suffix: suffix.into(),
	}
}

fn alphabets() -> Vec<Alphabet> {
	vec![
		Alphabet {
			segments: vec![literal("a"), literal("b"), Segment::Wildcard, Segment::Globstar],
			max_pattern: 3,
			parts: &["a", "b", "c"],
			max_path: 6,
		},
		Alphabet {
			segments: vec![
				literal("a"),
				literal("ab"),
				partial("a", ""),
				partial("", "b"),
				partial("a", "b"),
				Segment::Wildcard,
				Segment::Globstar,
			],
			max_pattern: 2,
			parts: &["a", "b", "ab", "ba", "aab"],
			max_path: 4,
		},
	]
}

impl Alphabet {
	/// Every valid pattern over the segments with up to `max_pattern` of them.
	fn patterns(&self) -> Vec<Pattern> {
		let mut out = vec![Pattern::default()];
		let mut layer: Vec<Vec<Segment>> = vec![vec![]];
		for _ in 0..self.max_pattern {
			let mut next = Vec::new();
			for prefix in &layer {
				for choice in &self.segments {
					let mut segments = prefix.clone();
					segments.push(choice.clone());
					if let Ok(pattern) = Pattern::new(segments.clone()) {
						out.push(pattern);
						next.push(segments);
					}
				}
			}
			layer = next;
		}
		out
	}

	/// Every path over the parts with up to `max` segments, the empty path included.
	fn paths(&self, max: usize) -> Vec<String> {
		let mut out = vec![String::new()];
		let mut layer = vec![Vec::<&str>::new()];
		for _ in 0..max {
			let mut next = Vec::new();
			for prefix in &layer {
				for part in self.parts {
					let mut parts = prefix.clone();
					parts.push(part);
					out.push(parts.join("/"));
					next.push(parts);
				}
			}
			layer = next;
		}
		out
	}
}

fn join(root: &str, rel: &str) -> String {
	match (root.is_empty(), rel.is_empty()) {
		(true, _) => rel.to_string(),
		(_, true) => root.to_string(),
		_ => format!("{root}/{rel}"),
	}
}

/// Which paths each pattern matches, computed once so the pairwise checks below
/// compare bit tables instead of re-matching strings.
fn match_table(alphabet: &Alphabet) -> (Vec<Pattern>, Vec<Vec<bool>>) {
	let patterns = alphabet.patterns();
	let paths = alphabet.paths(alphabet.max_path);
	let table = patterns
		.iter()
		.map(|pattern| paths.iter().map(|p| pattern.matches(p)).collect())
		.collect();
	(patterns, table)
}

#[test]
fn contains_means_every_match_is_shared() {
	for alphabet in alphabets() {
		let (patterns, table) = match_table(&alphabet);
		for (i, a) in patterns.iter().enumerate() {
			for (j, b) in patterns.iter().enumerate() {
				let expected = table[i].iter().zip(&table[j]).all(|(a, b)| !b || *a);
				assert_eq!(a.contains(b), expected, "{a} contains {b}");
			}
		}
	}
}

#[test]
fn overlaps_means_some_match_is_shared() {
	for alphabet in alphabets() {
		let (patterns, table) = match_table(&alphabet);
		for (i, a) in patterns.iter().enumerate() {
			for (j, b) in patterns.iter().enumerate() {
				let expected = table[i].iter().zip(&table[j]).any(|(a, b)| *a && *b);
				assert_eq!(a.overlaps(b), expected, "{a} overlaps {b}");
			}
		}
	}
}

#[test]
fn specificity_agrees_with_strict_containment() {
	for alphabet in alphabets() {
		for a in alphabet.patterns() {
			for b in alphabet.patterns() {
				if a.contains(&b) && !b.contains(&a) {
					assert!(
						a.specificity() < b.specificity(),
						"{a} strictly contains {b} but does not rank below it"
					);
				}
			}
		}
	}
}

#[test]
fn rebase_matches_exactly_what_lies_beneath_the_root() {
	for alphabet in alphabets() {
		let roots = alphabet.paths(2);
		let relative = alphabet.paths(3);
		for pattern in alphabet.patterns() {
			for root in &roots {
				let rebased = pattern.rebase(root);
				for rel in &relative {
					assert_eq!(
						rebased.matches(rel),
						pattern.matches(&join(root, rel)),
						"{pattern} rebased at {root:?} on {rel:?}"
					);
				}
				// The rebased union is reduced: no member contains another.
				for a in rebased.iter() {
					for b in rebased.iter() {
						assert!(a == b || !a.contains(b), "{rebased:?} is not reduced");
					}
				}
			}
		}
	}
}

#[test]
fn rooted_inverts_rebase() {
	for alphabet in alphabets() {
		for pattern in alphabet.patterns() {
			for root in alphabet.paths(2) {
				let rooted = pattern.rooted(&root).unwrap();
				assert!(rooted.rebase(&root).contains(&pattern), "{pattern} under {root:?}");
				for p in alphabet.paths(3) {
					assert_eq!(
						rooted.matches(&join(&root, &p)),
						pattern.matches(&p),
						"{rooted} on {p:?}"
					);
				}
			}
		}
	}
}

#[test]
fn union_matches_exactly_its_members() {
	for alphabet in alphabets() {
		let all = alphabet.patterns();
		let paths = alphabet.paths(3);
		// Every pair, which is enough to exercise reduction in both directions.
		for (i, a) in all.iter().enumerate() {
			for b in &all[i..] {
				let set: Patterns = [a.clone(), b.clone()].into_iter().collect();
				for p in &paths {
					assert_eq!(set.matches(p), a.matches(p) || b.matches(p), "{{{a}, {b}}} on {p:?}");
				}
				assert!(set.len() <= 2);
				assert_eq!(set.len() == 1, a.contains(b) || b.contains(a), "{{{a}, {b}}} reduction");
			}
		}
	}
}

#[test]
fn text_round_trips_through_parse() {
	for alphabet in alphabets() {
		for pattern in alphabet.patterns() {
			let text = pattern.to_string();
			assert_eq!(text.parse::<Pattern>().unwrap(), pattern);
			assert_eq!(Pattern::new(pattern.segments().to_vec()).unwrap(), pattern);
		}
	}
}

/// Random strings, parsed and printed, to catch a parse that accepts what print
/// cannot reproduce. A tiny xorshift keeps this free of a dependency.
#[test]
fn random_text_round_trips() {
	let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
	let mut next = move || {
		state ^= state << 13;
		state ^= state >> 7;
		state ^= state << 17;
		state
	};
	const PIECES: &[&str] = &["a", "b", "*", "**", "/", "", "a*", ".hang", "//", "*."];
	for _ in 0..20_000 {
		let len = (next() % 8) as usize;
		let text: String = (0..len)
			.map(|_| PIECES[(next() % PIECES.len() as u64) as usize])
			.collect();
		if let Ok(pattern) = text.parse::<Pattern>() {
			assert_eq!(pattern.to_string().parse::<Pattern>().unwrap(), pattern);
			assert_eq!(text.parse::<Pattern>().unwrap(), pattern);
			assert!(pattern.contains(&pattern) && pattern.overlaps(&pattern));
		}
	}
}

#[test]
fn equivalent_patterns_have_one_identity() {
	for alphabet in alphabets() {
		let all = alphabet.patterns();
		for a in &all {
			for b in &all {
				assert_eq!(a == b, a.contains(b) && b.contains(a), "{a} and {b}");
				let forward: Patterns = [a.clone(), b.clone()].into_iter().collect();
				let reverse: Patterns = [b.clone(), a.clone()].into_iter().collect();
				assert_eq!(forward, reverse);
			}
		}
	}
}
