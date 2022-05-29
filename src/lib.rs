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
//! # Examples
//!
//! A very basic example to just show the API:
//!
//! ```rust
//! # #[cfg(not(feature = "sync"))]
//! # fn main() {}
//! # #[cfg(feature = "sync")]
//! # fn main() {
//! # use std::thread;
//! let (sender, receiver) = oneshot::channel();
//! thread::spawn(move || {
//!     sender.send("Hello from worker thread!");
//! });
//!
//! let message = receiver.recv().expect("Worker thread does not want to talk :(");
//! println!("A message from a different thread: {}", message);
//! # }
//! ```
//!
//! A slightly larger example showing communicating back work of *different types* during a
//! long computation. The final result here could have been communicated back via the thread's
//! `JoinHandle`. But those can't be waited on with a timeout. This is a quite artificial example,
//! that mostly shows the API.
//!
//! ```rust
//! # #[cfg(not(feature = "sync"))]
//! # fn main() {}
//! # #[cfg(feature = "sync")]
//! # fn main() {
//! # use core::time::Duration;
//! # use std::thread;
//! # fn expensive_initialization() -> Data { Data }
//! # struct Data;
//! # impl Data {
//! #     fn summary(&self) -> &'static str { "" }
//! #     fn expensive_computation(self) -> Vec<u8> { Vec::new() }
//! # }
//! let (sender1, receiver1) = oneshot::channel();
//! let (sender2, receiver2) = oneshot::channel();
//!
//! let thread = thread::spawn(move || {
//!     let data_processor = expensive_initialization();
//!     sender1.send(data_processor.summary()).expect("Main thread not waiting");
//!     sender2.send(data_processor.expensive_computation()).expect("Main thread not waiting");
//! });
//!
//! let summary = receiver1.recv().expect("Worker thread died");
//! println!("Initialized data. Will crunch these numbers: {}", summary);
//!
//! let result = loop {
//!     match receiver2.recv_timeout(Duration::from_secs(1)) {
//!         Ok(result) => break result,
//!         Err(oneshot::RecvTimeoutError::Timeout) => println!("Still working..."),
//!         Err(oneshot::RecvTimeoutError::Disconnected) => panic!("Worker thread died"),
//!     }
//! };
//! println!("Done computing. Results: {:?}", result);
//! thread.join().expect("Worker thread panicked");
//! # }
//! ```
//!
//! # Sync vs async
//!
//! The main motivation for writing this library was that there were no (known to me) channel
//! implementations allowing you to seamlessly send messages between a normal thread and an async
//! task, or the other way around. If message passing is the way you are communicating, of course
//! that should work smoothly between the sync and async parts of the program!
//!
//! This library achieves that by having an almost[1] wait-free send operation that can safely
//! be used in both sync threads and async tasks. The receiver has both thread blocking
//! receive methods for synchronous usage, and implements `Future` for asynchronous usage.
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

use core::{
    marker::PhantomData,
    mem::{self, MaybeUninit},
    ptr::{self, NonNull},
};

#[cfg(not(loom))]
use core::{
    cell::UnsafeCell,
    hint,
    sync::atomic::{fence, AtomicU8, Ordering::*},
};
#[cfg(loom)]
use loom::{
    cell::UnsafeCell,
    hint,
    sync::atomic::{fence, AtomicU8, Ordering::*},
};

#[cfg(feature = "async")]
use core::{
    pin::Pin,
    task::{self, Poll},
};
#[cfg(feature = "std")]
use std::time::{Duration, Instant};

#[cfg(feature = "std")]
mod thread {
    #[cfg(not(loom))]
    pub use std::thread::{current, park, park_timeout, yield_now, Thread};

    #[cfg(loom)]
    pub use loom::thread::{current, park, yield_now, Thread};

    // loom does not support parking with a timeout. So we just
    // yield. This means that the "park" will "spuriously" wake up
    // way too early. But the code should properly handle this.
    // One thing to note is that very short timeouts are needed
    // when using loom, since otherwise the looping will cause
    // an overflow in loom.
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

