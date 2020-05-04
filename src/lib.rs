//! Oneshot spsc channel working both in and between sync and async environments.

#![deny(rust_2018_idioms)]

use core::mem;
use core::pin::Pin;
use core::ptr;
#[cfg(not(loom))]
use core::sync::atomic::{AtomicU8, Ordering::SeqCst};
use core::task::{self, Poll};
#[cfg(loom)]
use loom::sync::atomic::{AtomicU8, Ordering::SeqCst};
use std::time::{Duration, Instant};

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
#[cfg(loom)]
use loombox::Box;
#[cfg(not(loom))]
use std::boxed::Box;

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

#[derive(Debug, Eq, PartialEq, Hash)]
pub struct Sender<T> {
    channel_ptr: *mut Channel<T>,
}

#[derive(Debug, Eq, PartialEq, Hash)]
pub struct Receiver<T> {
    channel_ptr: *mut Channel<T>,
}

unsafe impl<T: Send> Send for Sender<T> {}
unsafe impl<T: Send> Send for Receiver<T> {}
impl<T> Unpin for Receiver<T> {}

impl<T> Sender<T> {
    /// Sends `message` over the channel to the [`Receiver`].
    ///
    /// Returns an error if the receiver has already been dropped. The message can
    /// be extracted from the error.
    pub fn send(self, message: T) -> Result<(), SendError<T>> {
        // SAFETY: The reference won't be used after it is freed in this method
        let channel: &mut Channel<T> = unsafe { &mut *self.channel_ptr };

        // Don't run our Drop implementation if send was called, any cleanup now happens here
        mem::forget(self);

        // Write the message into the channel on the heap.
        unsafe { channel.message.as_mut_ptr().write(message) };
        // Set the state to signal there is a message on the channel.
        match channel.state.swap(MESSAGE, SeqCst) {
            // The receiver is alive and has not started waiting. Send done.
            EMPTY => Ok(()),
            // The receiver is waiting. Wake it up so it can return the message.
            RECEIVING => {
                unsafe { ptr::read(&channel.waker).assume_init() }.unpark();
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
        // SAFETY: The reference won't be used after it is freed in this method
        let channel: &mut Channel<T> = unsafe { &mut *self.channel_ptr };

        // Set the channel state to disconnected and read what state the receiver was in
        match channel.state.swap(DISCONNECTED, SeqCst) {
            // The receiver has not started waiting, nor is it dropped.
            EMPTY => (),
            // The receiver is waiting. Wake it up so it can detect that the channel disconnected.
            RECEIVING => {
                unsafe { ptr::read(&channel.waker).assume_init() }.unpark();
            }
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
    /// This method will always block the current thread if there is no data available and it's
    /// still possible for the message to be sent. Once the message is sent to the corresponding
    /// [`Sender`], then this receiver will wake up and return that message.
    ///
    /// If the corresponding [`Sender`] has disconnected (been dropped), or it disconnects while
    /// this call is blocking, this call will wake up and return `Err` to indicate that the message
    /// can never be received on this channel.
    ///
    /// If a sent message has already been extracted from this channel this method will return an
    /// error.
    pub fn recv(self) -> Result<T, RecvError> {
        // SAFETY: The reference won't be used after it is freed in this method
        let channel: &mut Channel<T> = unsafe { &mut *self.channel_ptr };

        // Don't run our Drop implementation if we are receiving consuming ourselves.
        mem::forget(self);

        match channel.state.load(SeqCst) {
            // The sender is alive but has not sent anything yet. We prepare to park.
            EMPTY => {
                // Conditionally add a delay here to help the tests trigger the edge cases where
                // the sender manages to be dropped or send something before we are able to store our
                // `Thread` object in the state.
                #[cfg(oneshot_test_delay)]
                std::thread::sleep(std::time::Duration::from_millis(10));

                // Write our waker instance to the channel.
                let waker = ReceiverWaker::current_thread();
                unsafe { channel.waker.as_mut_ptr().write(waker) };

                match channel.state.compare_and_swap(EMPTY, RECEIVING, SeqCst) {
                    // We stored our waker, now we park until the sender has changed the state
                    EMPTY => loop {
                        thread::park();
                        match channel.state.load(SeqCst) {
                            // The sender sent the message while we were parked.
                            MESSAGE => {
                                let message = unsafe { ptr::read(&channel.message).assume_init() };
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
                        unsafe { ptr::read(&channel.waker).assume_init() };
                        let message = unsafe { ptr::read(&channel.message).assume_init() };
                        unsafe { Box::from_raw(channel) };
                        Ok(message)
                    }
                    // The sender was dropped before sending anything while we prepared to park.
                    DISCONNECTED => {
                        unsafe { ptr::read(&channel.waker).assume_init() };
                        unsafe { Box::from_raw(channel) };
                        Err(RecvError)
                    }
                    _ => unreachable!(),
                }
            }
            // The sender already sent the message.
            MESSAGE => {
                let message = unsafe { ptr::read(&channel.message).assume_init() };
                unsafe { Box::from_raw(channel) };
                Ok(message)
            }
            // The sender was dropped before sending anything, or we already received the message.
            DISCONNECTED => {
                unsafe { Box::from_raw(channel) };
                Err(RecvError)
            }
            _ => unreachable!(),
        }
    }

    /// Attempts to wait for a message from the [`Sender`], returning an error if the channel is
    /// disconnected. This is a non consuming version of [`Receiver::recv`], but with a bit
    /// worse performance. Prefer `[`Receiver::recv`]` if your code allows consuming the receiver.
    ///
    /// If a message is returned, the channel is disconnected and any subsequent receive operation
    /// using this receiver will return an error.
    pub fn recv_ref(&self) -> Result<T, RecvError> {
        // SAFETY: The channel will not be freed while this method is still running.
        let channel: &mut Channel<T> = unsafe { &mut *self.channel_ptr };

        match channel.state.load(SeqCst) {
            // The sender is alive but has not sent anything yet. We prepare to park.
            EMPTY => {
                // Conditionally add a delay here to help the tests trigger the edge cases where
                // the sender manages to be dropped or send something before we are able to store our
                // `Thread` object in the channel.
                #[cfg(oneshot_test_delay)]
                std::thread::sleep(std::time::Duration::from_millis(10));

                // Write our waker instance to the channel.
                let waker = ReceiverWaker::current_thread();
                unsafe { channel.waker.as_mut_ptr().write(waker) };

                match channel.state.compare_and_swap(EMPTY, RECEIVING, SeqCst) {
                    // We stored our waker, now we park until the sender has changed the state
                    EMPTY => loop {
                        thread::park();
                        match channel.state.load(SeqCst) {
                            // The sender sent the message while we were parked.
                            // We take the message and mark the channel disconnected.
                            MESSAGE => {
                                channel.state.store(DISCONNECTED, SeqCst);
                                break Ok(unsafe { ptr::read(&channel.message).assume_init() });
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
                        unsafe { ptr::read(&channel.waker).assume_init() };
                        Ok(unsafe { ptr::read(&channel.message).assume_init() })
                    }
                    // The sender was dropped before sending anything while we prepared to park.
                    DISCONNECTED => {
                        unsafe { ptr::read(&channel.waker).assume_init() };
                        Err(RecvError)
                    }
                    _ => unreachable!(),
                }
            }
            // The sender sent the message. We take the message and mark the channel disconnected.
            MESSAGE => {
                channel.state.store(DISCONNECTED, SeqCst);
                Ok(unsafe { ptr::read(&channel.message).assume_init() })
            }
            // The sender was dropped before sending anything, or we already received the message.
            DISCONNECTED => Err(RecvError),
            _ => unreachable!(),
        }
    }

    /// Checks if there is a message in the channel without blocking. Returns:
    ///  * `Ok(message)` if there was a message in the channel.
    ///  * `Err(Empty)` if the sender is alive, but has not yet sent a message.
    ///  * `Err(Disconnected)` if the sender was dropped before sending anything or if the message
    ///    has already been extracted by a previous receive call.
    ///
    /// If a message is returned, the channel is disconnected and any subsequent receive operation
    /// using this receiver will return an error.
    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        // SAFETY: The channel will not be freed while this method is still running.
        let channel: &mut Channel<T> = unsafe { &mut *self.channel_ptr };

        match channel.state.load(SeqCst) {
            // The sender is alive but has not sent anything yet.
            EMPTY => Err(TryRecvError::Empty),
            // The sender sent the message. We take the message and mark the channel disconnected.
            MESSAGE => {
                channel.state.store(DISCONNECTED, SeqCst);
                Ok(unsafe { ptr::read(&channel.message).assume_init() })
            }
            // The sender was dropped before sending anything, or we already received the message.
            DISCONNECTED => Err(TryRecvError::Disconnected),
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
    pub fn recv_deadline(&self, deadline: Instant) -> Result<T, RecvTimeoutError> {
        // SAFETY: The channel will not be freed while this method is still running.
        let channel: &mut Channel<T> = unsafe { &mut *self.channel_ptr };

        match channel.state.load(SeqCst) {
            // The sender is alive but has not sent anything yet. We prepare to park.
            EMPTY => {
                // Conditionally add a delay here to help the tests trigger the edge cases where
                // the sender manages to be dropped or send something before we are able to store our
                // `Thread` object in the channel.
                #[cfg(oneshot_test_delay)]
                std::thread::sleep(std::time::Duration::from_millis(10));

                // Write our thread instance to the channel.
                let waker = ReceiverWaker::current_thread();
                unsafe { channel.waker.as_mut_ptr().write(waker) };

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
                                break Ok(unsafe { ptr::read(&channel.message).assume_init() });
                            }
                            // The sender was dropped while we were parked.
                            DISCONNECTED => break Err(RecvTimeoutError::Disconnected),
                            // State did not change, spurious wakeup, park again.
                            RECEIVING => {
                                if timed_out {
                                    unsafe { ptr::read(&channel.waker).assume_init() };
                                    break Err(RecvTimeoutError::Timeout);
                                }
                            }
                            _ => unreachable!(),
                        }
                    },
                    // The sender sent the message while we prepared to park.
                    MESSAGE => {
                        channel.state.store(DISCONNECTED, SeqCst);
                        unsafe { ptr::read(&channel.waker).assume_init() };
                        Ok(unsafe { ptr::read(&channel.message).assume_init() })
                    }
                    // The sender was dropped before sending anything while we prepared to park.
                    DISCONNECTED => {
                        unsafe { ptr::read(&channel.waker).assume_init() };
                        Err(RecvTimeoutError::Disconnected)
                    }
                    _ => unreachable!(),
                }
            }
            // The sender sent the message.
            MESSAGE => {
                channel.state.store(DISCONNECTED, SeqCst);
                Ok(unsafe { ptr::read(&channel.message).assume_init() })
            }
            // The sender was dropped before sending anything, or we already received the message.
            DISCONNECTED => Err(RecvTimeoutError::Disconnected),
            _ => unreachable!(),
        }
    }
}

impl<T> core::future::Future for Receiver<T> {
    type Output = Result<T, RecvError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<Self::Output> {
        // SAFETY: The channel will not be freed while this method is still running.
        let channel: &mut Channel<T> = unsafe { &mut *self.channel_ptr };

        match channel.state.load(SeqCst) {
            EMPTY => {
                // The sender is alive but has not sent anything yet.

                // Write our thread instance to the channel.
                let waker = ReceiverWaker::task_waker(cx);
                unsafe { channel.waker.as_mut_ptr().write(waker) };

                match channel.state.compare_and_swap(EMPTY, RECEIVING, SeqCst) {
                    // We stored our waker, now we return and let the sender wake us up
                    EMPTY => Poll::Pending,
                    // The sender was dropped before sending anything while we prepared to park.
                    DISCONNECTED => {
                        unsafe { ptr::read(&channel.waker).assume_init() };
                        Poll::Ready(Err(RecvError))
                    }
                    // The sender sent the message while we prepared to park.
                    // We take the message and mark the channel disconnected.
                    MESSAGE => {
                        unsafe { ptr::read(&channel.waker).assume_init() };
                        channel.state.store(DISCONNECTED, SeqCst);
                        Poll::Ready(Ok(unsafe { ptr::read(&channel.message).assume_init() }))
                    }
                    _ => unreachable!(),
                }
            }
            // Our waker is still in the channel. We were polled while waiting for the sender.
            RECEIVING => Poll::Pending,
            // The sender sent the message.
            MESSAGE => {
                channel.state.store(DISCONNECTED, SeqCst);
                Poll::Ready(Ok(unsafe { ptr::read(&channel.message).assume_init() }))
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
                unsafe { ptr::read(&channel.message).assume_init() };
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

const EMPTY: u8 = 0;
const MESSAGE: u8 = 1;
const RECEIVING: u8 = 2;
const DISCONNECTED: u8 = 3;

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
}

enum ReceiverWaker {
    /// The receiver is waiting synchronously. Its thread is parked.
    Thread(thread::Thread),
    /// The receiver is waiting asynchronously. Its task can be woken up with this `Waker`.
    Task(task::Waker),
}

impl ReceiverWaker {
    pub fn current_thread() -> Self {
        Self::Thread(thread::current())
    }

    pub fn task_waker(cx: &task::Context<'_>) -> Self {
        Self::Task(cx.waker().clone())
    }

    pub fn unpark(self) {
        match self {
            ReceiverWaker::Thread(thread) => thread.unpark(),
            ReceiverWaker::Task(waker) => waker.wake(),
        }
    }
}
