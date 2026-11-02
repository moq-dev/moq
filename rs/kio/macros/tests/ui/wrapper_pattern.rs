// An argument destructured in place: the wrapper has no name to pass it by.
use std::task::Poll;

struct S;

impl S {
	#[kio::wrapper]
	fn poll_pair(&self, _waiter: &kio::Waiter, (a, b): (u8, u8)) -> Poll<u8> {
		Poll::Ready(a + b)
	}
}

fn main() {}
