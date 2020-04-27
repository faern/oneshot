//! Oneshot spsc channel working both in and between sync and async environments.

#![deny(rust_2018_idioms)]

use core::mem;
use core::pin::Pin;
#[cfg(not(feature = "loom"))]
use core::sync::atomic::{AtomicUsize, Ordering};
use core::task::{self, Poll};
#[cfg(feature = "loom")]
use loom::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

mod thread {
    pub use std::thread::{current, Thread};

    #[cfg(feature = "loom")]
    pub use loom::thread::yield_now as park;
    #[cfg(not(feature = "loom"))]
    pub use std::thread::{park, park_timeout};

    #[cfg(feature = "loom")]
    pub fn park_timeout(_timeout: std::time::Duration) {
        loom::thread::yield_now()
    }
}

#[cfg(feature = "loom")]
mod loombox;
#[cfg(feature = "loom")]
use loombox::Box;
#[cfg(not(feature = "loom"))]
use std::boxed::Box;

mod errors;
pub use errors::{RecvError, RecvTimeoutError, SendError, TryRecvError};

/// Creates a new oneshot channel and returns the two endpoints, [`Sender`] and [`Receiver`].
pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    debug_assert_ne!(states::init(), 0);
    debug_assert_ne!(states::disconnected(), 0);
    debug_assert_ne!(states::init(), states::disconnected());
    // Allocate the state on the heap and initialize it with `states::init()` and get the pointer.
    // The last endpoint of the channel to be alive is responsible for freeing the state.
    let state_ptr = Box::into_raw(Box::new(AtomicUsize::new(states::init())));
    (
        Sender {
            state_ptr,
            _marker: std::marker::PhantomData,
        },
        Receiver {
            state_ptr,
            _marker: std::marker::PhantomData,
        },
    )
}

#[derive(Debug, Eq, PartialEq, Hash)]
pub struct Sender<T> {
    state_ptr: *mut AtomicUsize,
    _marker: std::marker::PhantomData<T>,
}

#[derive(Debug, Eq, PartialEq, Hash)]
pub struct Receiver<T> {
    state_ptr: *mut AtomicUsize,
    _marker: std::marker::PhantomData<T>,
}

unsafe impl<T: Send> Send for Sender<T> {}
unsafe impl<T: Send> Send for Receiver<T> {}

