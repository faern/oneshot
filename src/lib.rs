//! Oneshot spsc channel. The sender's send method is non-blocking, lock- and wait-free[1].
//! The receiver supports both lock- and wait-free `try_recv` as well as indefinite and time
//! limited thread blocking receive operations. The receiver also implements `Future` and
//! supports asynchronously awaiting the message.
//!
//! This is a oneshot channel implementation. Meaning each channel instance can only transport
//! a single message. This has a few nice outcomes. One thing is that the implementation can
//! be very efficient, utilizing the knowledge that there will only be one message. But more
//! importantly, it allows the API to be expressed in such a way that certain edge cases
//! that you don't want to care about when only sending a single message on a channel does not
//! exist. For example. The sender can't be copied or cloned and the send method takes ownership
//! and consumes the sender. So you are guaranteed, at the type level, that there can only be
//! one message sent.
//!
//! # Sync vs async
//!
//! The main thing motivating this library was that there were no (known to me) channel
//! implementations allowing you to seamlessly send messages between a normal thread and an async
//! task, or the other way around. If message passing is the way you are communicating, of course
//! that should work smoothly between the sync and async parts of the program!
//!
//! This library achieves that by having an almost[1] wait-free send operation that can safely
//! be used in both sync threads and async tasks. The receiver has both thread blocking
//! receive methods for synchronous usage, and implements `Future` for asynchronous usage.
//!
//! # Asynchronous support
//!
//! The receiving endpoint of this channel implements Rust's `Future` trait and can be waited on
//! in an asynchronous task. This implementation is completely executor/runtime agnostic. It should
//! be possible to use this library with any executor.
//!
//! # Footnotes
//!
//! [1]: See documentation on [Sender::send] for situations where it might not be fully wait-free.

// # Implementation description
//
// When a channel is created via the channel function, it allocates space on the heap to fit:
// * A one byte atomic integer that represents the current channel state,
// * Uninitialized memory to fit the message,
// * Uninitialized memory to fit the waker that can wake the receiving task or thread up.
//
// The size of the waker depends on which features are activated, it ranges from 0 to 24 bytes[1].
// So with all features enabled (the default) each channel allocates 25 bytes plus the size of the
// message, plus any padding needed to get correct memory alignment.
//
// The Sender and Receiver only holds a raw pointer to this heap channel object. The last endpoint
// to be consumed or dropped is responsible for freeing the heap memory. The first endpoint to
// go away signal via the state that it is gone. And the second one see this and frees the memory.
//
// Sending on the sender copies the message to the (so far uninitialized) memory region on the
// heap and swaps the state from whatever it was to MESSAGE.
// if the state before the swap was DISCONNECTED the SendError is returned and nothing else is done.
// The SendError now owns the heap channel memory and is responsible for dropping the message
// and freeing the memory.
// If the state was RECEIVING the sender reads the waker object from the channel heap memory and
// call the unpark method, which will wake up the receiver.
//
// Receiving on the channel first checks the state. If it is MESSAGE the message object is read
// from the heap back into the stack, the heap memory is freed and the message returned. If the
// state is DISCONNECTED the heap memory is freed and an error is returned. And if the state is
// EMPTY and the receive operation is a blocking one it creates a waker object and writes it to
// the channel on the heap and does an atomic compare_and_swap on the state from EMPTY to RECEIVING.
// If the swap went fine, it either parks the thread or returns Poll::Pending, depending on if
// the receive is a blocking or an async one. It now just waits for the sender to wake it up.
//
//
// ## Footnotes
//
// [1]: Mind that the waker only takes zero bytes when all features are disabled, making it
//      impossible to *wait* for the message. `try_recv` the only available method in this scenario.

#![deny(rust_2018_idioms)]
#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(not(loom))]
extern crate alloc;

use core::{mem, ptr};

