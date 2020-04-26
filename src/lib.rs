//! Oneshot spsc channel working both in and between sync and async environments.

#![deny(rust_2018_idioms)]

use core::fmt;
use core::mem;
#[cfg(not(all(loom, test)))]
use core::sync::atomic::{AtomicUsize, Ordering};
#[cfg(all(loom, test))]
use loom::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

mod thread {
    pub use std::thread::{current, Thread};

    #[cfg(all(loom, test))]
    pub use loom::thread::yield_now as park;
    #[cfg(not(all(loom, test)))]
    pub use std::thread::{park, park_timeout};

    #[cfg(all(loom, test))]
    pub fn park_timeout(_timeout: std::time::Duration) {
        loom::thread::yield_now()
    }
}

/// Creates a new oneshot channel and returns the two endpoints, [`Sender`] and [`Receiver`].
pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
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
    /// Sends `value` over the channel to the [`Receiver`].
    /// Returns an error if the receiver has already been dropped. The value can
    /// be extracted from the error.
    pub fn send(self, value: T) -> Result<(), DroppedReceiverError<T>> {
        let state_ptr = self.state_ptr;
        // Don't run our Drop implementation if send was called, any cleanup now happens here
        mem::forget(self);

        // Put the value on the heap and get the pointer. If sending succeeds the receiver is
        // responsible for freeing it, otherwise we do that
        let value_ptr = Box::into_raw(Box::new(value));

        // Store the address to the value in the state and read out what state the receiver is in
        let state = unsafe { &*state_ptr }.swap(value_ptr as usize, Ordering::SeqCst);
        if state == states::init() {
            // The receiver is alive and has not started waiting. Send done
            // Receiver frees state and value from heap
            Ok(())
        } else if state == states::closed() {
            // The receiver was already dropped. We are responsible for freeing the state and value
            unsafe { Box::from_raw(state_ptr) };
            Err(DroppedReceiverError(unsafe { Box::from_raw(value_ptr) }))
        } else {
            // The receiver is waiting. Wake it up so it can return the value. The receiver frees
            // the state, the value. We free the thread instance in the state
            unsafe { Box::from_raw(state as *mut thread::Thread) }.unpark();
            Ok(())
        }
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        // Set the channel state to closed and read what state the receiver was in
        let state = unsafe { &*self.state_ptr }.swap(states::closed(), Ordering::SeqCst);
        if state == states::init() {
            // The receiver has not started waiting, nor is it dropped. Nothing to do.
            // The receiver is responsible for freeing the state.
        } else if state == states::closed() {
            // The receiver was already dropped. We are responsible for freeing the state
            unsafe { Box::from_raw(self.state_ptr) };
        } else {
            // The receiver started waiting. Wake it up so it can detect that the channel closed.
            // The receiver frees the state. We free the thread instance in the state
            unsafe { Box::from_raw(state as *mut thread::Thread) }.unpark();
        }
    }
}

