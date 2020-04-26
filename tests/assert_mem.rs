use std::mem;

/// Just sanity check that both channel endpoints stay the size of a single pointer.
#[test]
fn channel_endpoints_single_pointer() {
    const EXPECTED: usize = mem::size_of::<usize>();
    assert_eq!(mem::size_of::<oneshot::Sender<()>>(), EXPECTED);
    assert_eq!(mem::size_of::<oneshot::Receiver<()>>(), EXPECTED);

    assert_eq!(mem::size_of::<oneshot::Sender<u8>>(), EXPECTED);
    assert_eq!(mem::size_of::<oneshot::Receiver<u8>>(), EXPECTED);

    assert_eq!(mem::size_of::<oneshot::Sender<[u8; 1024]>>(), EXPECTED);
    assert_eq!(mem::size_of::<oneshot::Receiver<[u8; 1024]>>(), EXPECTED);
}
