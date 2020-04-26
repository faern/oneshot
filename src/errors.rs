use super::Box;
use core::fmt;

#[derive(Debug, Eq, PartialEq)]
pub struct DroppedSenderError;

impl fmt::Display for DroppedSenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        "Oneshot sender dropped without sending anything, or message already received".fmt(f)
    }
}

impl std::error::Error for DroppedSenderError {}

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
        "oneshot receiver has already been dropped".fmt(f)
    }
}

impl<T> fmt::Debug for SendError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SendError<{}>(_)", stringify!(T))
    }
}

impl<T> std::error::Error for SendError<T> {}