#[cfg(not(loom))]
use core::sync::atomic::{AtomicU8, Ordering::SeqCst};
#[cfg(loom)]
use loom::sync::atomic::{AtomicU8, Ordering::SeqCst};

#[cfg(feature = "async")]
use core::{
    pin::Pin,
    task::{self, Poll},
};
#[cfg(feature = "std")]
use std::time::{Duration, Instant};

#[cfg(feature = "std")]
mod thread {
    pub use std::thread::{current, Thread};

    #[cfg(loom)]
    pub use loom::thread::yield_now as park;
    #[cfg(not(loom))]
    pub use std::thread::{park, park_timeout};

    #[cfg(loom)]
    pub fn park_timeout(_timeout: std::time::Duration) {
        loom::thread::yield_now()
    }
}

#[cfg(loom)]
mod loombox;
#[cfg(not(loom))]
use alloc::boxed::Box;
#[cfg(loom)]
use loombox::Box;

mod errors;
pub use errors::{RecvError, RecvTimeoutError, SendError, TryRecvError};

/// Creates a new oneshot channel and returns the two endpoints, [`Sender`] and [`Receiver`].
pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    // Allocate the channel on the heap and get the pointer.
    // The last endpoint of the channel to be alive is responsible for freeing the channel
    // and dropping any object that might have been written to it.
    let channel_ptr = Box::into_raw(Box::new(Channel::new()));
    (Sender { channel_ptr }, Receiver { channel_ptr })
}

#[derive(Debug)]
pub struct Sender<T> {
    channel_ptr: *mut Channel<T>,
}

#[derive(Debug)]
pub struct Receiver<T> {
    channel_ptr: *mut Channel<T>,
}

unsafe impl<T: Send> Send for Sender<T> {}
unsafe impl<T: Send> Send for Receiver<T> {}
impl<T> Unpin for Receiver<T> {}

impl<T> Sender<T> {
    /// Sends `message` over the channel to the corresponding [`Receiver`].
    ///
    /// Returns an error if the receiver has already been dropped. The message can
    /// be extracted from the error.
    ///
    /// This method is completely lock-free and wait-free when sending on a channel that the
    /// receiver is currently not receiving on. If the receiver is receiving during the send
    /// operation this method includes waking up the thread/task. Unparking a thread currently
    /// involves a mutex in Rust's standard library. How lock-free waking up an async task is
    /// depends on your executor. If this method returns a `SendError`, please mind that dropping
    /// the error involves running any drop implementation on the message type, which might or
    /// might not be lock-free.
    pub fn send(self, message: T) -> Result<(), SendError<T>> {
        // SAFETY: The channel exists on the heap for the entire duration of this method.
        let channel: &mut Channel<T> = unsafe { &mut *self.channel_ptr };

        // Don't run our Drop implementation if send was called, any cleanup now happens here
        mem::forget(self);

        // Write the message into the channel on the heap.
        channel.write_message(message);
        // Set the state to signal there is a message on the channel.
        match channel.state.swap(MESSAGE, SeqCst) {
            // The receiver is alive and has not started waiting. Send done.
            EMPTY => Ok(()),
            // The receiver is waiting. Wake it up so it can return the message.
            RECEIVING => {
                unsafe { channel.take_waker() }.unpark();
                Ok(())
            }
            // The receiver was already dropped. The error is responsible for freeing the channel.
            DISCONNECTED => Err(SendError::new(channel)),
            _ => unreachable!(),
        }
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        // SAFETY: The reference won't be used after the channel is freed in this method
        let channel: &mut Channel<T> = unsafe { &mut *self.channel_ptr };

        // Set the channel state to disconnected and read what state the receiver was in
        match channel.state.swap(DISCONNECTED, SeqCst) {
            // The receiver has not started waiting, nor is it dropped.
            EMPTY => (),
            // The receiver is waiting. Wake it up so it can detect that the channel disconnected.
            RECEIVING => unsafe { channel.take_waker() }.unpark(),
            // The receiver was already dropped. We are responsible for freeing the channel.
            DISCONNECTED => {
                unsafe { Box::from_raw(channel) };
            }
            _ => unreachable!(),
        }
    }
}