impl<T> Receiver<T> {
    /// Attempts to wait for a value from the [`Sender`], returning an error if the channel is
    /// closed.
    ///
    /// This method will always block the current thread if there is no data available and it's
    /// still possible for the value to be sent. Once the value is sent to the corresponding
    /// [`Sender`], then this receiver will wake up and return that message.
    ///
    /// If the corresponding [`Sender`] has disconnected (been dropped), or it disconnects while
    /// this call is blocking, this call will wake up and return Err to indicate that the value
    /// can never be received on this channel. However, since channels are buffered, if the value is
    /// sent before the disconnect, it will still be properly received.
    ///
    /// If a sent value has already been extracted from this channel this method will return an
    /// error.
    pub fn recv(self) -> Result<T, DroppedSenderError> {
        let state_ptr = self.state_ptr;
        // Don't run our Drop implementation if we are receiving
        mem::forget(self);

        let state = unsafe { &*state_ptr }.load(Ordering::SeqCst);
        if state == states::init() {
            // The sender is alive but has not sent anything yet. We prepare to park.

            // Conditionally add a delay here to help the tests trigger the edge cases where
            // the sender manages to be dropped or send something before we are able to store our
            // `Thread` object in the state.
            #[cfg(feature = "test-delay")]
            std::thread::sleep(std::time::Duration::from_millis(10));

            // Allocate and put our thread instance on the heap and then store it in the state.
            // The sender will use this to unpark us when it sends or is dropped.
            // The actor taking this object out of the state is responsible for freeing it.
            let thread_ptr = Box::into_raw(Box::new(thread::current()));
            let state = unsafe { &*state_ptr }.compare_and_swap(
                states::init(),
                thread_ptr as usize,
                Ordering::SeqCst,
            );
            if state == states::init() {
                // We stored our thread, now we park until the sender has changed the state
                loop {
                    thread::park();
                    // Check if the sender updated the state
                    let state = unsafe { &*state_ptr }.load(Ordering::SeqCst);
                    if state == states::closed() {
                        // The sender was dropped while we were parked.
                        unsafe { Box::from_raw(state_ptr) };
                        break Err(DroppedSenderError(()));
                    } else if state != thread_ptr as usize {
                        // The sender sent data while we were parked.
                        // We take the value and free the state.
                        unsafe { Box::from_raw(state_ptr) };
                        break Ok(unsafe { *Box::from_raw(state as *mut T) });
                    }
                }
            } else if state == states::closed() {
                // The sender was dropped before sending anything while we prepared to park.
                unsafe { Box::from_raw(thread_ptr) };
                unsafe { Box::from_raw(state_ptr) };
                Err(DroppedSenderError(()))
            } else {
                // The sender sent data while we prepared to park.
                // We take the value and free the state.
                unsafe { Box::from_raw(thread_ptr) };
                unsafe { Box::from_raw(state_ptr) };
                Ok(unsafe { *Box::from_raw(state as *mut T) })
            }
        } else if state == states::closed() {
            // The sender was dropped before sending anything, or we already took the value.
            unsafe { Box::from_raw(state_ptr) };
            Err(DroppedSenderError(()))
        } else {
            // The sender already sent a value. We take the value and free the state.
            unsafe { Box::from_raw(state_ptr) };
            Ok(unsafe { *Box::from_raw(state as *mut T) })
        }
    }

    pub fn recv_ref(&self) -> Result<T, DroppedSenderError> {
        let state_ptr = self.state_ptr;

        let state = unsafe { &*state_ptr }.load(Ordering::SeqCst);
        if state == states::init() {
            // The sender is alive but has not sent anything yet. We prepare to park.

            // Conditionally add a delay here to help the tests trigger the edge cases where
            // the sender manages to be dropped or send something before we are able to store our
            // `Thread` object in the state.
            #[cfg(feature = "test-delay")]
            std::thread::sleep(std::time::Duration::from_millis(10));

            // Allocate and put our thread instance on the heap and then store it in the state.
            // The sender will use this to unpark us when it sends or is dropped.
            // The actor taking this object out of the state is responsible for freeing it.
            let thread_ptr = Box::into_raw(Box::new(thread::current()));
            let state = unsafe { &*state_ptr }.compare_and_swap(
                states::init(),
                thread_ptr as usize,
                Ordering::SeqCst,
            );
            if state == states::init() {
                // We stored our thread, now we park until the sender has changed the state
                loop {
                    thread::park();
                    // Check if the sender updated the state
                    let state = unsafe { &*state_ptr }.load(Ordering::SeqCst);
                    if state == states::closed() {
                        // The sender was dropped while we were parked.
                        break Err(DroppedSenderError(()));
                    } else if state != thread_ptr as usize {
                        // The sender sent data while we were parked.
                        // We take the value and treat the channel as closed.
                        unsafe { &*state_ptr }.store(states::closed(), Ordering::SeqCst);
                        break Ok(unsafe { *Box::from_raw(state as *mut T) });
                    }
                }
            } else if state == states::closed() {
                // The sender was dropped before sending anything while we prepared to park.
                unsafe { Box::from_raw(thread_ptr) };
                Err(DroppedSenderError(()))
            } else {
                // The sender sent data while we prepared to park.
                // We take the value and treat the channel as closed.
                unsafe { Box::from_raw(thread_ptr) };
                unsafe { &*state_ptr }.store(states::closed(), Ordering::SeqCst);
                Ok(unsafe { *Box::from_raw(state as *mut T) })
            }
        } else if state == states::closed() {
            // The sender was dropped before sending anything, or we already took the value.
            Err(DroppedSenderError(()))
        } else {
            // The sender already sent a value. We take the value and treat the channel as closed.
            unsafe { &*state_ptr }.store(states::closed(), Ordering::SeqCst);
            Ok(unsafe { *Box::from_raw(state as *mut T) })
        }
    }

