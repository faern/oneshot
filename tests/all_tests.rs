#[cfg(loom)]
use loom::sync::{
    atomic::{AtomicUsize, Ordering::SeqCst},
    Arc,
};
use oneshot::{RecvError, RecvTimeoutError, TryRecvError};
#[cfg(not(loom))]
use std::sync::{
    atomic::{AtomicUsize, Ordering::SeqCst},
    Arc,
};
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
        let (sender, receiver) = oneshot::channel();
        assert!(sender.send(19i128).is_ok());

        assert_eq!(receiver.recv_ref(), Ok(19i128));
        assert_eq!(receiver.recv_ref(), Err(RecvError));
        assert_eq!(receiver.try_recv(), Err(TryRecvError::Disconnected));
        assert!(receiver.recv_timeout(Duration::from_secs(1)).is_err());
    })
}

#[test]
fn send_before_recv() {
    maybe_loom_model(|| {
        let (sender, receiver) = oneshot::channel::<()>();
        assert!(sender.send(()).is_ok());
        assert_eq!(receiver.recv(), Ok(()));
    });
    maybe_loom_model(|| {
        let (sender, receiver) = oneshot::channel::<u8>();
        assert!(sender.send(19).is_ok());
        assert_eq!(receiver.recv(), Ok(19));
    });
    maybe_loom_model(|| {
        let (sender, receiver) = oneshot::channel::<u64>();
        assert!(sender.send(21).is_ok());
        assert_eq!(receiver.recv(), Ok(21));
    });
    // FIXME: This test does not work with loom. There is something that happens after the
    // channel object becomes larger than ~500 bytes and that makes an atomic read from the state
    // result in "signal: 10, SIGBUS: access to undefined memory"
    #[cfg(not(loom))]
    maybe_loom_model(|| {
        let (sender, receiver) = oneshot::channel::<[u8; 4096]>();
        assert!(sender.send([0b10101010; 4096]).is_ok());
        assert!(receiver.recv().unwrap()[..] == [0b10101010; 4096][..]);
    });
}

#[test]
fn send_before_try_recv() {
    maybe_loom_model(|| {
        let (sender, receiver) = oneshot::channel();
        assert!(sender.send(19i128).is_ok());

        assert_eq!(receiver.try_recv(), Ok(19i128));
        assert_eq!(receiver.try_recv(), Err(TryRecvError::Disconnected));
        assert_eq!(receiver.recv_ref(), Err(RecvError));
        assert!(receiver.recv_timeout(Duration::from_secs(1)).is_err());
    })
}

#[test]
fn send_before_recv_timeout() {
    maybe_loom_model(|| {
        let (sender, receiver) = oneshot::channel();
        assert!(sender.send(19i128).is_ok());

        let start = Instant::now();
        let timeout = Duration::from_secs(1);
        assert_eq!(receiver.recv_timeout(timeout), Ok(19i128));
        assert!(start.elapsed() < Duration::from_millis(100));

        assert!(receiver.recv_timeout(timeout).is_err());
        assert!(receiver.try_recv().is_err());
        assert!(receiver.recv().is_err());
    })
}

#[cfg(not(loom))]
#[tokio::test]
async fn send_before_await() {
    let (sender, receiver) = oneshot::channel();
    assert!(sender.send(19i128).is_ok());
    assert_eq!(receiver.await, Ok(19i128));
}

#[test]
fn send_then_drop_receiver() {
    maybe_loom_model(|| {
        let (sender, receiver) = oneshot::channel();
        assert!(sender.send(19i128).is_ok());
        mem::drop(receiver);
    })
}

#[test]
fn send_with_dropped_receiver() {
    maybe_loom_model(|| {
        let (sender, receiver) = oneshot::channel();
        mem::drop(receiver);
        let send_error = sender.send(5u128).unwrap_err();
        assert_eq!(*send_error.as_inner(), 5);
        assert_eq!(send_error.into_inner(), 5);
    })
}