impl<T> Receiver<T> {
    /// Attempts to wait for a message from the [`Sender`], returning an error if the channel is
    /// disconnected.
    ///
    /// This method will always block the current thread if there is no data available and it is
    /// still possible for the message to be sent. Once the message is sent to the corresponding
    /// [`Sender`], then this receiver will wake up and return that message.
    ///
    /// If the corresponding [`Sender`] has disconnected (been dropped), or it disconnects while
    /// this call is blocking, this call will wake up and return `Err` to indicate that the message
    /// can never be received on this channel.
    ///
    /// If a sent message has already been extracted from this channel this method will return an
    /// error.
    ///
    /// # Panics
    ///
    /// Panics if called after this receiver has been polled asynchronously.
    #[cfg(feature = "std")]
    pub fn recv(self) -> Result<T, RecvError> {
        // SAFETY: The reference won't be used after the channel is freed in this method
        let channel: &mut Channel<T> = unsafe { &mut *self.channel_ptr };

        // Don't run our Drop implementation if we are receiving consuming ourselves.
        mem::forget(self);

        match channel.state.load(SeqCst) {
            // The sender is alive but has not sent anything yet. We prepare to park.
            EMPTY => {
                // Conditionally add a delay here to help the tests trigger the edge cases where
                // the sender manages to be dropped or send something before we are able to store
                // our waker object in the channel.
                #[cfg(oneshot_test_delay)]
                std::thread::sleep(std::time::Duration::from_millis(10));

                // Write our waker instance to the channel.
                channel.write_waker(ReceiverWaker::current_thread());

                match channel.state.compare_and_swap(EMPTY, RECEIVING, SeqCst) {
                    // We stored our waker, now we park until the sender has changed the state
                    EMPTY => loop {
                        thread::park();
                        match channel.state.load(SeqCst) {
                            // The sender sent the message while we were parked.
                            MESSAGE => {
                                let message = unsafe { channel.take_message() };
                                unsafe { Box::from_raw(channel) };
                                break Ok(message);
                            }
                            // The sender was dropped while we were parked.
                            DISCONNECTED => {
                                unsafe { Box::from_raw(channel) };
                                break Err(RecvError);
                            }
                            // State did not change, spurious wakeup, park again.
                            RECEIVING => (),
                            _ => unreachable!(),
                        }
                    },
                    // The sender sent the message while we prepared to park.
                    MESSAGE => {
                        unsafe { channel.drop_waker() };
                        let message = unsafe { channel.take_message() };
                        unsafe { Box::from_raw(channel) };
                        Ok(message)
                    }
                    // The sender was dropped before sending anything while we prepared to park.
                    DISCONNECTED => {
                        unsafe { channel.drop_waker() };
                        unsafe { Box::from_raw(channel) };
                        Err(RecvError)
                    }
                    _ => unreachable!(),
                }
            }
            // The sender already sent the message.
            MESSAGE => {
                let message = unsafe { channel.take_message() };
                unsafe { Box::from_raw(channel) };
                Ok(message)
            }
            // The sender was dropped before sending anything, or we already received the message.
            DISCONNECTED => {
                unsafe { Box::from_raw(channel) };
                Err(RecvError)
            }
            // The receiver must have been `Future::poll`ed prior to this call.
            #[cfg(feature = "async")]
            RECEIVING => panic!(RECEIVER_USED_SYNC_AND_ASYNC_ERROR),
            _ => unreachable!(),
        }
    }