    // SAFETY: `channel_ptr` came from a Box and thus is not null
    let channel_ptr = unsafe { NonNull::new_unchecked(channel_ptr) };

    (
        Sender {
            channel_ptr,
            _invariant: PhantomData,
        },
        Receiver { channel_ptr },
    )
}

#[derive(Debug)]
pub struct Sender<T> {
    channel_ptr: NonNull<Channel<T>>,
    // In reality we want contravariance, however we can't obtain that.
    //
    // Consider the following scenario:
    // ```
    // let (mut tx, rx) = channel::<&'short u8>();
    // let (tx2, rx2) = channel::<&'long u8>();
    //
    // tx = tx2;
    //
    // // Pretend short_ref is some &'short u8
    // tx.send(short_ref).unwrap();
    // let long_ref = rx2.recv().unwrap();
    // ```
    //
    // If this type were covariant then we could safely extend lifetimes, which is not okay.
    // Hence, we enforce invariance.
    _invariant: PhantomData<fn(T) -> T>,
}

#[derive(Debug)]
pub struct Receiver<T> {
    // Covariance is the right choice here. Consider the example presented in Sender, and you'll
    // see that if we replaced `rx` instead then we would get the expected behavior
    channel_ptr: NonNull<Channel<T>>,
}

unsafe impl<T: Send> Send for Sender<T> {}
unsafe impl<T: Send> Send for Receiver<T> {}

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
        let channel_ptr = self.channel_ptr;

        // Don't run our Drop implementation if send was called, any cleanup now happens here
        mem::forget(self);

        // SAFETY: The channel exists on the heap for the entire duration of this method and we
        // only ever acquire shared references to it. Note that if the receiver disconnects it
        // does not free the channel.
        let channel = unsafe { channel_ptr.as_ref() };

        // Write the message into the channel on the heap.
        // SAFETY: The receiver only ever accesses this memory location if we are in the MESSAGE
        // state, and since we're responsible for setting that state, we can guarantee that we have
        // exclusive access to this memory location to perform this write.
        unsafe { channel.write_message(message) };

        // Set the state to signal there is a message on the channel.
        // ORDERING: we use release ordering to ensure the write of the message is visible to the
        // receiving thread. The EMPTY and DISCONNECTED branches do not observe any shared state,
        // and thus we do not need acquire orderng. The RECEIVING branch manages synchronization
        // independent of this operation.
        //
        // EMPTY + 1 = MESSAGE
        // RECEIVING + 1 = UNPARKING
        // DISCONNECTED + 1 = invalid, however this state is never observed
        match channel.state.fetch_add(1, Release) {
            // The receiver is alive and has not started waiting. Send done.
            EMPTY => Ok(()),
            // The receiver is waiting. Wake it up so it can return the message.
            RECEIVING => {
                // ORDERING: Synchronizes with the write of the waker to memory, and prevents the
                // taking of the waker from being ordered before this operation.
                fence(Acquire);

                // Take the waker, but critically do not unpark it. If we unparked now, then the
                // receiving thread could still observe the UNPARKING state and re-park, meaning
                // that after we change to the MESSAGE state, it would remain parked indefinitely
                // or until a spurious wakeup.
                // SAFETY: at this point we are in the UNPARKING state, and the receiving thread
                // does not access the waker while in this state, nor does it free the channel
                // allocation in this state.
                let waker = unsafe { channel.take_waker() };

                // ORDERING: this ordering serves two-fold: it synchronizes with the acquire load
                // the state in the receiving thread, ensuring that both our read of the waker and
                // write of the message happen-before the taking of the message and freeing of the
                // channel.
                channel.state.store(MESSAGE, Release);

                // Note: it is possible that between the store above and this statement that
                // the receiving thread is spuriously unparked, takes the message, and frees
                // the channel allocation. However, we took ownership of the channel out of
                // that allocation, and freeing the channel does not drop the waker since the
                // waker is wrapped in MaybeUninit. Therefore this data is valid regardless of
                // whether or not the receive has completed by this point.
                waker.unpark();

                Ok(())
            }
            // The receiver was already dropped. The error is responsible for freeing the channel.
            // SAFETY: since the receiver disconnected it will no longer access `channel_ptr`, so
            // we can transfer exclusive ownership of the channel's resources to it. Moreover,
            // since we just placed the message in the channel, the channel contains a valid
            // message.
            DISCONNECTED => Err(unsafe { SendError::new(channel_ptr) }),
            _ => unreachable!(),
        }
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        // SAFETY: The receiver only ever frees the channel if we are in the MESSAGE or
        // DISCONNECTED states. If we are in the MESSAGE state, then we called
        // mem::forget(self), so we should not be in this function call. If we are in the
        // DISCONNECTED state, then the receiver either received a MESSAGE so this statement is
        // unreachable, or was dropped and observed that our side was still alive, and thus didn't
        // free the channel.
        let channel = unsafe { self.channel_ptr.as_ref() };

        // Set the channel state to disconnected and read what state the receiver was in
        // ORDERING: we don't need release ordering here since there are no modifications we
        // need to make visible to other thread, and the Err(RECEIVING) branch handles
        // synchronization independent of this cmpxchg
        //
        // EMPTY ^ 001 = DISCONNECTED
        // RECEIVING ^ 001 = UNPARKING
        // DISCONNECTED ^ 001 = EMPTY (invalid), but this state is never observed
        match channel.state.fetch_xor(0b001, Relaxed) {
            // The receiver has not started waiting, nor is it dropped.
            EMPTY => (),
            // The receiver is waiting. Wake it up so it can detect that the channel disconnected.
            RECEIVING => {
                // See comments in Sender::send

                fence(Acquire);

                let waker = unsafe { channel.take_waker() };

                // We still need release ordering here to make sure our read of the waker happens
                // before this.
                channel.state.store(DISCONNECTED, Release);

                waker.unpark();
            }
            // The receiver was already dropped. We are responsible for freeing the channel.
            DISCONNECTED => {
                // SAFETY: when the receiver switches the state to DISCONNECTED they have received
                // the message or will no longer be trying to receive the message, and have
                // observed that the sender is still alive, meaning that we're responsible for
                // freeing the channel allocation.
                unsafe { dealloc(self.channel_ptr) };
            }
            _ => unreachable!(),
        }
    }
}

