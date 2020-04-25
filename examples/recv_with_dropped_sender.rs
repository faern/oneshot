use std::mem;

fn main() {
    let (sender, receiver) = oneshot::channel::<u128>();
    mem::drop(sender);
    receiver.recv().unwrap_err();
}