    /// Attempts to wait for a message from the [`Sender`], returning an error if the channel is
    /// disconnected. This is a non consuming version of [`Receiver::recv`], but with a bit
    /// worse performance. Prefer `[`Receiver::recv`]` if your code allows consuming the receiver.
    ///
    /// If a message is returned, the channel is disconnected and any subsequent receive operation
    /// using this receiver will return an error.
    ///
    /// # Panics
    ///
    /// Panics if called after this receiver has been polled asynchronously.
    #[cfg(feature = "std")]
    pub fn recv_ref(&self) -> Result<T, RecvError> {
        // SAFETY: The channel will not be freed while this method is still running.
        let channel: &mut Channel<T> = unsafe { &mut *self.channel_ptr };

        match channel.state.load(SeqCst) {
            // The sender is alive but has not sent anything yet. We prepare to park.
            EMPTY => {
                // Conditionally add a delay here to help the tests trigger the edge cases where
                // the sender manages to be dropped or send something before we are able to store
                // our waker object in the channel.
                #[cfg(oneshot_test_delay)]
                std::thread::sleep(std::time::Duration::from_millis(10));

                // Write our waker instance to the channel.
                channel.write_waker(ReceiverWaker::current_thread());

                match channel.state.compare_and_swap(EMPTY, RECEIVING, SeqCst) {
                    // We stored our waker, now we park until the sender has changed the state
                    EMPTY => loop {
                        thread::park();
                        match channel.state.load(SeqCst) {
                            // The sender sent the message while we were parked.
                            // We take the message and mark the channel disconnected.
                            MESSAGE => {
                                channel.state.store(DISCONNECTED, SeqCst);
                                break Ok(unsafe { channel.take_message() });
                            }
                            // The sender was dropped while we were parked.
                            DISCONNECTED => break Err(RecvError),
                            // State did not change, spurious wakeup, park again.
                            RECEIVING => (),
                            _ => unreachable!(),
                        }
                    },
                    // The sender sent the message while we prepared to park.
                    MESSAGE => {
                        channel.state.store(DISCONNECTED, SeqCst);
                        unsafe { channel.drop_waker() };
                        Ok(unsafe { channel.take_message() })
                    }
                    // The sender was dropped before sending anything while we prepared to park.
                    DISCONNECTED => {
                        unsafe { channel.drop_waker() };
                        Err(RecvError)
                    }
                    _ => unreachable!(),
                }
            }
            // The sender sent the message. We take the message and mark the channel disconnected.
            MESSAGE => {
                channel.state.store(DISCONNECTED, SeqCst);
                Ok(unsafe { channel.take_message() })
            }
            // The sender was dropped before sending anything, or we already received the message.
            DISCONNECTED => Err(RecvError),
            // The receiver must have been `Future::poll`ed prior to this call.
            #[cfg(feature = "async")]
            RECEIVING => panic!(RECEIVER_USED_SYNC_AND_ASYNC_ERROR),
            _ => unreachable!(),
        }
    }

    /// Checks if there is a message in the channel without blocking. Returns:
    ///  * `Ok(message)` if there was a message in the channel.
    ///  * `Err(Empty)` if the [`Sender`] is alive, but has not yet sent a message.
    ///  * `Err(Disconnected)` if the [`Sender`] was dropped before sending anything or if the
    ///    message has already been extracted by a previous receive call.
    ///
    /// If a message is returned, the channel is disconnected and any subsequent receive operation
    /// using this receiver will return an error.
    ///
    /// This method is completely lock-free and wait-free. The only thing it does is an atomic
    /// integer load of the channel state. And if there is a message in the channel it additionally
    /// performs one atomic integer store and copies the message from the heap to the stack for
    /// returning it.
    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        // SAFETY: The channel will not be freed while this method is still running.
        let channel: &mut Channel<T> = unsafe { &mut *self.channel_ptr };

