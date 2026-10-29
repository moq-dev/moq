//! Generated codec traces checked against an unbounded logical window.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

use proptest::prelude::*;
use proptest::test_runner::{Config, RngSeed, TestCaseResult, TestRunner};
use serde_json::Value;

use super::{ConsumerConfig, Decoder, Encoded, Encoder, Event, ProducerConfig};

// Mirror the JS generator, including nulls and keys that are special to JS objects.
fn value() -> BoxedStrategy<Value> {
	let key = prop_oneof![
		proptest::sample::select(vec![
			"a",
			"b",
			"c",
			"",
			"__proto__",
			"constructor",
			"prototype",
			"\0",
			"雪"
		])
		.prop_map(str::to_owned),
		"[ -~]{0,8}",
	];
	let leaf = prop_oneof![
		Just(Value::Null),
		any::<bool>().prop_map(Value::Bool),
		(-1_000_000i32..=1_000_000).prop_map(|n| Value::from(f64::from(n) / 4.0)),
		prop_oneof!["[ -~]{0,16}", Just("雪\0".to_owned())].prop_map(Value::String),
	];
	leaf.prop_recursive(4, 64, 4, move |inner| {
		prop_oneof![
			proptest::collection::vec(inner.clone(), 0..5).prop_map(Value::Array),
			proptest::collection::btree_map(key.clone(), inner, 0..5)
				.prop_map(|map| Value::Object(map.into_iter().collect())),
		]
	})
	.boxed()
}

#[derive(Clone, Debug)]
enum Edit {
	Push(Value),
	Pop(u64),
	/// Restate the window with [`Encoder::snapshot`], as a refresh does.
	Restate,
}

#[derive(Clone, Debug)]
struct Step {
	edit: Edit,
	reset: bool,
	drop: bool,
}

struct Frame {
	encoded: Encoded,
	offset: u64,
	records: Vec<Value>,
}

struct Group {
	frames: Vec<Frame>,
	drop: bool,
}

// Group consecutive indices into public event ranges without reproducing codec cursors.
fn ranges(indices: impl IntoIterator<Item = u64>) -> Vec<Range<u64>> {
	let mut result: Vec<Range<u64>> = Vec::new();
	for index in indices {
		if let Some(last) = result.last_mut()
			&& last.end == index
		{
			last.end += 1;
		} else {
			result.push(index..index + 1);
		}
	}
	result
}

