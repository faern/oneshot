#[cfg(feature = "std")]
fn main() {
    use core::ptr::NonNull;

    use oneshot::ChannelStorage;

    struct Container {
        generation: usize,

        // NB! We are not allowed to create `&mut` exclusive references to this or the parent type
        // when the `ChannelStorage` is in use because that would violate the Rust aliasing rules
        // as the channel essentially takes a shared reference to the storage object.
        storage: ChannelStorage<usize>,
    }

    let mut container = Container {
        generation: 0,
        storage: ChannelStorage::new(),
    };

    // SAFETY: We promise that no other channel is using this storage and that it stays alive
    // at least as long as the channel itself - guaranteed because it is a local variable and
    // the channel endpoints are local variables with lesser scope.
    let (sender, receiver) =
        unsafe { oneshot::channel_with_storage(NonNull::from(&container.storage), |_| {}) };

    sender.send(1234).unwrap();
    assert_eq!(receiver.recv().unwrap(), 1234);

    // The storage is not in use anymore, so we can now mutate the container.
    container.generation += 1;

    // SAFETY: We promise that no other channel is using this storage and that it stays alive
    // at least as long as the channel itself - guaranteed because it is a local variable and
    // the channel endpoints are local variables with lesser scope.
    let (sender, receiver) =
        unsafe { oneshot::channel_with_storage(NonNull::from(&container.storage), |_| {}) };

    sender.send(5678).unwrap();
    assert_eq!(receiver.recv().unwrap(), 5678);
}

#[cfg(not(feature = "std"))]
fn main() {
    panic!("This example is only for when the \"std\" feature is used");
}
