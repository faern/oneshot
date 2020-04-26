use super::Box;
use core::fmt;

/// An error returned when trying to send on a closed channel. Returned from
/// [`Sender::send`] if the corresponding [`Receiver`] has already been dropped.
///
/// The message that could not be sent can be retreived again with [`SendError::into_inner`].
#[derive(Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct SendError<T>(Box<T>);

impl<T> SendError<T> {
    pub const fn new(message: Box<T>) -> Self {
        Self(message)
    }

    /// Consumes the error and returns the message that failed to be sent.
    #[inline]
    pub fn into_inner(self) -> T {
        super::take(self.0)
    }

    /// Get a reference to the message that failed to be sent.
    #[inline]
    pub fn as_inner(&self) -> &T {
        &self.0
    }
}

impl<T> fmt::Display for SendError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        "sending on a closed channel".fmt(f)
    }
}

impl<T> fmt::Debug for SendError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SendError<{}>(_)", stringify!(T))
    }
}

impl<T> std::error::Error for SendError<T> {}

/// An error returned from the indefinitely blocking recv functions on a [`Receiver`].
///
/// The recv operation can only fail if the corresponding [`Sender`] was dropped before sending
/// any message. Or if a message has already been sent and received on the channel.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub struct RecvError;

impl fmt::Display for RecvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        "receiving on a closed channel".fmt(f)
    }
}

impl std::error::Error for RecvError {}

/// An error returned when trying a non blocking receive on a [`Receiver`].
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

impl std::error::Error for TryRecvError {}

/// An error returned when trying a time limited blocking receive on a [`Receiver`].
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum RecvTimeoutError {
    /// No message arrived on the channel before the timeout was reached. The channel is still open.
    Timeout,

    /// The channel is closed. Either the sender was dropped before sending any message, or the
    /// message has already been extracted from the receiver.
    Disconnected,
}

impl fmt::Display for RecvTimeoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = match self {
            RecvTimeoutError::Timeout => "timed out waiting on channel",
            RecvTimeoutError::Disconnected => "channel is empty and sending half is closed",
        };
        msg.fmt(f)
    }
}

impl std::error::Error for RecvTimeoutError {}