fn check(config: ProducerConfig, steps: Vec<Step>, join: usize) -> TestCaseResult {
	let mut encoder = Encoder::<Value>::new(config.clone());
	let mut groups: Vec<Group> = Vec::new();
	let mut offset = 0;
	let mut records = Vec::new();
	for step in steps {
		if step.reset {
			encoder.reset();
		}
		{
			let frame = match step.edit {
				Edit::Push(value) => {
					let frame = encoder.push(&value).unwrap();
					records.push(value);
					Some(frame)
				}
				Edit::Pop(count) => {
					let frame = encoder.pop(count).unwrap();
					let count = (count as usize).min(records.len());
					prop_assert_eq!(frame.is_some(), count > 0);
					offset += count as u64;
					records.drain(..count);
					frame
				}
				Edit::Restate => {
					let frame = encoder.snapshot().unwrap();
					prop_assert!(frame.keyframe);
					Some(frame)
				}
			};
			if let Some(frame) = frame {
				if frame.keyframe {
					groups.push(Group {
						frames: Vec::new(),
						drop: step.drop,
					});
				}
				groups
					.last_mut()
					.expect("first frame must open a group")
					.frames
					.push(Frame {
						encoded: (*frame).clone(),
						offset,
						records: records.clone(),
					});
				frame.commit();
			}
		}
		prop_assert_eq!(encoder.range(), offset..offset + records.len() as u64);
	}
	if groups.is_empty() {
		return Ok(());
	}

	for mode in ["following", "late", "lagging"] {
		let mut decoder = Decoder::<Value>::new(ConsumerConfig::default());
		let mut notified = BTreeSet::new();
		let mut received = BTreeSet::new();
		let mut contents = BTreeMap::new();
		let mut timeline = Vec::new();
		let mut floor = None;
		let start_group = if mode == "late" { join % groups.len() } else { 0 };
		for (g, encoded) in groups.iter().enumerate() {
			if g < start_group || (mode == "lagging" && g > 0 && g < groups.len() - 1 && encoded.drop) {
				continue;
			}
			let mut group = decoder.group();
			for (f, frame) in encoded.frames.iter().enumerate() {
				let floor = *floor.get_or_insert(frame.offset);
				let mut expected: Vec<_> = ranges(timeline.iter().copied().filter(|index| *index < frame.offset))
					.into_iter()
					.map(Event::Pop)
					.collect();
				let end = frame.offset + frame.records.len() as u64;
				let start = if f == 0 {
					frame
						.offset
						.max(end.saturating_sub(config.checkpoint_records.unwrap_or(frame.records.len()) as u64))
				} else {
					frame.offset
				};
				let missing: Vec<_> = if f == 0 {
					(floor..start).filter(|index| !notified.contains(index)).collect()
				} else {
					Vec::new()
				};
				expected.extend(ranges(missing.iter().copied()).into_iter().map(Event::Skip));
				notified.extend(missing);
				for (i, value) in frame.records.iter().enumerate() {
					let index = frame.offset + i as u64;
					if index >= start && notified.insert(index) {
						expected.push(Event::Push {
							index,
							value: value.clone(),
						});
					}
				}
				group.decode(&frame.encoded.payload).unwrap();
				let events: Vec<_> = std::iter::from_fn(|| group.next_event()).collect();
				prop_assert_eq!(&events, &expected, "{} group {} frame {}", mode, g, f);
				for event in events {
					match event {
						Event::Push { index, value } => {
							prop_assert!(received.insert(index), "duplicate push {}", index);
							contents.insert(index, value);
						}
						Event::Pop(range) => {
							for index in range {
								contents.remove(&index);
							}
						}
						Event::Skip(range) => {
							for index in range {
								prop_assert!(!received.contains(&index), "skipped received {}", index);
							}
						}
					}
				}
				timeline = (frame.offset..end).collect();
				prop_assert_eq!(group.range(), frame.offset..end);
				for (index, value) in &contents {
					prop_assert!(*index >= frame.offset && *index < end);
					prop_assert_eq!(value, &frame.records[(*index - frame.offset) as usize]);
				}
				if mode == "following" {
					prop_assert_eq!(
						contents.values().collect::<Vec<_>>(),
						frame.records.iter().collect::<Vec<_>>()
					);
				}
			}
		}
	}
	Ok(())
}

#[test]
fn window_codec_property_matches_every_readers_logical_window() {
	let setting = |name: &str, fallback: u32, min: u32| {
		let value = std::env::var(name).map_or(fallback, |value| {
			value.parse::<u32>().expect("integer property setting")
		});
		assert!(
			(min..=i32::MAX as u32).contains(&value),
			"{name} outside supported range"
		);
		value
	};
	let seed = setting("MOQ_JSON_SEED", 0x6d6f71, 0);
	let cases = setting("MOQ_JSON_CASES", 256, 1);
	eprintln!("MOQ_JSON_SEED={seed} MOQ_JSON_CASES={cases}");
	let mut runner = TestRunner::new(Config {
		cases,
		rng_seed: RngSeed::Fixed(u64::from(seed)),
		failure_persistence: None,
		..Default::default()
	});
	let steps = proptest::collection::vec(
		(
			prop_oneof![
				4 => value().prop_map(Edit::Push),
				4 => (0u64..=8).prop_map(Edit::Pop),
				1 => Just(Edit::Restate),
			],
			0u8..16,
			any::<bool>(),
		)
			.prop_map(|(edit, reset, drop)| Step {
				edit,
				reset: reset == 0,
				drop,
			}),
		1..65,
	);
	let sequence = (
		(any::<bool>(), proptest::option::of(1usize..=8)).prop_map(|(ops, checkpoint_records)| ProducerConfig {
			ops,
			checkpoint_records,
		}),
		steps,
		0usize..64,
	);
	runner
		.run(&sequence, |(config, steps, join)| check(config, steps, join))
		.unwrap();
}