impl<T> Sender<T> {
    /// Sends `message` over the channel to the [`Receiver`].
    ///
    /// Returns an error if the receiver has already been dropped. The message can
    /// be extracted from the error.
    pub fn send(self, message: T) -> Result<(), SendError<T>> {
        let state_ptr = self.state_ptr;
        // Don't run our Drop implementation if send was called, any cleanup now happens here
        mem::forget(self);

        // Put the message on the heap and get the pointer. If sending succeeds the receiver is
        // responsible for freeing it, otherwise we do that
        let message_ptr = Box::into_raw(Box::new(message));
        debug_assert_ne!(message_ptr as usize, states::init());
        debug_assert_ne!(message_ptr as usize, states::disconnected());

        // Store the address to the message in the state and read out what state the receiver is in
        let state = unsafe { &*state_ptr }.swap(message_ptr as usize, Ordering::SeqCst);
        if state == states::init() {
            // The receiver is alive and has not started waiting. Send done.
            // Receiver frees state and message from heap.
            Ok(())
        } else if state == states::disconnected() {
            // The receiver was already dropped.
            // We are responsible for freeing the state and the message
            unsafe { Box::from_raw(state_ptr) };
            Err(SendError::new(unsafe { Box::from_raw(message_ptr) }))
        } else {
            // The receiver is waiting. Wake it up so it can return the message. The receiver frees
            // the state and the message. We free the waker instance in the state.
            take(unsafe { Box::from_raw(state as *mut ReceiverWaker) }).unpark();
            Ok(())
        }
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        // Set the channel state to disconnected and read what state the receiver was in
        let state = unsafe { &*self.state_ptr }.swap(states::disconnected(), Ordering::SeqCst);
        if state == states::init() {
            // The receiver has not started waiting, nor is it dropped. Nothing to do.
            // The receiver is responsible for freeing the state.
        } else if state == states::disconnected() {
            // The receiver was already dropped. We are responsible for freeing the state
            unsafe { Box::from_raw(self.state_ptr) };
        } else {
            // The receiver started waiting. Wake it up so it can detect that the channel
            // disconnected. The receiver frees the state. We free the waker instance in the state.
            take(unsafe { Box::from_raw(state as *mut ReceiverWaker) }).unpark();
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
    pub fn current_thread() -> Box<Self> {
        Box::new(Self::Thread(thread::current()))
    }

    pub fn task_waker(cx: &task::Context<'_>) -> Box<Self> {
        Box::new(Self::Task(cx.waker().clone()))
    }

    pub fn unpark(self) {
        match self {
            ReceiverWaker::Thread(thread) => thread.unpark(),
            ReceiverWaker::Task(waker) => waker.wake(),
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
        let state_ptr = self.state_ptr;
        // Don't run our Drop implementation if we are receiving
        mem::forget(self);

        let state = unsafe { &*state_ptr }.load(Ordering::SeqCst);
        if state == states::init() {
            // The sender is alive but has not sent anything yet. We prepare to park.

            // Conditionally add a delay here to help the tests trigger the edge cases where
            // the sender manages to be dropped or send something before we are able to store our
            // `Thread` object in the state.
            #[cfg(oneshot_test_delay)]
            std::thread::sleep(std::time::Duration::from_millis(10));

            // Allocate and put our thread instance on the heap and then store it in the state.
            // The sender will use this to unpark us when it sends or is dropped.
            // The actor taking this object out of the state is responsible for freeing it.
            let waker_ptr = Box::into_raw(ReceiverWaker::current_thread());
            debug_assert_ne!(waker_ptr as usize, states::init());
            debug_assert_ne!(waker_ptr as usize, states::disconnected());
            let state = unsafe { &*state_ptr }.compare_and_swap(
                states::init(),
                waker_ptr as usize,
                Ordering::SeqCst,
            );
            if state == states::init() {
                // We stored our thread, now we park until the sender has changed the state
                loop {
                    thread::park();
                    // Check if the sender updated the state
                    let state = unsafe { &*state_ptr }.load(Ordering::SeqCst);
                    debug_assert_ne!(state, states::init());
                    if state == states::disconnected() {
                        // The sender was dropped while we were parked.
                        unsafe { Box::from_raw(state_ptr) };
                        break Err(RecvError);
                    } else if state != waker_ptr as usize {
                        // The sender sent the message while we were parked.
                        // We take the message and free the state.
                        unsafe { Box::from_raw(state_ptr) };
                        break Ok(take(unsafe { Box::from_raw(state as *mut T) }));
                    }
                }
            } else if state == states::disconnected() {
                // The sender was dropped before sending anything while we prepared to park.
                unsafe { Box::from_raw(waker_ptr) };
                unsafe { Box::from_raw(state_ptr) };
                Err(RecvError)
            } else {
                // The sender sent the message while we prepared to park.
                // We take the message and free the state.
                unsafe { Box::from_raw(waker_ptr) };
                unsafe { Box::from_raw(state_ptr) };
                Ok(take(unsafe { Box::from_raw(state as *mut T) }))
            }
        } else if state == states::disconnected() {
            // The sender was dropped before sending anything, or we already received the message.
            unsafe { Box::from_raw(state_ptr) };
            Err(RecvError)
        } else {
            // The sender already sent the message. We take the message and free the state.
            unsafe { Box::from_raw(state_ptr) };
            Ok(take(unsafe { Box::from_raw(state as *mut T) }))
        }
    }

    /// Attempts to wait for a message from the [`Sender`], returning an error if the channel is
    /// disconnected. This is a non consuming version of [`Receiver::recv`], but with a bit
    /// worse performance. Prefer `[`Receiver::recv`]` if your code allows consuming the receiver.
    ///
    /// If a message is returned, the channel is disconnected and any subsequent receive operation
    /// using this receiver will return an error.
    pub fn recv_ref(&self) -> Result<T, RecvError> {
        let state_ptr = self.state_ptr;

        let state = unsafe { &*state_ptr }.load(Ordering::SeqCst);
        if state == states::init() {
            // The sender is alive but has not sent anything yet. We prepare to park.

            // Conditionally add a delay here to help the tests trigger the edge cases where
            // the sender manages to be dropped or send something before we are able to store our
            // `Thread` object in the state.
            #[cfg(oneshot_test_delay)]
            std::thread::sleep(std::time::Duration::from_millis(10));

            // Allocate and put our thread instance on the heap and then store it in the state.
            // The sender will use this to unpark us when it sends or is dropped.
            // The actor taking this object out of the state is responsible for freeing it.
            let waker_ptr = Box::into_raw(ReceiverWaker::current_thread());
            debug_assert_ne!(waker_ptr as usize, states::init());
            debug_assert_ne!(waker_ptr as usize, states::disconnected());
            let state = unsafe { &*state_ptr }.compare_and_swap(
                states::init(),
                waker_ptr as usize,
                Ordering::SeqCst,
            );
            if state == states::init() {
                // We stored our thread, now we park until the sender has changed the state
                loop {
                    thread::park();
                    // Check if the sender updated the state
                    let state = unsafe { &*state_ptr }.load(Ordering::SeqCst);
                    debug_assert_ne!(state, states::init());
                    if state == states::disconnected() {
                        // The sender was dropped while we were parked.
                        break Err(RecvError);
                    } else if state != waker_ptr as usize {
                        // The sender sent the message while we were parked.
                        // We take the message and mark the channel disconnected.
                        unsafe { &*state_ptr }.store(states::disconnected(), Ordering::SeqCst);
                        break Ok(take(unsafe { Box::from_raw(state as *mut T) }));
                    }
                }
            } else if state == states::disconnected() {
                // The sender was dropped before sending anything while we prepared to park.
                unsafe { Box::from_raw(waker_ptr) };
                Err(RecvError)
            } else {
                // The sender sent the message while we prepared to park.
                // We take the message and mark the channel disconnected.
                unsafe { Box::from_raw(waker_ptr) };
                unsafe { &*state_ptr }.store(states::disconnected(), Ordering::SeqCst);
                Ok(take(unsafe { Box::from_raw(state as *mut T) }))
            }
        } else if state == states::disconnected() {
            // The sender was dropped before sending anything, or we already received the message.
            Err(RecvError)
        } else {
            // The sender sent the message. We take the message and mark the channel disconnected.
            unsafe { &*state_ptr }.store(states::disconnected(), Ordering::SeqCst);
            Ok(take(unsafe { Box::from_raw(state as *mut T) }))
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
        let state = unsafe { &*self.state_ptr }.load(Ordering::SeqCst);
        if state == states::init() {
            // The sender is alive but has not sent anything yet.
            Err(TryRecvError::Empty)
        } else if state == states::disconnected() {
            // The sender was dropped before sending anything, or we already received the message.
            Err(TryRecvError::Disconnected)
        } else {
            // The sender sent the message. We take the message and mark the channel disconnected.
            unsafe { &*self.state_ptr }.store(states::disconnected(), Ordering::SeqCst);
            Ok(take(unsafe { Box::from_raw(state as *mut T) }))
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
        let state_ptr = self.state_ptr;

        let state = unsafe { &*state_ptr }.load(Ordering::SeqCst);
        if state == states::init() {
            // The sender is alive but has not sent anything yet. We prepare to park.

            // Conditionally add a delay here to help the tests trigger the edge cases where
            // the sender manages to be dropped or send something before we are able to store our
            // `Thread` object in the state.
            #[cfg(oneshot_test_delay)]
            std::thread::sleep(std::time::Duration::from_millis(10));

            // Allocate and put our thread instance on the heap and then store it in the state.
            // The sender will use this to unpark us when it sends or is dropped.
            // The actor taking this object out of the state is responsible for freeing it.
            let waker_ptr = Box::into_raw(ReceiverWaker::current_thread());
            debug_assert_ne!(waker_ptr as usize, states::init());
            debug_assert_ne!(waker_ptr as usize, states::disconnected());
            let state = unsafe { &*state_ptr }.compare_and_swap(
                states::init(),
                waker_ptr as usize,
                Ordering::SeqCst,
            );
            if state == states::init() {
                // We stored our thread, now we park until the sender has changed the state
                loop {
                    if let Some(timeout) = deadline.checked_duration_since(Instant::now()) {
                        thread::park_timeout(timeout);
                    } else {
                        // We reached the deadline. Take our thread object out of the state again.
                        let state = unsafe { &*state_ptr }.swap(states::init(), Ordering::SeqCst);
                        debug_assert_ne!(state, states::init());
                        if state == waker_ptr as usize {
                            // The sender has not touched the state. We took out the thread object.
                            unsafe { Box::from_raw(waker_ptr) };
                            break Err(RecvTimeoutError::Timeout);
                        } else if state == states::disconnected() {
                            // The sender was dropped while we were parked.
                            unsafe { &*state_ptr }.store(states::disconnected(), Ordering::SeqCst);
                            break Err(RecvTimeoutError::Disconnected);
                        } else {
                            // The sender sent the message while we were parked.
                            // We take the message and mark the channel disconnected.
                            unsafe { &*state_ptr }.store(states::disconnected(), Ordering::SeqCst);
                            break Ok(take(unsafe { Box::from_raw(state as *mut T) }));
                        }
                    }
                    // Check if the sender updated the state
                    let state = unsafe { &*state_ptr }.load(Ordering::SeqCst);
                    debug_assert_ne!(state, states::init());
                    if state == states::disconnected() {
                        // The sender was dropped while we were parked.
                        break Err(RecvTimeoutError::Disconnected);
                    } else if state != waker_ptr as usize {
                        // The sender sent the message while we were parked.
                        // We take the message and mark the channel disconnected.
                        unsafe { &*state_ptr }.store(states::disconnected(), Ordering::SeqCst);
                        break Ok(take(unsafe { Box::from_raw(state as *mut T) }));
                    }
                }
            } else if state == states::disconnected() {
                // The sender was dropped before sending anything while we prepared to park.
                unsafe { Box::from_raw(waker_ptr) };
                Err(RecvTimeoutError::Disconnected)
            } else {
                // The sender sent the message while we prepared to park.
                // We take the message and mark the channel disconnected.
                unsafe { Box::from_raw(waker_ptr) };
                unsafe { &*state_ptr }.store(states::disconnected(), Ordering::SeqCst);
                Ok(take(unsafe { Box::from_raw(state as *mut T) }))
            }
        } else if state == states::disconnected() {
            // The sender was dropped before sending anything, or we already received the message.
            Err(RecvTimeoutError::Disconnected)
        } else {
            // The sender sent the message. We take the message and mark the channel disconnected.
            unsafe { &*state_ptr }.store(states::disconnected(), Ordering::SeqCst);
            Ok(take(unsafe { Box::from_raw(state as *mut T) }))
        }
    }
}

impl<T> core::future::Future for Receiver<T> {
    type Output = Result<T, RecvError>;

    fn poll(self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<Self::Output> {
        match self.try_recv() {
            Err(TryRecvError::Empty) => {
                // Allocate and put our waker instance on the heap and then store it in the state.
                // The sender will use this to unpark us when it sends or is dropped.
                // The actor taking this object out of the state is responsible for freeing it.
                let waker_ptr = Box::into_raw(ReceiverWaker::task_waker(cx));
                debug_assert_ne!(waker_ptr as usize, states::init());
                debug_assert_ne!(waker_ptr as usize, states::disconnected());
                let state = unsafe { &*self.state_ptr }.compare_and_swap(
                    states::init(),
                    waker_ptr as usize,
                    Ordering::SeqCst,
                );
                if state == states::init() {
                    // We stored our waker, now we return and let the sender wake us up
                    Poll::Pending
                } else if state == states::disconnected() {
                    // The sender was dropped before sending anything while we prepared to park.
                    unsafe { Box::from_raw(waker_ptr) };
                    Poll::Ready(Err(RecvError))
                } else {
                    // The sender sent the message while we prepared to park.
                    // We take the message and mark the channel disconnected.
                    unsafe { Box::from_raw(waker_ptr) };
                    unsafe { &*self.state_ptr }.store(states::disconnected(), Ordering::SeqCst);
                    Poll::Ready(Ok(take(unsafe { Box::from_raw(state as *mut T) })))
                }
            }
            Err(TryRecvError::Disconnected) => Poll::Ready(Err(RecvError)),
            Ok(message) => Poll::Ready(Ok(message)),
        }
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        let state = unsafe { &*self.state_ptr }.swap(states::disconnected(), Ordering::SeqCst);
        if state == states::init() {
            // The sender has not sent anything, nor is it dropped. The sender is responsible for
            // freeing the state
        } else if state == states::disconnected() {
            // The sender was already dropped. We are responsible for freeing the state
            unsafe { Box::from_raw(self.state_ptr) };
        } else {
            // The sender already sent something. We must free it, and our state
            unsafe { Box::from_raw(self.state_ptr) };
            unsafe { Box::from_raw(state as *mut T) };
        }
    }
}

mod states {
    static INIT: u8 = 1u8;
    static DISCONNECTED: u8 = 2u8;

    /// Returns a memory address in integer form representing the initial state of a channel.
    /// This state is active while both the sender and receiver are still alive, no message
    /// has yet been sent and the receiver has not started receiving.
    ///
    /// The return value is guaranteed to:
    /// * be the same for every call in the same process
    /// * be different from what `states::disconnected` returns
    /// * and never equal a pointer returned from `Box::into_raw`.
    #[inline(always)]
    pub fn init() -> usize {
        &INIT as *const u8 as usize
    }

    /// Returns a memory address in integer form representing a disconnected channel.
    /// A channel is disconnected when one end has been dropped or a sent message has been received.
    ///
    /// The return value is guaranteed to:
    /// * be the same for every call in the same process
    /// * be different from what `states::init` returns
    /// * and never equal a pointer returned from `Box::into_raw`.
    #[inline(always)]
    pub fn disconnected() -> usize {
        &DISCONNECTED as *const u8 as usize
    }

    #[test]
    fn special_state_values_sane() {
        assert_ne!(init(), 0);
        assert_ne!(disconnected(), 0);
        assert_ne!(init(), disconnected());
    }
}

/// Consumes a box and returns the value within on the stack. A workaround since custom box
/// implementations can't implement this the same way as the standard library box does.
#[allow(clippy::boxed_local)]
#[inline(always)]
fn take<T>(b: Box<T>) -> T {
    #[cfg(not(feature = "loom"))]
    {
        *b
    }
    #[cfg(feature = "loom")]
    {
        b.into_value()
    }
}
