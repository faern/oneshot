#![cfg(all(feature = "async", not(oneshot_loom)))]

use core::mem;
use core::time::Duration;

mod helpers;
use helpers::DropCounter;

#[tokio::test]
async fn send_before_await_tokio() {
    let (sender, receiver) = oneshot::channel();
    assert!(sender.send(19i128).is_ok());
    assert_eq!(receiver.await, Ok(19i128));
}

#[async_std::test]
async fn send_before_await_async_std() {
    let (sender, receiver) = oneshot::channel();
    assert!(sender.send(19i128).is_ok());
    assert_eq!(receiver.await, Ok(19i128));
}

#[tokio::test]
async fn await_with_dropped_sender_tokio() {
    let (sender, receiver) = oneshot::channel::<u128>();
    mem::drop(sender);
    receiver.await.unwrap_err();
}

#[async_std::test]
async fn await_with_dropped_sender_async_std() {
    let (sender, receiver) = oneshot::channel::<u128>();
    mem::drop(sender);
    receiver.await.unwrap_err();
}

#[tokio::test]
async fn await_before_send_tokio() {
    let (sender, receiver) = oneshot::channel();
    let (message, counter) = DropCounter::new(79u128);
    let t = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        sender.send(message)
    });
    let returned_message = receiver.await.unwrap();
    assert_eq!(counter.count(), 0);
    assert_eq!(*returned_message.value(), 79u128);
    mem::drop(returned_message);
    assert_eq!(counter.count(), 1);
    t.await.unwrap().unwrap();
}

#[async_std::test]
async fn await_before_send_async_std() {
    let (sender, receiver) = oneshot::channel();
    let (message, counter) = DropCounter::new(79u128);
    let t = async_std::task::spawn(async move {
        async_std::task::sleep(Duration::from_millis(10)).await;
        sender.send(message)
    });
    let returned_message = receiver.await.unwrap();
    assert_eq!(counter.count(), 0);
    assert_eq!(*returned_message.value(), 79u128);
    mem::drop(returned_message);
    assert_eq!(counter.count(), 1);
    t.await.unwrap();
}

#[tokio::test]
async fn await_before_send_then_drop_sender_tokio() {
    let (sender, receiver) = oneshot::channel::<u128>();
    let t = tokio::spawn(async {
        tokio::time::sleep(Duration::from_millis(10)).await;
        mem::drop(sender);
    });
    assert!(receiver.await.is_err());
    t.await.unwrap();
}

#[async_std::test]
async fn await_before_send_then_drop_sender_async_std() {
    let (sender, receiver) = oneshot::channel::<u128>();
    let t = async_std::task::spawn(async {
        async_std::task::sleep(Duration::from_millis(10)).await;
        mem::drop(sender);
    });
    assert!(receiver.await.is_err());
    t.await;
}

#[tokio::test]
async fn poll_receiver_then_drop_it() {
    let (sender, receiver) = oneshot::async_channel::<()>();
    // This will poll the receiver and then give up after 100 ms.
    tokio::time::timeout(Duration::from_millis(100), receiver)
        .await
        .unwrap_err();
    // Make sure the receiver has been dropped by the runtime.
    assert!(sender.send(()).is_err());
}
