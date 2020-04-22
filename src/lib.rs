//! Oneshot spsc channel working both in and between sync and async environments.

#![deny(rust_2018_idioms)]

use core::fmt;
use core::mem;
use core::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

pub fn sync_channel<T>() -> (SyncSender<T>, SyncReceiver<T>) {
    // Allocate the state on the heap and initialize it with `new_state()` and get the pointer.
    // The last endpoint of the channel to be alive is responsible for freeing the state.
    let state = Box::into_raw(Box::new(AtomicUsize::new(new_state())));
    (
        SyncSender {
            state,
            _marker: std::marker::PhantomData,
        },
        SyncReceiver {
            state,
            _marker: std::marker::PhantomData,
        },
    )
}

#[derive(Debug, Eq, PartialEq, Hash)]
pub struct SyncSender<T> {
    state: *mut AtomicUsize,
    _marker: std::marker::PhantomData<T>,
}

#[derive(Debug, Eq, PartialEq, Hash)]
pub struct SyncReceiver<T> {
    state: *mut AtomicUsize,
    _marker: std::marker::PhantomData<T>,
}

unsafe impl<T: Send> Send for SyncSender<T> {}
unsafe impl<T: Send> Send for SyncReceiver<T> {}

impl<T> SyncSender<T> {
    /// Sends `value` over the channel to the [`Receiver`].
    /// Returns an error if the receiver was dropped before the send took place. The value can
    /// be extracted from the error again.
    pub fn send(self, value: T) -> Result<(), DroppedReceiverError<T>> {
        let state_ptr = self.state;
        // Don't run our Drop implementation if send was called, any cleanup now happens here
        mem::forget(self);

        // Put the value on the heap and get the pointer. If sending succeeds the receiver is
        // responsible for freeing it, otherwise we do that
        let value_ptr = Box::into_raw(Box::new(value));

        // Store the address to the value in the state and read out what state the receiver is in
        let state = unsafe { &*state_ptr }.swap(value_ptr as usize, Ordering::SeqCst);
        if state == new_state() {
            // The receiver is alive and has not started waiting. Send done
            // Receiver frees state and value from heap
            Ok(())
        } else if state == dropped_state() {
            // The receiver was already dropped. We are responsible for freeing the state and value
            unsafe { Box::from_raw(state_ptr) };
            Err(DroppedReceiverError(unsafe { Box::from_raw(value_ptr) }))
        } else {
            // The receiver is waiting. Wake it up so it can return the value. The receiver frees
            // the state, the value and the thread instance in the state
            unsafe { &*(state as *const thread::Thread) }.unpark();
            Ok(())
        }
    }
}

impl<T> Drop for SyncSender<T> {
    fn drop(&mut self) {
        let state = unsafe { &*self.state }.swap(dropped_state(), Ordering::SeqCst);
        if state == new_state() {
            // The receiver has not started waiting, nor is it dropped. Nothing to do
        } else if state == dropped_state() {
            // The receiver was already dropped. We are responsible for freeing the state
            unsafe { Box::from_raw(self.state) };
        } else {
            // The receiver started waiting. Wake it up so it can detect it has been cancelled
            unsafe { &*(state as *const thread::Thread) }.unpark();
        }
    }
}

