//! Oneshot spsc channel working both in and between sync and async environments.

#![deny(rust_2018_idioms)]

use std::sync::{
    atomic::{AtomicUsize, Ordering},
};
use std::mem;
use std::thread;

pub fn sync_channel<T>() -> (SyncSender<T>, SyncReceiver<T>) {
    let state = Box::into_raw(Box::new(AtomicUsize::new(0)));
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

pub struct SyncSender<T> {
    state: *mut AtomicUsize,
    _marker: std::marker::PhantomData<T>,
}


pub struct SyncReceiver<T> {
    state: *mut AtomicUsize,
    _marker: std::marker::PhantomData<T>,
}

unsafe impl<T: Send> Send for SyncSender<T> {}
unsafe impl<T: Send> Send for SyncReceiver<T> {}

impl<T> SyncSender<T> {
    pub fn send(self, value: T) -> Result<(), T> {
        let state = self.state;
        // Don't run our Drop implementation if we have sent something
        mem::forget(self);

        let data_ptr = Box::into_raw(Box::new(value));
        match unsafe { &*state }.swap(data_ptr as usize, Ordering::SeqCst) {
            // The receiver has not started waiting, nor is it dropped
            0 => Ok(()),
            // The receiver was already dropped. We are responsible for deallocating state
            1 => {
                unsafe { Box::from_raw(state); };
                Err(unsafe { *Box::from_raw(data_ptr) })
            },
            // The receiver has started waiting. Wake it up
            thread_ptr => {
                let thread = unsafe { Box::from_raw(thread_ptr as *mut thread::Thread) };
                thread.unpark();
                Ok(())
            }
        }
    }
}

impl<T> Drop for SyncSender<T> {
    fn drop(&mut self) {
        match unsafe { &*self.state }.swap(1, Ordering::SeqCst) {
            // The receiver has not started waiting, nor is it dropped
            0 => (),
            // The receiver was already dropped. We are responsible for deallocating state
            1 => unsafe { Box::from_raw(self.state); },
            // The receiver started waiting. Wake it up so it can detect it has been cancelled
            thread_ptr => {
                let thread = unsafe { Box::from_raw(thread_ptr as *mut thread::Thread) };
                thread.unpark();
            }
        }
    }
}

impl<T> SyncReceiver<T> {
    pub fn recv(self) -> Result<T, Cancelled> {
        let state = self.state;
        // Don't run our Drop implementation if we are receiving
        mem::forget(self);

        match unsafe { &*state }.load(Ordering::SeqCst) {
            // The sender has not sent anything, nor is it dropped
            0 => {
                let thread_ptr = Box::into_raw(Box::new(thread::current()));
                match unsafe { &*state }.compare_and_swap(0, thread_ptr as usize, Ordering::SeqCst) {
                    // We stored our thread, now we park
                    0 => loop {
                        thread::park();
                        // Check if the sender replaced our thread instance with the `T` pointer
                        let data_ptr = unsafe { &*state }.load(Ordering::SeqCst);
                        if data_ptr != thread_ptr as usize {
                            return Ok(unsafe { *Box::from_raw(data_ptr as *mut T) });
                        }
                    },
                    // The sender was dropped while we prepared to park
                    1 => {
                        unsafe { Box::from_raw(thread_ptr); };
                        unsafe { Box::from_raw(state); };
                        Err(Cancelled(()))
                    },
                    // The sender sent data while we prepared to park, just return it
                    data_ptr => {
                        unsafe { Box::from_raw(thread_ptr); };
                        unsafe { Box::from_raw(state); };
                        Ok(unsafe { *Box::from_raw(data_ptr as *mut T) })
                    }
                }
            }
            // The sender was already dropped
            1 => {
                unsafe { Box::from_raw(state); };
                Err(Cancelled(()))
            }
            // The sender already sent data
            data_ptr => {
                unsafe { Box::from_raw(state); };
                Ok(unsafe { *Box::from_raw(data_ptr as *mut T) })
            },
        }
    }
}

impl<T> Drop for SyncReceiver<T> {
    fn drop(&mut self) {
        match unsafe { &*self.state }.swap(1, Ordering::SeqCst) {
            // The sender has not sent anything, nor is it dropped
            0 => (),
            // The sender was already dropped. We are responsible for deallocating state
            1 => unsafe { Box::from_raw(self.state); },
            // The sender already sent something. We must deallocate it, and our state
            data_ptr => {
                unsafe { Box::from_raw(self.state); };
                unsafe { Box::from_raw(data_ptr as *mut T) };
            }
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct Cancelled(());

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
        let (sender, receiver) = crate::sync_channel::<()>();
        mem::drop(sender);
        assert!(receiver.recv().is_err());
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
            sender.send(3usize).unwrap();
        });
        assert_eq!(receiver.recv(), Ok(3));
    }
}
