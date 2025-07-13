use crate::Channel;

/// The mechanism used to manage the storage of the inner state of a channel of `T`.
#[expect(private_bounds, reason = "sealed trait with private API surface")]
pub trait Storage<T>: StoragePrivate<T> {}

/// Usage model is to treat the implementing type as if it were a `NonNull<Channel<T>>`.
///
/// That is, it can be cloned freely and every clone (pointer) points to the same underlying data.
/// Dropping the object itself only drops the object (pointer), not the thing it points to.
/// To drop the data within and release the storage capacity, `release()` must be called explicitly.
///
/// # Safety
///
/// Implementations must implement the usage model described above, acting as pointers.
pub(crate) unsafe trait StoragePrivate<T> {
    /// Releases the capacity that provides this storage.
    ///
    /// This will drop the `Channel<T>` and invalidate all clones of this storage.
    ///
    /// This must be called exactly once for each family of clones to avoid resource leaks.
    ///
    /// # Safety
    ///
    /// This must not be called multiple times on the same family of clones.
    unsafe fn release(&mut self);

    /// Dereferences the stored `Channel<T>`.
    ///
    /// # Safety
    ///
    /// The caller must guarantee that `release()` has not been called on any storage
    /// object in the same family of clones.
    unsafe fn as_ref(&self) -> &Channel<T>;

    /// Clones the storage, returning a new instance that points to the same underlying data.
    ///
    /// This is implemented as an inherent method to avoid exposing the `Clone` trait
    /// to users of implementation types outside this crate. Only the logic in this crate
    /// should be cloning the storage objects.
    fn clone(&self) -> Self;
}

impl<S: StoragePrivate<T>, T> Storage<T> for S {}