impl<T> SyncReceiver<T> {
    pub fn recv(self) -> Result<T, DroppedSenderError> {
        let state_ptr = self.state;
        // Don't run our Drop implementation if we are receiving
        mem::forget(self);

        let state = unsafe { &*state_ptr }.load(Ordering::SeqCst);
        if state == new_state() {
            // The sender has not sent anything, nor is it dropped
            // Put our thread object on the heap. We are always responsible for freeing it
            let thread_ptr = Box::into_raw(Box::new(thread::current()));
            let state = unsafe { &*state_ptr }.compare_and_swap(
                new_state(),
                thread_ptr as usize,
                Ordering::SeqCst,
            );
            if state == new_state() {
                // We stored our thread, now we park
                loop {
                    thread::park();
                    // Check if the sender replaced our thread instance with the value pointer
                    let value_ptr = unsafe { &*state_ptr }.load(Ordering::SeqCst);
                    if value_ptr != thread_ptr as usize {
                        unsafe { Box::from_raw(thread_ptr) };
                        unsafe { Box::from_raw(state_ptr) };
                        return Ok(unsafe { *Box::from_raw(value_ptr as *mut T) });
                    }
                }
            } else if state == dropped_state() {
                // The sender was dropped while we prepared to park
                unsafe { Box::from_raw(thread_ptr) };
                unsafe { Box::from_raw(state_ptr) };
                Err(DroppedSenderError(()))
            } else {
                // The sender sent data while we prepared to park. We free everything
                unsafe { Box::from_raw(thread_ptr) };
                unsafe { Box::from_raw(state_ptr) };
                Ok(unsafe { *Box::from_raw(state as *mut T) })
            }
        } else if state == dropped_state() {
            // The sender was already dropped
            unsafe { Box::from_raw(state_ptr) };
            Err(DroppedSenderError(()))
        } else {
            // The sender already sent data. We free the state and the value
            unsafe { Box::from_raw(state_ptr) };
            Ok(unsafe { *Box::from_raw(state as *mut T) })
        }
    }
}

impl<T> Drop for SyncReceiver<T> {
    fn drop(&mut self) {
        let state = unsafe { &*self.state }.swap(dropped_state(), Ordering::SeqCst);
        if state == new_state() {
            // The sender has not sent anything, nor is it dropped
        } else if state == dropped_state() {
            // The sender was already dropped. We are responsible for freeing the state
            unsafe { Box::from_raw(self.state) };
        } else {
            // The sender already sent something. We must free it, and our state
            unsafe { Box::from_raw(self.state) };
            unsafe { Box::from_raw(state as *mut T) };
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct DroppedSenderError(());

impl fmt::Display for DroppedSenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        "Oneshot sender dropped without sending anything".fmt(f)
    }
}

impl std::error::Error for DroppedSenderError {}

pub struct DroppedReceiverError<T>(pub Box<T>);

impl<T> DroppedReceiverError<T> {
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

/// Returns a memory address in integer form. The value is guaranteed to:
/// * be the same for every call in the same process
/// * be different from what `dropped_state` returns
/// * and never equal a pointer returned from `Box::into_raw`.
#[inline(always)]
fn new_state() -> usize {
    static NEW: u8 = 1u8;
    &NEW as *const u8 as usize
}

/// Returns a memory address in integer form. The value is guaranteed to:
/// * be the same for every call in the same process
/// * be different from what `new_state` returns
/// * and never equal a pointer returned from `Box::into_raw`.
#[inline(always)]
fn dropped_state() -> usize {
    static DROPPED: u8 = 2u8;
    &DROPPED as *const u8 as usize
}

#[cfg(test)]
mod tests {
    use std::{mem, thread, time::Duration};

    #[test]
    fn send_with_dropped_receiver() {
        let (sender, receiver) = crate::sync_channel();
        mem::drop(receiver);
        assert_eq!(sender.send(5usize), Err(5));
    }

    #[test]
    fn recv_with_dropped_sender() {
        let (sender, receiver) = crate::sync_channel::<u128>();
        mem::drop(sender);
        receiver.recv().unwrap_err();
    }

    #[test]
    fn send_before_recv() {
        let (sender, receiver) = crate::sync_channel();
        assert!(sender.send(19i128).is_ok());
        assert_eq!(receiver.recv(), Ok(19i128));
    }

    #[test]
    fn recv_before_send() {
        let (sender, receiver) = crate::sync_channel();
        thread::spawn(move || {
            thread::sleep(Duration::from_secs(1));
            sender.send(9u128).unwrap();
        });
        assert_eq!(receiver.recv(), Ok(9));
    }

    #[test]
    fn send_then_drop_receiver() {
        let (sender, receiver) = crate::sync_channel();
        assert!(sender.send(19i128).is_ok());
        mem::drop(receiver);
    }
}