impl<T> Receiver<T> {
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
        let channel = unsafe { self.channel_ptr.as_ref() };

        // ORDERING: we use acquire ordering to synchronize with the store of the message
        match channel.state.load(Acquire) {
            MESSAGE => {
                // It's okay to break up the load and store since once we're in the message state
                // the sender no longer modifies the state
                // ORDERING: at this point the sender has done its job and is no longer active, so
                // we don't need to make any side effects visible to it
                channel.state.store(DISCONNECTED, Relaxed);

                // SAFETY: we are in the MESSAGE state so the message is present
                Ok(unsafe { channel.take_message() })
            }
            EMPTY => Err(TryRecvError::Empty),
            DISCONNECTED => Err(TryRecvError::Disconnected),
            #[cfg(feature = "async")]
            RECEIVING => Err(TryRecvError::Empty),
            _ => unreachable!(),
        }
    }

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
        // Note that we don't need to worry about changing the state to disconnected or setting the
        // state to an invalid value at any point in this functon because we take ownership of
        // self, and this function does not exit until the message has been received or both side
        // of the channel are inactive and cleaned up.

        let channel_ptr = self.channel_ptr;

        // Don't run our Drop implementation if we are receiving consuming ourselves.
        mem::forget(self);

        // SAFETY: the existence of the `self` parameter serves as a certificate that the receiver
        // is still alive, meaning that even if the sender was dropped then it would have observed
        // the fact that we're still alive and left the responsibility of deallocating the
        // channel to us, so channel_ptr is valid
        let channel = unsafe { channel_ptr.as_ref() };

        // ORDERING: we use acquire ordering to synchronize with the write of the message in the
        // case that it's available
        match channel.state.load(Acquire) {
            // The sender is alive but has not sent anything yet. We prepare to park.
            EMPTY => {
                // Conditionally add a delay here to help the tests trigger the edge cases where
                // the sender manages to be dropped or send something before we are able to store
                // our waker object in the channel.
                #[cfg(oneshot_test_delay)]
                std::thread::sleep(std::time::Duration::from_millis(10));

                // Write our waker instance to the channel.
                // SAFETY: we are still in the empty state, meaning that the sender will not try
                // to access the waker until it sees the state set to RECEIVING
                unsafe { channel.write_waker(ReceiverWaker::current_thread()) };

                // Switch the state from EMPTY to RECEIVING. We need to do this in one
                // atomic step in case the sender disconnected or sent the message while we wrote
                // the waker to memory. We don't need to do a compare exchange here however because
                // if the original state was not EMPTY, then the sender has either finished sending
                // the message or is being dropped, so the RECEIVING state will never be observed
                // after we return.
                // ORDERING: we use release ordering so the sender can synchronize with our writing
                // of the waker to memory. The individual branches handle any additional
                // synchronizaton
                match channel.state.swap(RECEIVING, Release) {
                    // We stored our waker, now we park until the sender has changed the state
                    EMPTY => loop {
                        thread::park();

                        // ORDERING: synchronize with the write of the message
                        match channel.state.load(Acquire) {
                            // The sender sent the message while we were parked.
                            MESSAGE => {
                                // SAFETY: we are in the message state so the message is valid
                                let message = unsafe { channel.take_message() };

                                // SAFETY: the Sender delegates the responsibility of deallocating
                                // the channel to us upon sending the message
                                unsafe { dealloc(channel_ptr) };

                                break Ok(message);
                            }
                            // The sender was dropped while we were parked.
                            DISCONNECTED => {
                                // SAFETY: the Sender doesn't deallocate the channel allocation in
                                // its drop implementation if we're receiving
                                unsafe { dealloc(channel_ptr) };

                                break Err(RecvError);
                            }
                            // State did not change, spurious wakeup, park again.
                            RECEIVING | UNPARKING => (),
                            _ => unreachable!(),
                        }
                    },
                    // The sender sent the message while we prepared to park.
                    MESSAGE => {
                        // ORDERING: Synchronize with the write of the message. This branch is
                        // unlikely to be taken, so it's likely more efficient to use a fence here
                        // instead of AcqRel ordering on the RMW operation
                        fence(Acquire);

                        // SAFETY: we started in the empty state and the sender switched us to the
                        // message state. This means that it did not take the waker, so we're
                        // responsible for dropping it.
                        unsafe { channel.drop_waker() };

                        // SAFETY: we are in the message state so the message is valid
                        let message = unsafe { channel.take_message() };

                        // SAFETY: the Sender delegates the responsibility of deallocating the
                        // channel to us upon sending the message
                        unsafe { dealloc(channel_ptr) };

                        Ok(message)
                    }
                    // The sender was dropped before sending anything while we prepared to park.
                    DISCONNECTED => {
                        // SAFETY: we started in the empty state and the sender switched us to the
                        // disconnected state. It does not take the waker when it does this so we
                        // need to drop it.
                        unsafe { channel.drop_waker() };

                        // SAFETY: the sender does not deallocate the channel if it switches from
                        // empty to disconnected so we need to free the allocation
                        unsafe { dealloc(channel_ptr) };

                        Err(RecvError)
                    }
                    _ => unreachable!(),
                }
            }
            // The sender already sent the message.
            MESSAGE => {
                // SAFETY: we are in the message state so the message is valid
                let message = unsafe { channel.take_message() };

                // SAFETY: we are already in the message state so the sender has been forgotten
                // and it's our job to clean up resources
                unsafe { dealloc(channel_ptr) };

                Ok(message)
            }
            // The sender was dropped before sending anything, or we already received the message.
            DISCONNECTED => {
                // SAFETY: the sender does not deallocate the channel if it switches from empty to
                // disconnected so we need to free the allocation
                unsafe { dealloc(channel_ptr) };

                Err(RecvError)
            }
            // The receiver must have been `Future::poll`ed prior to this call.
            #[cfg(feature = "async")]
            RECEIVING => panic!("{}", RECEIVER_USED_SYNC_AND_ASYNC_ERROR),
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
        self.start_recv_ref(
            || RecvError,
            |channel| {
                loop {
                    thread::park();

                    // ORDERING: we use acquire ordering to synchronize with the write of the message
                    match channel.state.load(Acquire) {
                        // The sender sent the message while we were parked.
                        // We take the message and mark the channel disconnected.
                        MESSAGE => {
                            // ORDERING: the sender is inactive at this point so we don't need to make
                            // and reads or writes visible to the sending thread
                            channel.state.store(DISCONNECTED, Relaxed);

                            // SAFETY: we were just in the message state so the message is valid
                            break Ok(unsafe { channel.take_message() });
                        }
                        // The sender was dropped while we were parked.
                        DISCONNECTED => break Err(RecvError),
                        // State did not change, spurious wakeup, park again.
                        RECEIVING | UNPARKING => (),
                        _ => unreachable!(),
                    }
                }
            },
        )
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
        #[cold]
        fn wait_for_unpark<T>(channel: &Channel<T>) -> Result<T, RecvTimeoutError> {
            loop {
                thread::park();

                // Same ordering as usual
                match channel.state.load(Acquire) {
                    MESSAGE => {
                        // Same ordering and safety as usual

                        channel.state.store(DISCONNECTED, Relaxed);
                        break Ok(unsafe { channel.take_message() });
                    }
                    DISCONNECTED => {
                        break Err(RecvTimeoutError::Disconnected);
                    }
                    // We continue on the empty state here since the current implementation eagerly
                    // sets the state to EMPTY upon timeout.
                    EMPTY => (),
                    _ => unreachable!(),
                }
            }
        }

        self.start_recv_ref(
            || RecvTimeoutError::Disconnected,
            |channel| {
                loop {
                    match deadline.checked_duration_since(Instant::now()) {
                        Some(timeout) => {
                            thread::park_timeout(timeout);

                            // ORDERING: synchronize with the write of the message
                            match channel.state.load(Acquire) {
                                // The sender sent the message while we were parked.
                                MESSAGE => {
                                    // ORDERING: the sender has been `mem::forget`-ed so this update
                                    // only needs to be visible to us.
                                    channel.state.store(DISCONNECTED, Relaxed);

                                    // SAFETY: we either are in the message state or were just in the
                                    // message state
                                    break Ok(unsafe { channel.take_message() });
                                }
                                // The sender was dropped while we were parked.
                                DISCONNECTED => break Err(RecvTimeoutError::Disconnected),
                                // State did not change, spurious wakeup, park again.
                                RECEIVING | UNPARKING => (),
                                _ => unreachable!(),
                            }
                        }
                        None => {
                            // ORDERING: synchronize with the write of the message
                            match channel.state.swap(EMPTY, Acquire) {
                                // We reached the end of the timeout without receiving a message
                                RECEIVING => {
                                    // SAFETY: we were in th receiving state and are now in the empty
                                    // state, so the sender has not and will not try to read the waker,
                                    // so we have exclusive access to drop it.
                                    unsafe { channel.drop_waker() };

                                    break Err(RecvTimeoutError::Timeout);
                                }
                                // The sender sent the message while we were parked.
                                MESSAGE => {
                                    // Same safety and ordering as the Some branch

                                    channel.state.store(DISCONNECTED, Relaxed);
                                    break Ok(unsafe { channel.take_message() });
                                }
                                // The sender was dropped while we were parked.
                                DISCONNECTED => {
                                    // ORDERING: we were originally in the disconnected state meaning
                                    // that the sender is inactive and no longer observing the state,
                                    // so we only need to change it back to DISCONNECTED for if the
                                    // receiver is dropped or a recv* method is called again
                                    channel.state.store(DISCONNECTED, Relaxed);

                                    break Err(RecvTimeoutError::Disconnected);
                                }
                                UNPARKING => {
                                    // We were in the UNPARKING state and are now in the EMPTY state.
                                    // We wait to be unparked since this is the only way to maintain
                                    // correctness with intrusive memory.

                                    break wait_for_unpark(channel);
                                }
                                _ => unreachable!(),
                            }
                        }
                    }
                }
            },
        )
    }

    #[cfg(feature = "std")]
    #[inline(always)]
    fn start_recv_ref<E>(
        &self,
        disconnected: impl FnOnce() -> E,
        finish: impl FnOnce(&Channel<T>) -> Result<T, E>,
    ) -> Result<T, E> {
        // SAFETY: the existence of the `self` parameter serves as a certificate that the receiver
        // is still alive, meaning that even if the sender was dropped then it would have observed
        // the fact that we're still alive and left the responsibility of deallocating the
        // channel to us, so `self.channel` is valid
        let channel = unsafe { self.channel_ptr.as_ref() };

        // ORDERING: synchronize with the write of the message
        match channel.state.load(Acquire) {
            // The sender is alive but has not sent anything yet. We prepare to park.
            EMPTY => {
                // Conditionally add a delay here to help the tests trigger the edge cases where
                // the sender manages to be dropped or send something before we are able to store
                // our waker object in the channel.
                #[cfg(oneshot_test_delay)]
                std::thread::sleep(std::time::Duration::from_millis(10));

                // Write our waker instance to the channel.
                unsafe {
                    channel.write_waker(ReceiverWaker::current_thread());
                }

                // ORDERING: we use release ordering on success so the sender can synchronize with
                // our write of the waker. We use relaxed ordering on failure since the sender does
                // not need to syncrhonize with our write and the individual match arms handle any
                // additional synchronization
                match channel
                    .state
                    .compare_exchange(EMPTY, RECEIVING, Release, Relaxed)
                {
                    // We stored our waker, now we delegate to the callback to finish the receive
                    // operation
                    Ok(EMPTY) => finish(channel),
                    // The sender sent the message while we prepared to finish
                    Err(MESSAGE) => {
                        // See comments in `recv` for ordering and safety

                        fence(Acquire);

                        unsafe { channel.drop_waker() };

                        // ORDERING: the sender has been `mem::forget`-ed so this update only
                        // needs to be visible to us
                        channel.state.store(DISCONNECTED, Relaxed);

                        Ok(unsafe { channel.take_message() })
                    }
                    // The sender was dropped before sending anything while we prepared to park.
                    Err(DISCONNECTED) => {
                        // See comments in `recv` for safety
                        unsafe { channel.drop_waker() };
                        Err(disconnected())
                    }
                    _ => unreachable!(),
                }
            }
            // The sender sent the message. We take the message and mark the channel disconnected.
            MESSAGE => {
                // ORDERING: the sender has been `mem::forget`-ed so this update only needs to be
                // visible to us
                channel.state.store(DISCONNECTED, Relaxed);

                // SAFETY: we are in the message state so the message is valid
                Ok(unsafe { channel.take_message() })
            }
            // The sender was dropped before sending anything, or we already received the message.
            DISCONNECTED => Err(disconnected()),
            // The receiver must have been `Future::poll`ed prior to this call.
            #[cfg(feature = "async")]
            RECEIVING => panic!("{}", RECEIVER_USED_SYNC_AND_ASYNC_ERROR),
            _ => unreachable!(),
        }
    }
}