#[test]
fn recv_with_dropped_sender() {
    maybe_loom_model(|| {
        let (sender, receiver) = oneshot::channel::<u128>();
        mem::drop(sender);
        receiver.recv().unwrap_err();
    })
}

#[test]
fn try_recv_with_dropped_sender() {
    maybe_loom_model(|| {
        let (sender, receiver) = oneshot::channel::<u128>();
        mem::drop(sender);
        receiver.try_recv().unwrap_err();
    })
}

#[cfg(not(loom))]
#[tokio::test]
async fn await_with_dropped_sender() {
    let (sender, receiver) = oneshot::channel::<u128>();
    mem::drop(sender);
    receiver.await.unwrap_err();
}

#[test]
fn recv_before_send() {
    maybe_loom_model(|| {
        let (sender, receiver) = oneshot::channel();
        let t = thread::spawn(move || {
            thread::sleep(Duration::from_millis(2));
            sender.send(9u128).unwrap();
        });
        assert_eq!(receiver.recv(), Ok(9));
        t.join().unwrap();
    })
}

#[test]
fn recv_timeout_before_send() {
    maybe_loom_model(|| {
        let (sender, receiver) = oneshot::channel();
        let t = thread::spawn(move || {
            thread::sleep(Duration::from_millis(2));
            sender.send(9u128).unwrap();
        });
        assert_eq!(receiver.recv_timeout(Duration::from_secs(1)), Ok(9));
        t.join().unwrap();
    })
}

#[cfg(not(loom))]
#[tokio::test]
async fn await_before_send() {
    let (sender, receiver) = oneshot::channel();
    let t = tokio::spawn(async move {
        tokio::time::delay_for(Duration::from_millis(2)).await;
        sender.send(9u128)
    });
    assert_eq!(receiver.await, Ok(9));
    t.await.unwrap().unwrap();
}

#[test]
fn recv_before_send_then_drop_sender() {
    maybe_loom_model(|| {
        let (sender, receiver) = oneshot::channel::<u128>();
        let t = thread::spawn(move || {
            thread::sleep(Duration::from_millis(10));
            mem::drop(sender);
        });
        assert!(receiver.recv().is_err());
        t.join().unwrap();
    })
}

#[test]
fn recv_timeout_before_send_then_drop_sender() {
    maybe_loom_model(|| {
        let (sender, receiver) = oneshot::channel::<u128>();
        let t = thread::spawn(move || {
            thread::sleep(Duration::from_millis(10));
            mem::drop(sender);
        });
        assert!(receiver.recv_timeout(Duration::from_secs(1)).is_err());
        t.join().unwrap();
    })
}

#[cfg(not(loom))]
#[tokio::test]
async fn await_before_send_then_drop_sender() {
    let (sender, receiver) = oneshot::channel::<u128>();
    let t = tokio::spawn(async {
        tokio::time::delay_for(Duration::from_millis(10)).await;
        mem::drop(sender);
    });
    assert!(receiver.await.is_err());
    t.await.unwrap();
}

