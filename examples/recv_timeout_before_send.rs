use std::thread;
use std::time::Duration;

fn main() {
    let (sender, receiver) = oneshot::channel();
    let t = thread::spawn(move || {
        thread::sleep(Duration::from_millis(2));
        sender.send(9u128).unwrap();
    });
    assert_eq!(receiver.recv_timeout(Duration::from_millis(100)), Ok(9));
    t.join().unwrap();
}
