use std::{
    fmt::Debug,
    ops::{Deref, DerefMut},
};

mod non_tracking {
    use super::*;

    /// A Mutex which optionally allows to track the time a lock was held and
    /// emit warnings in case of excessive lock times
    #[derive(Debug)]
    pub(crate) struct Mutex<T> {
        inner: std::sync::Mutex<T>,
    }

    impl<T> Mutex<T> {
        pub(crate) fn new(value: T) -> Self {
            Self {
                inner: std::sync::Mutex::new(value),
            }
        }

        /// Acquires the lock for a certain purpose
        ///
        /// The purpose will be recorded in the list of last lock owners
        pub(crate) fn lock(&self, _purpose: &'static str) -> MutexGuard<'_, T> {
            MutexGuard {
                guard: self.inner.lock().unwrap(),
            }
        }
    }

    pub(crate) struct MutexGuard<'a, T> {
        guard: std::sync::MutexGuard<'a, T>,
    }

    impl<T> Deref for MutexGuard<'_, T> {
        type Target = T;

        fn deref(&self) -> &Self::Target {
            self.guard.deref()
        }
    }

    impl<T> DerefMut for MutexGuard<'_, T> {
        fn deref_mut(&mut self) -> &mut Self::Target {
            self.guard.deref_mut()
        }
    }
}

pub(crate) use non_tracking::{Mutex, MutexGuard};
