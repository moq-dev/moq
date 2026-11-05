// A generic impl.
#[allow(unused_imports, reason = "a rejected impl expands to nothing")]
use std::task::Poll;

struct M<T> {
	values: kio::Queue<T>,
	state: MState,
}

#[kio::machine(self.state: MState)]
impl<T> M<T> {
	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<T, kio::Closed>> {
		let a = wait!(A => self.values.poll_pop(waiter))?;
		Poll::Ready(Ok(a))
	}
}

fn main() {}
