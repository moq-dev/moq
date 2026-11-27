// A method that is not ready with a value: no return type, or not a `Poll<T>`.

struct S;

impl S {
	#[kio::wrapper]
	fn poll_nothing(&self, _waiter: &kio::Waiter) {}

	#[kio::wrapper]
	fn poll_option(&self, _waiter: &kio::Waiter) -> Option<()> {
		None
	}
}

fn main() {}
