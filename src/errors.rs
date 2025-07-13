use crate::Channel;
use crate::Global;
use crate::Storage;

use core::fmt;
use core::marker::PhantomData;
use core::mem;

/// An error returned when trying to send on a closed channel. Returned from
/// [`Sender::send`](crate::Sender::send) if the corresponding [`Receiver`](crate::Receiver)
/// has already been dropped.
///
/// The message that could not be sent can be retreived again with [`SendError::into_inner`].
pub struct SendError<T, S: Storage<T> = Global<T>> {
    storage: S,

    _t: PhantomData<T>,
}

unsafe impl<T: Send, S: Storage<T>> Send for SendError<T, S> {}
unsafe impl<T: Sync, S: Storage<T>> Sync for SendError<T, S> {}

impl<T, S: Storage<T>> SendError<T, S> {
    /// # Safety
    ///
    /// By calling this function, the caller semantically transfers ownership of the
    /// channel's resources to the created `SendError`. Thus the caller must ensure that the
    /// pointer is not used in a way which would violate this ownership transfer. Moreover,
    /// the caller must assert that the channel contains a valid, initialized message.
    pub(crate) const unsafe fn new(storage: S) -> Self {
        Self {
            storage,
            _t: PhantomData,
        }
    }

    /// Consumes the error and returns the message that failed to be sent.
    #[inline]
    pub fn into_inner(self) -> T {
        let mut storage = self.storage.clone();

        // Don't run destructor if we consumed ourselves. Freeing happens here.
        mem::forget(self);

        // SAFETY: we have ownership of the channel
        let channel: &Channel<T> = unsafe { storage.as_ref() };

        // SAFETY: we know that the message is initialized according to the safety requirements of
        // `new`
        let message = unsafe { channel.take_message() };

        // SAFETY: we own the channel
        unsafe { storage.release() };

        message
    }

    /// Get a reference to the message that failed to be sent.
    #[inline]
    pub fn as_inner(&self) -> &T {
        unsafe { self.storage.as_ref().message().assume_init_ref() }
    }
}

impl<T, S: Storage<T>> Drop for SendError<T, S> {
    fn drop(&mut self) {
        // SAFETY: we have ownership of the channel and require that the message is initialized
        // upon construction
        unsafe {
            self.storage.as_ref().drop_message();
            self.storage.release();
        }
    }
}

impl<T, S: Storage<T>> fmt::Display for SendError<T, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        "sending on a closed channel".fmt(f)
    }
}

impl<T, S: Storage<T>> fmt::Debug for SendError<T, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SendError<{}>(_)", stringify!(T))
    }
}

#[cfg(feature = "std")]
impl<T, S: Storage<T>> std::error::Error for SendError<T, S> {}

/// An error returned from receiving methods that block/wait until a message is available.
///
/// The receive operation can only fail if the corresponding [`Sender`](crate::Sender) was dropped
/// before sending any message, or if a message has already been received on the channel.
#[cfg(any(feature = "std", feature = "async"))]
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub struct RecvError;

#[cfg(any(feature = "std", feature = "async"))]
impl fmt::Display for RecvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        "receiving on a closed channel".fmt(f)
    }
}

#[cfg(feature = "std")]
impl std::error::Error for RecvError {}

/// An error returned when failing to receive a message in the non-blocking
/// [`Receiver::try_recv`](crate::Receiver::try_recv).
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum TryRecvError {
    /// The channel is still open, but there was no message present in it.
    Empty,

    /// The channel is closed. Either the sender was dropped before sending any message, or the
    /// message has already been extracted from the receiver.
    Disconnected,
}

impl fmt::Display for TryRecvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = match self {
            TryRecvError::Empty => "receiving on an empty channel",
            TryRecvError::Disconnected => "receiving on a closed channel",
        };
        msg.fmt(f)
    }
}

#[cfg(feature = "std")]
impl std::error::Error for TryRecvError {}

/// An error returned when failing to receive a message in a method that block/wait for a message
/// for a while, but has a timeout after which it gives up.
#[cfg(feature = "std")]
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum RecvTimeoutError {
    /// No message arrived on the channel before the timeout was reached. The channel is still open.
    Timeout,

    /// The channel is closed. Either the sender was dropped before sending any message, or the
    /// message has already been extracted from the receiver.
    Disconnected,
}

#[cfg(feature = "std")]
impl fmt::Display for RecvTimeoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = match self {
            RecvTimeoutError::Timeout => "timed out waiting on channel",
            RecvTimeoutError::Disconnected => "channel is empty and sending half is closed",
        };
        msg.fmt(f)
    }
}

#[cfg(feature = "std")]
impl std::error::Error for RecvTimeoutError {}
