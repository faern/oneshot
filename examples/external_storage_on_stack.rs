#[cfg(feature = "std")]
fn main() {
    use core::ptr::NonNull;

    use oneshot::ChannelStorage;

    let storage = ChannelStorage::<usize>::new();

    // SAFETY: We promise that no other channel is using this storage and that it stays alive
    // at least as long as the channel itself - guaranteed because it is a local variable and
    // the channel endpoints are local variables with lesser scope.
    let (sender, receiver) =
        unsafe { oneshot::channel_with_storage(NonNull::from(&storage), |_| {}) };

    sender.send(1234).unwrap();
    assert_eq!(receiver.recv().unwrap(), 1234);
}

#[cfg(not(feature = "std"))]
fn main() {
    panic!("This example is only for when the \"std\" feature is used");
}