        match channel.state.load(SeqCst) {
            // The sender is alive but has not sent anything yet.
            EMPTY => Err(TryRecvError::Empty),
            // The sender sent the message. We take the message and mark the channel disconnected.
            MESSAGE => {
                channel.state.store(DISCONNECTED, SeqCst);
                Ok(unsafe { channel.take_message() })
            }
            // The sender was dropped before sending anything, or we already received the message.
            DISCONNECTED => Err(TryRecvError::Disconnected),
            // The receiver must have already been `Future::poll`ed. No message available.
            #[cfg(feature = "async")]
            RECEIVING => Err(TryRecvError::Empty),
            _ => unreachable!(),
        }
    }

    /// Like [`Receiver::recv`], but will not block longer than `timeout`. Returns:
    ///  * `Ok(message)` if there was a message in the channel before the timeout was reached.
    ///  * `Err(Timeout)` if no message arrived on the channel before the timeout was reached.
    ///  * `Err(Disconnected)` if the sender was dropped before sending anything or if the message
    ///    has already been extracted by a previous receive call.
    ///
    /// If a message is returned, the channel is disconnected and any subsequent receive operation
    /// using this receiver will return an error.
    ///
    /// If the supplied `timeout` is so large that Rust's `Instant` type can't represent this point
    /// in the future this falls back to an indefinitely blocking receive operation.
    ///
    /// # Panics
    ///
    /// Panics if called after this receiver has been polled asynchronously.
    #[cfg(feature = "std")]
    pub fn recv_timeout(&self, timeout: Duration) -> Result<T, RecvTimeoutError> {
        match Instant::now().checked_add(timeout) {
            Some(deadline) => self.recv_deadline(deadline),
            None => self.recv_ref().map_err(|_| RecvTimeoutError::Disconnected),
        }
    }

    /// Like [`Receiver::recv`], but will not block longer than until `deadline`. Returns:
    ///  * `Ok(message)` if there was a message in the channel before the deadline was reached.
    ///  * `Err(Timeout)` if no message arrived on the channel before the deadline was reached.
    ///  * `Err(Disconnected)` if the sender was dropped before sending anything or if the message
    ///    has already been extracted by a previous receive call.
    ///
    /// If a message is returned, the channel is disconnected and any subsequent receive operation
    /// using this receiver will return an error.
    ///
    /// # Panics
    ///
    /// Panics if called after this receiver has been polled asynchronously.
    #[cfg(feature = "std")]
    pub fn recv_deadline(&self, deadline: Instant) -> Result<T, RecvTimeoutError> {
        // SAFETY: The channel will not be freed while this method is still running.
        let channel: &mut Channel<T> = unsafe { &mut *self.channel_ptr };

        match channel.state.load(SeqCst) {
            // The sender is alive but has not sent anything yet. We prepare to park.
            EMPTY => {
                // Conditionally add a delay here to help the tests trigger the edge cases where
                // the sender manages to be dropped or send something before we are able to store
                // our waker object in the channel.
                #[cfg(oneshot_test_delay)]
                std::thread::sleep(std::time::Duration::from_millis(10));

                // Write our thread instance to the channel.
                channel.write_waker(ReceiverWaker::current_thread());

                match channel.state.compare_and_swap(EMPTY, RECEIVING, SeqCst) {
                    // We stored our waker, now we park until the sender has changed the state
                    EMPTY => loop {
                        let (state, timed_out) = if let Some(timeout) =
                            deadline.checked_duration_since(Instant::now())
                        {
                            thread::park_timeout(timeout);
                            (channel.state.load(SeqCst), false)
                        } else {
                            // We reached the deadline. Stop being in the receiving state.
                            (channel.state.swap(EMPTY, SeqCst), true)
                        };
                        match state {
                            // The sender sent the message while we were parked.
                            MESSAGE => {
                                channel.state.store(DISCONNECTED, SeqCst);
                                break Ok(unsafe { channel.take_message() });
                            }
                            // The sender was dropped while we were parked.
                            DISCONNECTED => break Err(RecvTimeoutError::Disconnected),
                            // State did not change, spurious wakeup, park again.
                            RECEIVING => {
                                if timed_out {
                                    unsafe { channel.drop_waker() };
                                    break Err(RecvTimeoutError::Timeout);
                                }
                            }
                            _ => unreachable!(),
                        }
                    },
                    // The sender sent the message while we prepared to park.
                    MESSAGE => {
                        channel.state.store(DISCONNECTED, SeqCst);
                        unsafe { channel.drop_waker() };
                        Ok(unsafe { channel.take_message() })
                    }
                    // The sender was dropped before sending anything while we prepared to park.
                    DISCONNECTED => {
                        unsafe { channel.drop_waker() };
                        Err(RecvTimeoutError::Disconnected)
                    }
                    _ => unreachable!(),
                }
            }
            // The sender sent the message.
            MESSAGE => {
                channel.state.store(DISCONNECTED, SeqCst);
                Ok(unsafe { channel.take_message() })
            }
            // The sender was dropped before sending anything, or we already received the message.
            DISCONNECTED => Err(RecvTimeoutError::Disconnected),
            // The receiver must have been `Future::poll`ed prior to this call.
            #[cfg(feature = "async")]
            RECEIVING => panic!(RECEIVER_USED_SYNC_AND_ASYNC_ERROR),
            _ => unreachable!(),
        }
    }
}

