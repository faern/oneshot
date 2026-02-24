#[cfg(not(any(oneshot_loom, feature = "extra-platforms")))]
pub use core::sync::atomic::{AtomicU8, Ordering, fence};
#[cfg(feature = "extra-platforms")]
pub use extra_platforms::{AtomicU8, Ordering, fence};
#[cfg(oneshot_loom)]
pub use loom::sync::atomic::{AtomicU8, Ordering, fence};
