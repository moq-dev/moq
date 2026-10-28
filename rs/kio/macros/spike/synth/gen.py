"""Generate src/lib.rs: N flat machines or a D-deep chain, in one style.

usage: gen.py <hand|macro|async> <flat|chain> <count> [tag]

Every flat machine waits five times, keeping a u64, a String, and a Vec across
waits. Every chain level holds the level below as a local across its first wait.
`tag` changes one string in machine 0's body, for the rmeta test.
"""

import sys

style, shape, count = sys.argv[1], sys.argv[2], int(sys.argv[3])
tag = sys.argv[4] if len(sys.argv) > 4 else ""

PRELUDE = """#![allow(clippy::all, unused_imports)]
use std::task::{Poll, ready};

#[derive(Debug)]
pub struct Error;

/// Ready on every other poll.
#[derive(Default)]
pub struct Source {
	n: u64,
}

impl Source {
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Result<u64, Error>> {
		self.n += 1;
		if self.n % 2 == 1 {
			waiter.waker().wake_by_ref();
			return Poll::Pending;
		}
		Poll::Ready(Ok(self.n))
	}
}

pub fn assert_send<T: Send>(_: &T) {}

#[allow(unused_macros)]
macro_rules! block_on {
	($poll:expr) => {{
		let waiter = kio::Waiter::noop();
		loop {
			if let Poll::Ready(value) = $poll(&waiter) {
				break value;
			}
		}
	}};
}
"""


def flat_hand(i, label):
    return f"""
pub struct M{i} {{
	src: Source,
	state: M{i}State,
}}

enum M{i}State {{
	A,
	B {{ a: u64, label: String }},
	C {{ a: u64, label: String, b: u64 }},
	D {{ label: String, v: Vec<u64> }},
	E {{ label: String, v: Vec<u64>, d: u64 }},
	Done,
}}

impl M{i} {{
	pub fn new() -> Self {{
		Self {{ src: Source::default(), state: M{i}State::A }}
	}}

	pub fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<u64, Error>> {{
		loop {{
			match &mut self.state {{
				M{i}State::A => {{
					let a = ready!(self.src.poll_next(waiter))?;
					let label = format!("{label}-{{a}}");
					self.state = M{i}State::B {{ a, label }};
				}}
				M{i}State::B {{ .. }} => {{
					let b = ready!(self.src.poll_next(waiter))?;
					let M{i}State::B {{ a, label }} = std::mem::replace(&mut self.state, M{i}State::Done) else {{
						unreachable!()
					}};
					self.state = M{i}State::C {{ a, label, b }};
				}}
				M{i}State::C {{ a, .. }} => {{
					let c = ready!(self.src.poll_next(waiter))? + *a;
					let M{i}State::C {{ a, label, b }} = std::mem::replace(&mut self.state, M{i}State::Done) else {{
						unreachable!()
					}};
					let v = vec![a, b, c];
					self.state = M{i}State::D {{ label, v }};
				}}
				M{i}State::D {{ .. }} => {{
					let d = ready!(self.src.poll_next(waiter))?;
					let M{i}State::D {{ label, v }} = std::mem::replace(&mut self.state, M{i}State::Done) else {{
						unreachable!()
					}};
					self.state = M{i}State::E {{ label, v, d }};
				}}
				M{i}State::E {{ label, v, d }} => {{
					let e = ready!(self.src.poll_next(waiter))?;
					let total = label.len() as u64 + v.iter().sum::<u64>() + *d + e;
					self.state = M{i}State::Done;
					return Poll::Ready(Ok(total));
				}}
				M{i}State::Done => panic!("polled after returning `Poll::Ready`"),
			}}
		}}
	}}
}}
"""


def flat_macro(i, label):
    return f"""
pub struct M{i} {{
	src: Source,
	state: M{i}State,
}}

#[kio::machine(self.state: M{i}State)]
impl M{i} {{
	pub fn new() -> Self {{
		Self {{ src: Source::default(), state: M{i}State::Start }}
	}}

	pub fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<u64, Error>> {{
		let a: u64 = wait!(A => self.src.poll_next(waiter))?;
		let label: String = format!("{label}-{{a}}");
		let b: u64 = wait!(B => self.src.poll_next(waiter))?;
		let c = wait!(C => self.src.poll_next(waiter))? + a;
		let v: Vec<u64> = vec![a, b, c];
		let d: u64 = wait!(D => self.src.poll_next(waiter))?;
		let e = wait!(E => self.src.poll_next(waiter))?;
		Poll::Ready(Ok(label.len() as u64 + v.iter().sum::<u64>() + d + e))
	}}
}}
"""


