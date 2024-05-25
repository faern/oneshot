#[cfg(all(feature = "std", not(loom)))]
use std::thread;

#[cfg(all(feature = "std", loom))]
use loom::thread;

#[cfg(feature = "async")]
use core::task;

pub trait Waker: private::Sealed {
    fn wake(self);
}

/// A waker that can be used in synchronous operations
///
/// It is assumed that the [`wake`](Waker::wake) method
/// of a [SyncWaker] will always call [`thread::unpark()`](std::thread::unpark).
pub trait SyncWaker: Waker {
    fn new() -> Self;
}

#[cfg(feature = "async")]
/// A waker that can be used in async operations
pub trait AsyncWaker: Waker {
    fn new(ctx: &task::Context<'_>) -> Self;
}

#[cfg(feature = "std")]
impl Waker for thread::Thread {
    fn wake(self) {
        self.unpark()
    }
}

#[cfg(feature = "std")]
impl SyncWaker for thread::Thread {
    fn new() -> Self {
        thread::current()
    }
}

#[cfg(feature = "async")]
impl Waker for task::Waker {
    fn wake(self) {
        self.wake()
    }
}

#[cfg(feature = "async")]
impl AsyncWaker for task::Waker {
    fn new(ctx: &task::Context<'_>) -> Self {
        ctx.waker().clone()
    }
}

/// A Waker that can be used in both sync and async contexts
#[cfg(all(feature = "std", feature = "async"))]
pub enum GenericWaker {
    Sync(thread::Thread),
    Async(task::Waker),
}

#[cfg(all(feature = "std", feature = "async"))]
impl Waker for GenericWaker {
    fn wake(self) {
        match self {
            Self::Sync(sync_waker) => sync_waker.unpark(),
            Self::Async(async_waker) => async_waker.wake(),
        }
    }
}

#[cfg(all(feature = "std", feature = "async"))]
impl SyncWaker for GenericWaker {
    fn new() -> Self {
        Self::Sync(thread::current())
    }
}

#[cfg(all(feature = "std", feature = "async"))]
impl AsyncWaker for GenericWaker {
    fn new(ctx: &task::Context<'_>) -> Self {
        Self::Async(ctx.waker().clone())
    }
}

/// A waker that cannot be waited on
pub struct DummyWaker;

impl Waker for DummyWaker {
    fn wake(self) {}
}

// FIXME: Once cfg_match! is within MSRV, use it here!

#[cfg(all(feature = "std", feature = "async"))]
pub type DefaultWaker = GenericWaker;

#[cfg(all(not(feature = "std"), feature = "async"))]
pub type DefaultWaker = task::Waker;

#[cfg(all(feature = "std", not(feature = "async")))]
pub type DefaultWaker = thread::Thread;

#[cfg(all(not(feature = "std"), not(feature = "async")))]
pub type DefaultWaker = DummyWaker;

mod private {
    pub trait Sealed {}
}

impl private::Sealed for DummyWaker {}

#[cfg(feature = "std")]
impl private::Sealed for thread::Thread {}

#[cfg(feature = "async")]
impl private::Sealed for task::Waker {}

#[cfg(all(feature = "std", feature = "async"))]
impl private::Sealed for GenericWaker {}
