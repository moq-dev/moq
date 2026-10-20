//! Carry replaceable control state and ordered commands on separate MoQ tracks.
//!
//! Run `cargo run -p moq-json --example teleop`. This in-process example teaches the
//! framing; attach the same broadcast to a moq-tokio origin to carry it over a session.
use std::task::Poll;

use moq_json::{snapshot, stream};
use moq_net::{Timed, Timescale, Timestamp, broadcast, track};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Control {
	speed: i32,
	turn: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
enum Command {
	Arm,
	Stop,
	Disarm,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
	let broadcast = broadcast::Info::new().produce();
	let info = track::Info::default().with_timescale(Timescale::MILLI);
	let state = broadcast.create_track("control.json", info.clone())?;
	let commands = broadcast.create_track("commands.json", info)?;
	let mut state_reader = snapshot::Consumer::<Control>::new(state.subscribe(None), Default::default());
	let mut command_reader = stream::Consumer::<Command>::new(commands.subscribe(None), Default::default());
	let mut state = snapshot::Producer::new(state, Default::default());
	let mut commands = stream::Producer::new(commands, Default::default());

	// The reader falls behind. Replaying the old steering values would be harmful.
	for (tick, control) in [
		Control { speed: 1, turn: -1 },
		Control { speed: 2, turn: 1 },
		Control { speed: 0, turn: 0 },
	]
	.iter()
	.enumerate()
	{
		state.update(Timed::from(control).at(Timestamp::from_millis(tick as u64)?))?;
	}
	state.finish()?;

	// Every discrete command is meaningful, including Stop between Arm and Disarm.
	let expected = [Command::Arm, Command::Stop, Command::Disarm];
	for (tick, command) in expected.iter().enumerate() {
		commands.append(Timed::from(command).at(Timestamp::from_millis(tick as u64)?))?;
	}
	commands.finish()?;

	let waiter = kio::Waiter::noop();
	// `latest` skips the stale steering values; `next` would replay them in order.
	let Poll::Ready(Ok(Some(latest))) = state_reader.poll_latest(&waiter) else {
		return Err("finished control track did not yield its latest state".into());
	};
	assert_eq!(latest.value, Control { speed: 0, turn: 0 });
	assert_eq!(latest.at, Some(Timestamp::from_millis(2)?));
	println!("Current control: {:?} at {:?}", latest.value, latest.at);
	assert!(matches!(state_reader.poll_latest(&waiter), Poll::Ready(Ok(None))));
	for (tick, expected) in expected.into_iter().enumerate() {
		let Poll::Ready(Ok(Some(command))) = command_reader.poll_next(&waiter) else {
			return Err("finished command track lost an ordered command".into());
		};
		assert_eq!(command.value, expected);
		assert_eq!(command.at, Some(Timestamp::from_millis(tick as u64)?));
		println!("Command: {:?} at {:?}", command.value, command.at);
	}
	assert!(matches!(command_reader.poll_next(&waiter), Poll::Ready(Ok(None))));
	Ok(())
}

#[cfg(test)]
#[test]
fn latest_state_and_every_command() {
	main().unwrap();
}
