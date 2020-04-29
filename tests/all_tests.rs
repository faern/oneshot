use oneshot::{RecvError, RecvTimeoutError, TryRecvError};
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
        let (sender, receiver) = oneshot::channel();
        assert!(sender.send(19i128).is_ok());
        assert_eq!(receiver.recv(), Ok(19i128));
    })
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