    /// Checks if there is a value in the channel without blocking. Returns:
    ///  * `Ok(Some(value))` if there is a value in the channel.
    ///  * `Ok(None)` if the sender has not yet sent a value but the channel is still open.
    ///  * `Err` if the sender was dropped before sending anything or if the value has already
    ///    been extracted by previous receive calls.
    pub fn try_recv(&self) -> Result<Option<T>, DroppedSenderError> {
        let state = unsafe { &*self.state_ptr }.load(Ordering::SeqCst);
        if state == states::init() {
            // The sender is alive but has not sent anything yet.
            Ok(None)
        } else if state == states::closed() {
            // The sender was already dropped before sending anything.
            Err(DroppedSenderError(()))
        } else {
            // The sender already sent a value. We take the value and treat the channel as closed.
            unsafe { &*self.state_ptr }.store(states::closed(), Ordering::SeqCst);
            Ok(Some(unsafe { *Box::from_raw(state as *mut T) }))
        }
    }

    /// Like [`Receiver::recv`], but will not block longer than `timeout`. Returns:
    ///  * `Ok(Some(value))` if there is a value in the channel before `timeout` elapses.
    ///  * `Ok(None)` if there is no value in the channel before `timeout` elapses.
    ///  * `Err` if the sender was dropped before sending anything or if the value has already
    ///    been extracted by previous receive calls.
    pub fn recv_timeout(&self, timeout: Duration) -> Result<Option<T>, DroppedSenderError> {
        match Instant::now().checked_add(timeout) {
            Some(deadline) => self.recv_deadline(deadline),
            None => self.recv_ref().map(Some),
        }
    }

    /// Like [`Receiver::recv`], but will not block longer than until `deadline`. Returns:
    ///  * `Ok(Some(value))` if there is a value in the channel before `instant`.
    ///  * `Ok(None)` if there is no value in the channel before `instant`.
    ///  * `Err` if the sender was dropped before sending anything or if the value has already
    ///    been extracted by previous receive calls.
    pub fn recv_deadline(&self, deadline: Instant) -> Result<Option<T>, DroppedSenderError> {
        let state_ptr = self.state_ptr;

        let state = unsafe { &*state_ptr }.load(Ordering::SeqCst);
        if state == states::init() {
            // The sender is alive but has not sent anything yet. We prepare to park.

            // Conditionally add a delay here to help the tests trigger the edge cases where
            // the sender manages to be dropped or send something before we are able to store our
            // `Thread` object in the state.
            #[cfg(feature = "test-delay")]
            std::thread::sleep(std::time::Duration::from_millis(10));

            // Allocate and put our thread instance on the heap and then store it in the state.
            // The sender will use this to unpark us when it sends or is dropped.
            // The actor taking this object out of the state is responsible for freeing it.
            let thread_ptr = Box::into_raw(Box::new(thread::current()));
            let state = unsafe { &*state_ptr }.compare_and_swap(
                states::init(),
                thread_ptr as usize,
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
                        assert_ne!(state, states::init());
                        if state == thread_ptr as usize {
                            // The sender has not touched the state. We took out the thread object.
                            unsafe { Box::from_raw(thread_ptr) };
                            break Ok(None);
                        } else if state == states::closed() {
                            // The sender was dropped while we were parked.
                            unsafe { &*state_ptr }.store(states::closed(), Ordering::SeqCst);
                            break Err(DroppedSenderError(()));
                        } else {
                            // The sender sent data while we were parked.
                            // We take the value and treat the channel as closed.
                            unsafe { &*state_ptr }.store(states::closed(), Ordering::SeqCst);
                            break Ok(Some(unsafe { *Box::from_raw(state as *mut T) }));
                        }
                    }
                    // Check if the sender updated the state
                    let state = unsafe { &*state_ptr }.load(Ordering::SeqCst);
                    if state == states::closed() {
                        // The sender was dropped while we were parked.
                        break Err(DroppedSenderError(()));
                    } else if state != thread_ptr as usize {
                        // The sender sent data while we were parked.
                        // We take the value and treat the channel as closed.
                        unsafe { &*state_ptr }.store(states::closed(), Ordering::SeqCst);
                        break Ok(Some(unsafe { *Box::from_raw(state as *mut T) }));
                    }
                }
            } else if state == states::closed() {
                // The sender was dropped before sending anything while we prepared to park.
                unsafe { Box::from_raw(thread_ptr) };
                Err(DroppedSenderError(()))
            } else {
                // The sender sent data while we prepared to park.
                // We take the value and treat the channel as closed.
                unsafe { Box::from_raw(thread_ptr) };
                unsafe { &*state_ptr }.store(states::closed(), Ordering::SeqCst);
                Ok(Some(unsafe { *Box::from_raw(state as *mut T) }))
            }
        } else if state == states::closed() {
            // The sender was dropped before sending anything, or we already took the value.
            Err(DroppedSenderError(()))
        } else {
            // The sender already sent a value. We take the value and treat the channel as closed.
            unsafe { &*state_ptr }.store(states::closed(), Ordering::SeqCst);
            Ok(Some(unsafe { *Box::from_raw(state as *mut T) }))
        }
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        let state = unsafe { &*self.state_ptr }.swap(states::closed(), Ordering::SeqCst);
        if state == states::init() {
            // The sender has not sent anything, nor is it dropped. The sender is responsible for
            // freeing the state
        } else if state == states::closed() {
            // The sender was already dropped. We are responsible for freeing the state
            unsafe { Box::from_raw(self.state_ptr) };
        } else {
            // The sender already sent something. We must free it, and our state
            unsafe { Box::from_raw(self.state_ptr) };
            unsafe { Box::from_raw(state as *mut T) };
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct DroppedSenderError(());

impl fmt::Display for DroppedSenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        "Oneshot sender dropped without sending anything or value already received".fmt(f)
    }
}

