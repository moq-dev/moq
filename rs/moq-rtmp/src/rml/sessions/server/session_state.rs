pub enum SessionState {
	Started,
	/// A connect was accepted for `app_name`.
	Connected {
		app_name: String,
	},
	Completed,
}