#[cfg(feature = "async")]
impl<T> core::future::Future for Receiver<T> {
    type Output = Result<T, RecvError>;

    fn poll(self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<Self::Output> {
        // TODO: fix this. all of it.

        // SAFETY: The channel will not be freed while this method is still running.
        let channel = unsafe { self.channel_ptr.as_ref() };

        match channel.state.load(Acquire) {
            // The sender is alive but has not sent anything yet.
            EMPTY => unsafe { channel.write_async_waker(cx) },
            // We were polled again while waiting for the sender. Replace the waker with the new one.
            RECEIVING => {
                match channel
                    .state
                    .compare_exchange(RECEIVING, EMPTY, Relaxed, Relaxed)
                {
                    // We successfully changed the state back to EMPTY. Replace the waker.
                    Ok(RECEIVING) => {
                        unsafe { channel.drop_waker() };
                        unsafe { channel.write_async_waker(cx) }
                    }
                    // The sender sent the message while we prepared to replace the waker.
                    // We take the message and mark the channel disconnected.
                    // The sender has already taken the waker.
                    Err(MESSAGE) => {
                        fence(Acquire);
                        channel.state.store(DISCONNECTED, Relaxed);
                        Poll::Ready(Ok(unsafe { channel.take_message() }))
                    }
                    // The sender was dropped before sending anything while we prepared to park.
                    // The sender has taken the waker already.
                    Err(DISCONNECTED) => Poll::Ready(Err(RecvError)),
                    Err(UNPARKING) => {
                        cx.waker().wake_by_ref();
                        hint::spin_loop();
                        Poll::Pending
                    }
                    _ => unreachable!(),
                }
            }
            // The sender sent the message.
            MESSAGE => {
                channel.state.store(DISCONNECTED, Relaxed);
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
        // SAFETY: since the receiving side is still alive the sender would have observed that and
        // left deallocating the channel allocation to us.
        let channel = unsafe { self.channel_ptr.as_ref() };

        // Set the channel state to disconnected and read what state the receiver was in
        match channel.state.swap(DISCONNECTED, Acquire) {
            // The sender has not sent anything, nor is it dropped.
            EMPTY => (),
            // The sender already sent something. We must drop it, and free the channel.
            MESSAGE => {
                // SAFETY: we are in the message state so the message is initialized
                unsafe { channel.drop_message() };

                // SAFETY: see safety comment at top of function
                unsafe { dealloc(self.channel_ptr) };
            }
            // The receiver has been polled.
            #[cfg(feature = "async")]
            RECEIVING => {
                // TODO: figure this out when async is fixed
                unsafe { channel.drop_waker() };
            }
            // The sender was already dropped. We are responsible for freeing the channel.
            DISCONNECTED => {
                // SAFETY: see safety comment at top of function
                unsafe { dealloc(self.channel_ptr) };
            }
            _ => unreachable!(),
        }
    }
}

/// All the values that the `Channel::state` field can have during the lifetime of a channel.
mod states {
    // These values are very explicitly chosen so that we can replace some cmpxchg calls with
    // fetch_* calls.

    /// The initial channel state. Active while both endpoints are still alive, no message has been
    /// sent, and the receiver is not receiving.
    pub const EMPTY: u8 = 0b011;
    /// A message has been sent to the channel, but the receiver has not yet read it.
    pub const MESSAGE: u8 = 0b100;
    /// No message has yet been sent on the channel, but the receiver is currently receiving.
    pub const RECEIVING: u8 = 0b000;
    pub const UNPARKING: u8 = 0b001;
    /// The channel has been closed. This means that either the sender or receiver has been dropped,
    /// or the message sent to the channel has already been received. Since this is a oneshot
    /// channel, it is disconnected after the one message it is supposed to hold has been
    /// transmitted.
    pub const DISCONNECTED: u8 = 0b010;
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
    message: UnsafeCell<MaybeUninit<T>>,
    waker: UnsafeCell<MaybeUninit<ReceiverWaker>>,
}

impl<T> Channel<T> {
    pub fn new() -> Self {
        Self {
            state: AtomicU8::new(EMPTY),
            message: UnsafeCell::new(MaybeUninit::uninit()),
            waker: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }

    #[inline(always)]
    unsafe fn message(&self) -> &MaybeUninit<T> {
        #[cfg(loom)]
        {
            self.message.with(|ptr| &*ptr)
        }

        #[cfg(not(loom))]
        {
            &*self.message.get()
        }
    }

    #[inline(always)]
    unsafe fn with_message_mut<F>(&self, op: F)
    where
        F: FnOnce(&mut MaybeUninit<T>),
    {
        #[cfg(loom)]
        {
            self.message.with_mut(|ptr| op(&mut *ptr))
        }

        #[cfg(not(loom))]
        {
            op(&mut *self.message.get())
        }
    }

    #[inline(always)]
    #[cfg(any(feature = "std", feature = "async"))]
    unsafe fn with_waker_mut<F>(&self, op: F)
    where
        F: FnOnce(&mut MaybeUninit<ReceiverWaker>),
    {
        #[cfg(loom)]
        {
            self.waker.with_mut(|ptr| op(&mut *ptr))
        }

        #[cfg(not(loom))]
        {
            op(&mut *self.waker.get())
        }
    }

    #[inline(always)]
    unsafe fn write_message(&self, message: T) {
        self.with_message_mut(|slot| slot.as_mut_ptr().write(message));
    }

    #[inline(always)]
    unsafe fn take_message(&self) -> T {
        #[cfg(loom)]
        {
            self.message.with(|ptr| ptr::read(ptr)).assume_init()
        }

        #[cfg(not(loom))]
        {
            ptr::read(self.message.get()).assume_init()
        }
    }

    #[inline(always)]
    unsafe fn drop_message(&self) {
        self.with_message_mut(|slot| slot.assume_init_drop());
    }

    #[cfg(any(feature = "std", feature = "async"))]
    #[inline(always)]
    unsafe fn write_waker(&self, waker: ReceiverWaker) {
        self.with_waker_mut(|slot| slot.as_mut_ptr().write(waker));
    }

    #[inline(always)]
    unsafe fn take_waker(&self) -> ReceiverWaker {
        #[cfg(loom)]
        {
            self.waker.with(|ptr| ptr::read(ptr)).assume_init()
        }

        #[cfg(not(loom))]
        {
            ptr::read(self.waker.get()).assume_init()
        }
    }

    #[cfg(any(feature = "std", feature = "async"))]
    #[inline(always)]
    unsafe fn drop_waker(&self) {
        self.with_waker_mut(|slot| slot.assume_init_drop());
    }

    #[cfg(feature = "async")]
    unsafe fn write_async_waker(&self, cx: &mut task::Context<'_>) -> Poll<Result<T, RecvError>> {
        // Write our thread instance to the channel.
        self.write_waker(ReceiverWaker::task_waker(cx));

        match self
            .state
            .compare_exchange(EMPTY, RECEIVING, AcqRel, Acquire)
        {
            // We stored our waker, now we return and let the sender wake us up
            Ok(EMPTY) => Poll::Pending,
            // The sender was dropped before sending anything while we prepared to park.
            Err(DISCONNECTED) => {
                self.drop_waker();
                Poll::Ready(Err(RecvError))
            }
            // The sender sent the message while we prepared to park.
            // We take the message and mark the channel disconnected.
            Err(MESSAGE) => {
                self.drop_waker();
                self.state.store(DISCONNECTED, Relaxed);
                Poll::Ready(Ok(self.take_message()))
            }
            _ => unreachable!(),
        }
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

#[cfg(not(loom))]
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

#[inline]
pub(crate) unsafe fn dealloc<T>(channel: NonNull<Channel<T>>) {
    drop(Box::from_raw(channel.as_ptr()))
}