impl std::error::Error for DroppedSenderError {}

pub struct DroppedReceiverError<T>(pub Box<T>);

impl<T: Eq> Eq for DroppedReceiverError<T> {}
impl<T: PartialEq> PartialEq for DroppedReceiverError<T> {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl<T> DroppedReceiverError<T> {
    #[inline]
    pub fn into_value(self) -> T {
        *self.0
    }
}

impl<T> fmt::Display for DroppedReceiverError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        "Oneshot receiver has already been dropped".fmt(f)
    }
}

impl<T> fmt::Debug for DroppedReceiverError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DroppedReceiverError<{}>(_)", stringify!(T))
    }
}

impl<T> std::error::Error for DroppedReceiverError<T> {}

mod states {
    static INIT: u8 = 1u8;
    static CLOSED: u8 = 2u8;

    /// Returns a memory address in integer form representing the initial state of a channel.
    /// This state is active while both the sender and receiver are still alive, no value
    /// has yet been sent and the receiver has not started receiving.
    ///
    /// The value is guaranteed to:
    /// * be the same for every call in the same process
    /// * be different from what `states::dropped` returns
    /// * and never equal a pointer returned from `Box::into_raw`.
    #[inline(always)]
    pub fn init() -> usize {
        &INIT as *const u8 as usize
    }

    /// Returns a memory address in integer form representing a closed channel.
    /// A channel is closed when one end has been dropped or a sent value has been received.
    ///
    /// The value is guaranteed to:
    /// * be the same for every call in the same process
    /// * be different from what `states::init` returns
    /// * and never equal a pointer returned from `Box::into_raw`.
    #[inline(always)]
    pub fn closed() -> usize {
        &CLOSED as *const u8 as usize
    }
}

#[cfg(test)]
mod tests {
    use std::{
        mem,
        time::{Duration, Instant},
    };

    mod thread {
        pub use std::thread::sleep;

        #[cfg(loom)]
        pub use loom::thread::spawn;
        #[cfg(not(loom))]
        pub use std::thread::spawn;
    }

    fn maybe_loom_model(test: impl Fn() + Sync + Send + 'static) {
        #[cfg(loom)]
        loom::model(test);
        #[cfg(not(loom))]
        test();
    }

    #[test]
    fn send_before_recv_ref() {
        maybe_loom_model(|| {
            let (sender, receiver) = crate::channel();
            assert!(sender.send(19i128).is_ok());

            assert_eq!(receiver.recv_ref(), Ok(19i128));
            assert_eq!(receiver.recv_ref(), Err(crate::DroppedSenderError(())));
            assert_eq!(receiver.try_recv(), Err(crate::DroppedSenderError(())));
            assert!(receiver.recv_timeout(Duration::from_secs(1)).is_err());
        })
    }

    #[test]
    fn send_before_recv() {
        maybe_loom_model(|| {
            let (sender, receiver) = crate::channel();
            assert!(sender.send(19i128).is_ok());
            assert_eq!(receiver.recv(), Ok(19i128));
        })
    }

