#[cfg(feature = "std")]
fn main() {
    use core::ptr::NonNull;

    use oneshot::ChannelStorage;

    // This is functionally identical to just using `oneshot::channel()`.
    let storage = NonNull::from(Box::leak(Box::new(ChannelStorage::<usize>::new())));

    // SAFETY: We promise that no other channel is using this storage and that it stays alive
    // at least as long as the channel itself - guaranteed because it is a leaked box, so the
    // only thing that will destroy it is the release fn we provide here.
    let (sender, receiver) = unsafe {
        oneshot::channel_with_storage(storage, |storage| {
            drop(Box::from_raw(storage.as_ptr()));
        })
    };

    sender.send(1234).unwrap();
    assert_eq!(receiver.recv().unwrap(), 1234);
}

#[cfg(not(feature = "std"))]
fn main() {
    panic!("This example is only for when the \"std\" feature is used");
}
