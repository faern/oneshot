use core::ptr::NonNull;

use crate::{Channel, StoragePrivate};

#[cfg(not(oneshot_loom))]
use crate::alloc::boxed::Box;
#[cfg(oneshot_loom)]
use crate::loombox::Box;

/// Marker type indicating that the inner state storage of a channel is allocated via the
/// Rust global allocator.
#[derive(Debug)]
pub struct Global<T> {
    // Not actually a marker type but as far as the public API is concerned,
    // it is a marker type because it is never created/referenced by user code.
    ptr: NonNull<Channel<T>>,
}

impl<T> Global<T> {
    pub(crate) fn new() -> Self {
        let ptr = NonNull::from(Box::leak(Box::new(Channel::new())));

        Global { ptr }
    }

    /// Obtains the raw heap pointer that this storage object wraps.
    ///
    /// Using this pointer, an equivalent storage object be reconstructed with `Global::from_raw()`.
    pub(crate) fn to_raw(&self) -> NonNull<Channel<T>> {
        self.ptr
    }

    /// Reconstructs a storage object previously created with `to_raw()`.
    ///
    /// # Safety
    ///
    /// All the type invariants must remain in place - the recreated storage object
    /// rejoins the same family of clones that it was created from.
    pub(crate) unsafe fn from_raw(raw: NonNull<Channel<T>>) -> Self {
        Global { ptr: raw }
    }
}

// SAFETY: We implement the "this is just a fancy pointer" model as required by the trait.
unsafe impl<T> StoragePrivate<T> for Global<T> {
    unsafe fn as_ref(&self) -> &Channel<T> {
        // SAFETY: Yes, our pointer is valid and points to a `Channel<T>`.
        // The caller is responsible for ensuring that `initialize()` has been called.
        unsafe { self.ptr.as_ref() }
    }

    unsafe fn release(&mut self) {
        dealloc(self.ptr);

        // We rely on safety requirements to ensure this is never used again.
        self.ptr = NonNull::dangling();
    }

    fn clone(&self) -> Self {
        Global { ptr: self.ptr }
    }
}

#[inline]
unsafe fn dealloc<T>(channel: NonNull<Channel<T>>) {
    drop(Box::from_raw(channel.as_ptr()))
}