    #[test]
    fn send_before_try_recv() {
        maybe_loom_model(|| {
            let (sender, receiver) = crate::channel();
            assert!(sender.send(19i128).is_ok());

            assert_eq!(receiver.try_recv(), Ok(Some(19i128)));
            assert_eq!(receiver.try_recv(), Err(crate::DroppedSenderError(())));
            assert_eq!(receiver.recv_ref(), Err(crate::DroppedSenderError(())));
            assert!(receiver.recv_timeout(Duration::from_secs(1)).is_err());
        })
    }

    #[test]
    fn send_before_recv_timeout() {
        maybe_loom_model(|| {
            let (sender, receiver) = crate::channel();
            assert!(sender.send(19i128).is_ok());

            let start = Instant::now();
            let timeout = Duration::from_secs(1);
            assert_eq!(receiver.recv_timeout(timeout), Ok(Some(19i128)));
            assert!(start.elapsed() < Duration::from_millis(100));

            assert!(receiver.recv_timeout(timeout).is_err());
            assert!(receiver.try_recv().is_err());
            assert!(receiver.recv().is_err());
        })
    }

    #[test]
    fn send_then_drop_receiver() {
        maybe_loom_model(|| {
            let (sender, receiver) = crate::channel();
            assert!(sender.send(19i128).is_ok());
            mem::drop(receiver);
        })
    }

    #[test]
    fn send_with_dropped_receiver() {
        maybe_loom_model(|| {
            let (sender, receiver) = crate::channel();
            mem::drop(receiver);
            let send_error = sender.send(5u128).unwrap_err();
            assert_eq!(send_error, crate::DroppedReceiverError(Box::new(5)));
            assert_eq!(send_error.into_value(), 5);
        })
    }

    #[test]
    fn recv_with_dropped_sender() {
        maybe_loom_model(|| {
            let (sender, receiver) = crate::channel::<u128>();
            mem::drop(sender);
            receiver.recv().unwrap_err();
        })
    }

    #[test]
    fn try_recv_with_dropped_sender() {
        maybe_loom_model(|| {
            let (sender, receiver) = crate::channel::<u128>();
            mem::drop(sender);
            receiver.try_recv().unwrap_err();
        })
    }

    #[test]
    fn recv_before_send() {
        maybe_loom_model(|| {
            let (sender, receiver) = crate::channel();
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(2));
                sender.send(9u128).unwrap();
            });
            assert_eq!(receiver.recv(), Ok(9));
        })
    }

    #[test]
    fn recv_timeout_before_send() {
        maybe_loom_model(|| {
            let (sender, receiver) = crate::channel();
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(2));
                sender.send(9u128).unwrap();
            });
            assert_eq!(receiver.recv_timeout(Duration::from_secs(1)), Ok(Some(9)));
        })
    }

    #[test]
    fn recv_before_send_then_drop_sender() {
        maybe_loom_model(|| {
            let (sender, receiver) = crate::channel::<u128>();
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(10));
                mem::drop(sender);
            });
            assert!(receiver.recv().is_err());
        })
    }

    #[test]
    fn recv_timeout_before_send_then_drop_sender() {
        maybe_loom_model(|| {
            let (sender, receiver) = crate::channel::<u128>();
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(10));
                mem::drop(sender);
            });
            assert!(receiver.recv_timeout(Duration::from_secs(1)).is_err());
        })
    }

    #[test]
    fn try_recv() {
        maybe_loom_model(|| {
            let (sender, receiver) = crate::channel::<u128>();
            assert_eq!(receiver.try_recv(), Ok(None));
            mem::drop(sender)
        })
    }

    #[test]
    fn recv_deadline_and_timeout_no_time() {
        maybe_loom_model(|| {
            let (sender, receiver) = crate::channel::<u128>();

            let start = Instant::now();
            assert_eq!(receiver.recv_deadline(start), Ok(None));
            assert_eq!(receiver.recv_timeout(Duration::from_millis(0)), Ok(None));
            assert!(start.elapsed() < Duration::from_millis(100));

            mem::drop(sender)
        })
    }

    #[test]
    fn recv_deadline_and_timeout_time_should_elapse() {
        maybe_loom_model(|| {
            let (sender, receiver) = crate::channel::<u128>();

            let start = Instant::now();
            let timeout = Duration::from_millis(100);
            assert_eq!(receiver.recv_deadline(start + timeout), Ok(None));
            assert!(start.elapsed() > timeout);
            assert!(start.elapsed() < timeout * 2);

            let start = Instant::now();
            assert_eq!(receiver.recv_timeout(timeout), Ok(None));
            assert!(start.elapsed() > timeout);
            assert!(start.elapsed() < timeout * 2);

            mem::drop(sender)
        })
    }
}