// Tests that the Receiver handles being used synchronously even after being polled
#[cfg(not(loom))]
#[tokio::test]
async fn poll_future_and_then_try_recv() {
    use core::future::Future;
    use core::pin::Pin;
    use core::task::{self, Poll};

    struct StupidReceiverFuture(oneshot::Receiver<()>);

    impl Future for StupidReceiverFuture {
        type Output = Result<(), oneshot::RecvError>;

        fn poll(mut self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<Self::Output> {
            let poll_result = Future::poll(Pin::new(&mut self.0), cx);
            self.0.try_recv().expect_err("Should never be a message");
            poll_result
        }
    }

    let (sender, receiver) = oneshot::channel();
    let t = tokio::spawn(async {
        tokio::time::delay_for(Duration::from_millis(20)).await;
        mem::drop(sender);
    });
    StupidReceiverFuture(receiver).await.unwrap_err();
    t.await.unwrap();
}

#[test]
fn try_recv() {
    maybe_loom_model(|| {
        let (sender, receiver) = oneshot::channel::<u128>();
        assert_eq!(receiver.try_recv(), Err(TryRecvError::Empty));
        mem::drop(sender)
    })
}

#[test]
fn recv_deadline_and_timeout_no_time() {
    maybe_loom_model(|| {
        let (_sender, receiver) = oneshot::channel::<u128>();

        let start = Instant::now();
        assert_eq!(
            receiver.recv_deadline(start),
            Err(RecvTimeoutError::Timeout)
        );
        assert!(start.elapsed() < Duration::from_millis(200));

        let start = Instant::now();
        assert_eq!(
            receiver.recv_timeout(Duration::from_millis(0)),
            Err(RecvTimeoutError::Timeout)
        );
        assert!(start.elapsed() < Duration::from_millis(200));
    })
}

#[test]
fn recv_deadline_and_timeout_time_should_elapse() {
    maybe_loom_model(|| {
        let (_sender, receiver) = oneshot::channel::<u128>();

        let start = Instant::now();
        let timeout = Duration::from_millis(100);
        assert_eq!(
            receiver.recv_deadline(start + timeout),
            Err(RecvTimeoutError::Timeout)
        );
        assert!(start.elapsed() > timeout);
        assert!(start.elapsed() < timeout * 3);

        let start = Instant::now();
        assert_eq!(
            receiver.recv_timeout(timeout),
            Err(RecvTimeoutError::Timeout)
        );
        assert!(start.elapsed() > timeout);
        assert!(start.elapsed() < timeout * 3);
    })
}

#[cfg(not(loom))]
#[test]
fn non_send_type_can_be_used_on_same_thread() {
    use std::ptr;

    #[derive(Debug, Eq, PartialEq)]
    struct NotSend(*mut ());

    let (sender, receiver) = oneshot::channel();
    sender.send(NotSend(ptr::null_mut())).unwrap();
    let reply = receiver.recv().unwrap();
    assert_eq!(reply, NotSend(ptr::null_mut()));
}

struct DropCounter(Arc<AtomicUsize>);

impl DropCounter {
    pub fn new() -> (Self, Arc<AtomicUsize>) {
        let counter = Arc::new(AtomicUsize::new(0));
        (Self(counter.clone()), counter)
    }
}

impl Drop for DropCounter {
    fn drop(&mut self) {
        self.0.fetch_add(1, SeqCst);
    }
}

#[test]
fn message_in_channel_dropped_on_receiver_drop() {
    maybe_loom_model(|| {
        let (sender, receiver) = oneshot::channel();
        let (message, drop_count) = DropCounter::new();
        assert_eq!(drop_count.load(SeqCst), 0);
        sender.send(message).unwrap();
        assert_eq!(drop_count.load(SeqCst), 0);
        mem::drop(receiver);
        assert_eq!(drop_count.load(SeqCst), 1);
    })
}

#[test]
fn send_error_drops_message_correctly() {
    maybe_loom_model(|| {
        let (sender, _) = oneshot::channel();
        let (message, drop_count) = DropCounter::new();

        let send_error = sender.send(message).unwrap_err();
        assert_eq!(drop_count.load(SeqCst), 0);
        mem::drop(send_error);
        assert_eq!(drop_count.load(SeqCst), 1);
    });
}

#[test]
fn send_error_drops_message_correctly_on_into_inner() {
    maybe_loom_model(|| {
        let (sender, _) = oneshot::channel();
        let (message, drop_count) = DropCounter::new();

        let send_error = sender.send(message).unwrap_err();
        assert_eq!(drop_count.load(SeqCst), 0);
        let message = send_error.into_inner();
        assert_eq!(drop_count.load(SeqCst), 0);
        mem::drop(message);
        assert_eq!(drop_count.load(SeqCst), 1);
    });
}
