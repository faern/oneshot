use core::{cell::UnsafeCell, ptr::NonNull};

use crate::{Channel, StoragePrivate};

/// Marker type indicating that the inner state storage of a channel is provided externally
/// to the `oneshot` crate by the creator of the channel.
///
/// This allows for advanced storage strategies such as embedding the inner state inside another
/// type and reusing the storage for the inner state across multiple consecutive channels.
///
/// The owner of the channel must provide a pointer to a `NonNull<ChannelStorage<T>>` and a function
/// to call when the storage is no longer needed. What the `oneshot` crate does with the storage
/// object is opaque to an external observer and is equivalent to a `&mut` exclusive borrow of the
/// storage for the duration of the channel's lifetime and any associated error types (i.e. until
/// the `release` fn is called).
pub struct External<T> {
    // Not actually a marker type but as far as the public API is concerned,
    // it is a marker type because it is never created/referenced by user code.
    ptr: NonNull<ChannelStorage<T>>,
    release: fn(NonNull<ChannelStorage<T>>),
}

impl<T> External<T> {
    /// # Safety
    ///
    /// The caller must guarantee that the provided `ChannelStorage<T>` pointer is valid and
    /// the storage is not concurrently in use by any other `oneshot` channel.
    ///
    /// The caller must guarantee that no `&mut` exclusive references are created to the
    /// `ChannelStorage<T>` (including to its parent object if embedded into another type)
    /// for the lifetime of the channel and any associated error types (i.e. until
    /// the `release` fn is called).
    pub(crate) unsafe fn new(
        storage: NonNull<ChannelStorage<T>>,
        release: fn(NonNull<ChannelStorage<T>>),
    ) -> Self {
        // SAFETY: The caller guarantees that no `&mut` exclusive references
        // exist, so creating a shared reference is valid.
        let storage_ref = unsafe { storage.as_ref() };

        // The first thing we need to do is initialize the storage with a clean `Channel<T>`.
        storage_ref.channel.get().write(Channel::new());

        External {
            ptr: storage,
            release,
        }
    }
}

/// Provides externally managed storage for a `oneshot` channel of `T`.
///
/// This may be embedded into another type and/or reused across multiple channels to reduce
/// memory allocation pressure and improve the efficiency of using `oneshot` channels.
pub struct ChannelStorage<T> {
    // The `StoragePrivate` API contract requires the channel to explicitly call `release()` when
    // this needs to be cleaned up, so unless someone calls `release()`, we will assume this is
    // an inert `Channel<T>` (either never used or already released) that does not require cleanup
    // beyond being simply dropped (i.e. that its `MaybeUninit` fields are not initialized).
    channel: UnsafeCell<Channel<T>>,
}

impl<T> ChannelStorage<T> {
    pub fn new() -> Self {
        ChannelStorage {
            channel: UnsafeCell::new(Channel::new()),
        }
    }
}

impl<T> Default for ChannelStorage<T> {
    fn default() -> Self {
        Self::new()
    }
}

// SAFETY: We implement the "this is just a fancy pointer" model as required by the trait.
unsafe impl<T> StoragePrivate<T> for External<T> {
    unsafe fn release(&mut self) {
        // The caller itself manages the `Channel<T>` within, all we need to do is inform
        // the owner that their storage is no longer needed by us.
        (self.release)(self.ptr);

        #[cfg(debug_assertions)]
        {
            // Just to make any anomalies easier to detect.
            self.ptr = NonNull::dangling();
        }
    }

    unsafe fn as_ref(&self) -> &Channel<T> {
        // SAFETY: `External::new()` requires a guarantee that no `&mut` exclusive references
        // exist, so creating a shared reference is valid.
        let storage = unsafe { self.ptr.as_ref() };

        // SAFETY: We only ever create shared references to the `Channel<T>`, so this is valid.
        unsafe { &*storage.channel.get() }
    }

    fn clone(&self) -> Self {
        External {
            ptr: self.ptr,
            release: self.release,
        }
    }
}