def flat_async(i, label):
    return f"""
pub async fn m{i}(src: &mut Source) -> Result<u64, Error> {{
	let a = kio::wait(|w| src.poll_next(w)).await?;
	let label = format!("{label}-{{a}}");
	let b = kio::wait(|w| src.poll_next(w)).await?;
	let c = kio::wait(|w| src.poll_next(w)).await? + a;
	let v = vec![a, b, c];
	let d = kio::wait(|w| src.poll_next(w)).await?;
	let e = kio::wait(|w| src.poll_next(w)).await?;
	Ok(label.len() as u64 + v.iter().sum::<u64>() + d + e)
}}
"""


def chain_hand(k):
    if k == 0:
        return """
pub struct C0 {
	src: Source,
}

impl C0 {
	pub fn new() -> Self {
		Self { src: Source::default() }
	}

	pub fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<u64, Error>> {
		self.src.poll_next(waiter)
	}
}
"""
    j = k - 1
    return f"""
pub struct C{k} {{
	src: Source,
	state: C{k}State,
}}

enum C{k}State {{
	Inner {{ inner: C{j} }},
	Own {{ x: u64 }},
	Done,
}}

impl C{k} {{
	pub fn new() -> Self {{
		Self {{ src: Source::default(), state: C{k}State::Inner {{ inner: C{j}::new() }} }}
	}}

	pub fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<u64, Error>> {{
		loop {{
			match &mut self.state {{
				C{k}State::Inner {{ inner }} => {{
					let x = ready!(inner.poll(waiter))?;
					self.state = C{k}State::Own {{ x }};
				}}
				C{k}State::Own {{ x }} => {{
					let y = ready!(self.src.poll_next(waiter))?;
					let total = *x + y;
					self.state = C{k}State::Done;
					return Poll::Ready(Ok(total));
				}}
				C{k}State::Done => panic!("polled after returning `Poll::Ready`"),
			}}
		}}
	}}
}}
"""


def chain_macro(k):
    if k == 0:
        return chain_hand(0)
    j = k - 1
    return f"""
pub struct C{k} {{
	src: Source,
	state: C{k}State,
}}

#[kio::machine(self.state: C{k}State)]
impl C{k} {{
	pub fn new() -> Self {{
		Self {{ src: Source::default(), state: C{k}State::Start }}
	}}

	pub fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<u64, Error>> {{
		let mut inner: C{j} = C{j}::new();
		let x: u64 = wait!(Inner => inner.poll(waiter))?;
		let y = wait!(Own => self.src.poll_next(waiter))?;
		Poll::Ready(Ok(x + y))
	}}
}}
"""


def chain_async(k):
    if k == 0:
        return """
pub async fn c0(src: &mut Source) -> Result<u64, Error> {
	kio::wait(|w| src.poll_next(w)).await
}
"""
    return f"""
pub async fn c{k}(src: &mut Source) -> Result<u64, Error> {{
	let mut inner = Source::default();
	let x = c{k - 1}(&mut inner).await?;
	let y = kio::wait(|w| src.poll_next(w)).await?;
	Ok(x + y)
}}
"""


def test(style, shape, count):
    top = count - 1
    if style == "async":
        name = f"m0" if shape == "flat" else f"c{top}"
        return f"""
#[test]
fn runs() {{
	let mut src = Source::default();
	let mut fut = Box::pin({name}(&mut src));
	assert_send(&fut);
	let value = block_on!(|waiter: &kio::Waiter| waiter.poll_future(fut.as_mut())).unwrap();
	println!("{{value}}");
}}
"""
    name = "M0" if shape == "flat" else f"C{top}"
    return f"""
#[test]
fn runs() {{
	let mut machine = {name}::new();
	assert_send(&machine);
	let value = block_on!(|waiter: &kio::Waiter| machine.poll(waiter)).unwrap();
	println!("{{value}}");
}}
"""


out = [PRELUDE]
for i in range(count):
    label = f"m{i}{tag}" if i == 0 else f"m{i}"
    if shape == "flat":
        out.append({"hand": flat_hand, "macro": flat_macro, "async": flat_async}[style](i, label))
    else:
        out.append({"hand": chain_hand, "macro": chain_macro, "async": chain_async}[style](i))
out.append(test(style, shape, count))
open("src/lib.rs", "w").write("".join(out))
