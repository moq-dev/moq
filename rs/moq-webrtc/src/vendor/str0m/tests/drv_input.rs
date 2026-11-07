#![cfg(feature = "drv")]
//! Verifies the `drv` feature derives `drv::Input` on str0m's public
//! identity types so downstream `#[drv::memo]` queries can take them by
//! value. `Frequency` exercises the `NonZeroU32` path specifically.

use std::collections::HashMap;

use str0m::format::Codec;
use str0m::media::{Frequency, MediaKind, MediaTime, Mid};

#[derive(drv::Input)]
struct TrackFacts {
    pub mid: Mid,
    pub kind: MediaKind,
    pub codec: Codec,
    pub clock_rate: Frequency,
}

#[drv::memo(single)]
fn is_opus_audio(input: TrackFacts) -> bool {
    matches!(input.kind, MediaKind::Audio) && matches!(input.codec, Codec::Opus)
}

#[test]
fn identity_types_are_valid_memo_inputs() {
    let mid = Mid::from("0");
    let audio = TrackFacts {
        mid,
        kind: MediaKind::Audio,
        codec: Codec::Opus,
        clock_rate: Frequency::FORTY_EIGHT_KHZ,
    };
    assert!(is_opus_audio(audio));

    let video = TrackFacts {
        mid,
        kind: MediaKind::Video,
        codec: Codec::Vp8,
        clock_rate: Frequency::NINETY_KHZ,
    };
    assert!(!is_opus_audio(video));
}

#[derive(drv::Input)]
struct TimeFacts {
    pub at: MediaTime,
}

#[drv::memo(single)]
fn micros(input: TimeFacts) -> i64 {
    // identity work; the point is that MediaTime is a valid memo input
    input.at.rebase(Frequency::MICROS).numer() as i64
}

#[test]
fn media_time_is_a_valid_memo_input() {
    // Same instant expressed in two timebases — MediaTime's PartialEq
    // (which the ToStatic impl defers to) treats them equal.
    let a = MediaTime::new(2_000, Frequency::MILLIS);
    let b = MediaTime::new(2_000_000, Frequency::MICROS);
    assert_eq!(a, b);
    assert_eq!(micros(TimeFacts { at: a }), 2_000_000);
    assert_eq!(micros(TimeFacts { at: b }), 2_000_000);
}

#[derive(drv::Input)]
struct MidMapInput<'a> {
    pub by_mid: &'a HashMap<Mid, u32>,
}

#[drv::memo(single)]
fn mid_count(input: MidMapInput<'_>) -> usize {
    input.by_mid.len()
}

#[test]
fn mid_works_as_a_projected_hashmap_key() {
    let mut by_mid = HashMap::new();
    by_mid.insert(Mid::from("0"), 1u32);
    by_mid.insert(Mid::from("1"), 2u32);
    assert_eq!(mid_count(MidMapInput { by_mid: &by_mid }), 2);
}
