use oneshot::{Receiver, Sender};
use std::mem;

/// Just sanity check that both channel endpoints stay the size of a single pointer.
#[test]
fn channel_endpoints_single_pointer() {
    const EXPECTED: usize = mem::size_of::<usize>();

    assert_eq!(mem::size_of::<Sender<()>>(), EXPECTED);
    assert_eq!(mem::size_of::<Receiver<()>>(), EXPECTED);

    assert_eq!(mem::size_of::<Sender<u8>>(), EXPECTED);
    assert_eq!(mem::size_of::<Receiver<u8>>(), EXPECTED);

    assert_eq!(mem::size_of::<Sender<[u8; 1024]>>(), EXPECTED);
    assert_eq!(mem::size_of::<Receiver<[u8; 1024]>>(), EXPECTED);

    // These tests would fail before switching to `NonNull`
    assert_eq!(mem::size_of::<Option<Sender<[u8; 1024]>>>(), EXPECTED);
    assert_eq!(mem::size_of::<Option<Receiver<[u8; 1024]>>>(), EXPECTED);
}

/// Check that the `SendError` stays small. Useful to automatically detect if it is refactored
/// to become large. We do not want the stack requirement for calling `Sender::send` to grow.
#[test]
fn error_sizes() {
    const EXPECTED: usize = mem::size_of::<usize>();

    assert_eq!(mem::size_of::<oneshot::SendError<()>>(), EXPECTED);
    assert_eq!(mem::size_of::<oneshot::SendError<u8>>(), EXPECTED);
    assert_eq!(mem::size_of::<oneshot::SendError<[u8; 1024]>>(), EXPECTED);

    // This test would fail before switching to `NonNull`
    // Note that if this test succeeds then that implies Option<SendError<..>> also has the niche
    // optimization.
    assert_eq!(
        mem::size_of::<Result<(), oneshot::SendError<[u8; 1024]>>>(),
        EXPECTED
    );
}
