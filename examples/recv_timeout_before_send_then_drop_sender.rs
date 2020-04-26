use std::mem;
use std::thread;
use std::time::Duration;

fn main() {
    let (sender, receiver) = oneshot::channel::<u128>();
    let t = thread::spawn(move || {
        thread::sleep(Duration::from_millis(2));
        mem::drop(sender);
    });
    assert!(receiver.recv_timeout(Duration::from_millis(100)).is_err());
    t.join().unwrap();
}