#[cfg(feature = "async")]
impl<T> core::future::Future for Receiver<T> {
    type Output = Result<T, RecvError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<Self::Output> {
        // SAFETY: The channel will not be freed while this method is still running.
        let channel: &mut Channel<T> = unsafe { &mut *self.channel_ptr };

        match channel.state.load(SeqCst) {
            EMPTY => {
                // The sender is alive but has not sent anything yet.

                // Write our thread instance to the channel.
                channel.write_waker(ReceiverWaker::task_waker(cx));

                match channel.state.compare_and_swap(EMPTY, RECEIVING, SeqCst) {
                    // We stored our waker, now we return and let the sender wake us up
                    EMPTY => Poll::Pending,
                    // The sender was dropped before sending anything while we prepared to park.
                    DISCONNECTED => {
                        unsafe { channel.drop_waker() };
                        Poll::Ready(Err(RecvError))
                    }
                    // The sender sent the message while we prepared to park.
                    // We take the message and mark the channel disconnected.
                    MESSAGE => {
                        unsafe { channel.drop_waker() };
                        channel.state.store(DISCONNECTED, SeqCst);
                        Poll::Ready(Ok(unsafe { channel.take_message() }))
                    }
                    _ => unreachable!(),
                }
            }
            // Our waker is still in the channel. We were polled while waiting for the sender.
            RECEIVING => Poll::Pending,
            // The sender sent the message.
            MESSAGE => {
                channel.state.store(DISCONNECTED, SeqCst);
                Poll::Ready(Ok(unsafe { channel.take_message() }))
            }
            // The sender was dropped before sending anything, or we already received the message.
            DISCONNECTED => Poll::Ready(Err(RecvError)),
            _ => unreachable!(),
        }
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        // SAFETY: The reference won't be used after it is freed in this method
        let channel: &mut Channel<T> = unsafe { &mut *self.channel_ptr };

        // Set the channel state to disconnected and read what state the receiver was in
        match channel.state.swap(DISCONNECTED, SeqCst) {
            // The sender has not sent anything, nor is it dropped.
            EMPTY => (),
            // The sender already sent something. We must drop it, and free the channel.
            MESSAGE => {
                unsafe { channel.drop_message() };
                unsafe { Box::from_raw(channel) };
            }
            // The sender was already dropped. We are responsible for freeing the channel
            DISCONNECTED => {
                unsafe { Box::from_raw(channel) };
            }
            _ => unreachable!(),
        }
    }
}

/// All the values that the `Channel::state` field can have during the lifetime of a channel.
mod states {
    /// The initial channel state. Active while both endpoints are still alive, no message has been
    /// sent, and the receiver is not receiving.
    pub const EMPTY: u8 = 0;
    /// A message has been sent to the channel, but the receiver has not yet read it.
    pub const MESSAGE: u8 = 1;
    /// No message has yet been sent on the channel, but the receiver is currently receiving.
    pub const RECEIVING: u8 = 2;
    /// The channel has been closed. This means that either the sender or receiver has been dropped,
    /// or the message sent to the channel has already been received. Since this is a oneshot
    /// channel, it is disconnected after the one message it is supposed to hold has been
    /// transmitted.
    pub const DISCONNECTED: u8 = 3;
}
use states::*;

/// Internal channel data structure structure. the `channel` method allocates and puts one instance
/// of this struct on the heap for each oneshot channel instance. The struct holds:
/// * The current state of the channel.
/// * The message in the channel. This memory is uninitialized until the message is sent.
/// * The waker instance for the thread or task that is currently receiving on this channel.
///   This memory is uninitialized until the receiver starts receiving.
struct Channel<T> {
    state: AtomicU8,
    message: mem::MaybeUninit<T>,
    waker: mem::MaybeUninit<ReceiverWaker>,
}

impl<T> Channel<T> {
    pub fn new() -> Self {
        Self {
            state: AtomicU8::new(EMPTY),
            message: mem::MaybeUninit::uninit(),
            waker: mem::MaybeUninit::uninit(),
        }
    }

    #[inline(always)]
    fn write_message(&mut self, message: T) {
        unsafe { self.message.as_mut_ptr().write(message) };
    }

    #[inline(always)]
    unsafe fn take_message(&mut self) -> T {
        ptr::read(&self.message).assume_init()
    }

    #[inline(always)]
    unsafe fn drop_message(&mut self) {
        ptr::drop_in_place(self.message.as_mut_ptr());
    }

    #[cfg(any(feature = "std", feature = "async"))]
    #[inline(always)]
    fn write_waker(&mut self, waker: ReceiverWaker) {
        unsafe { self.waker.as_mut_ptr().write(waker) };
    }

    #[inline(always)]
    unsafe fn take_waker(&mut self) -> ReceiverWaker {
        ptr::read(&self.waker).assume_init()
    }

    #[cfg(any(feature = "std", feature = "async"))]
    #[inline(always)]
    unsafe fn drop_waker(&mut self) {
        ptr::drop_in_place(self.waker.as_mut_ptr());
    }
}

enum ReceiverWaker {
    /// The receiver is waiting synchronously. Its thread is parked.
    #[cfg(feature = "std")]
    Thread(thread::Thread),
    /// The receiver is waiting asynchronously. Its task can be woken up with this `Waker`.
    #[cfg(feature = "async")]
    Task(task::Waker),
}

impl ReceiverWaker {
    #[cfg(feature = "std")]
    pub fn current_thread() -> Self {
        Self::Thread(thread::current())
    }

    #[cfg(feature = "async")]
    pub fn task_waker(cx: &task::Context<'_>) -> Self {
        Self::Task(cx.waker().clone())
    }

    pub fn unpark(self) {
        match self {
            #[cfg(feature = "std")]
            ReceiverWaker::Thread(thread) => thread.unpark(),
            #[cfg(feature = "async")]
            ReceiverWaker::Task(waker) => waker.wake(),
        }
    }
}

#[test]
fn receiver_waker_size() {
    let expected: usize = match (cfg!(feature = "std"), cfg!(feature = "async")) {
        (false, false) => 0,
        (false, true) => 16,
        (true, false) => 8,
        (true, true) => 24,
    };
    assert_eq!(mem::size_of::<ReceiverWaker>(), expected);
}

#[cfg(all(feature = "std", feature = "async"))]
const RECEIVER_USED_SYNC_AND_ASYNC_ERROR: &str =
    "Invalid to call a blocking receive method on oneshot::Receiver after it has been polled";
