# oneshot

Oneshot spsc channel. The sender's send method is non-blocking, lock- and wait-free[1].
The receiver supports both lock- and wait-free `try_recv` as well as indefinite and time
limited thread blocking receive operations. The receiver also implements `Future` and
supports asynchronously awaiting the message.

This is a oneshot channel implementation. Meaning each channel instance can only transport
a single message. This has a few nice outcomes. One thing is that the implementation can
be very efficient, utilizing the knowledge that there will only be one message. But more
importantly, it allows the API to be expressed in such a way that certain edge cases
that you don't want to care about when only sending a single message on a channel does not
exist. For example. The sender can't be copied or cloned and the send method takes ownership
and consumes the sender. So you are guaranteed, at the type level, that there can only be
one message sent.

## Examples

A very basic example to just show the API:

```rust
let (sender, receiver) = oneshot::channel();
thread::spawn(move || {
    sender.send("Hello from worker thread!");
});

let message = receiver.recv().expect("Worker thread does not want to talk :(");
println!("A message from a different thread: {}", message);
```

A slightly larger example showing communicating back work of *different types* during a
long computation. The final result here could have been communicated back via the thread's
`JoinHandle`. But those can't be waited on with a timeout. This is a quite artificial example,
that mostly shows the API.

```rust
let (sender1, receiver1) = oneshot::channel();
let (sender2, receiver2) = oneshot::channel();

let thread = thread::spawn(move || {
    let data_processor = expensive_initialization();
    sender1.send(data_processor.summary()).expect("Main thread not waiting");
    sender2.send(data_processor.expensive_computation()).expect("Main thread not waiting");
});

let summary = receiver1.recv().expect("Worker thread died");
println!("Initialized data. Will crunch these numbers: {}", summary);

let result = loop {
    match receiver2.recv_timeout(Duration::from_secs(1)) {
        Ok(result) => break result,
        Err(oneshot::RecvTimeoutError::Timeout) => println!("Still working..."),
        Err(oneshot::RecvTimeoutError::Disconnected) => panic!("Worker thread died"),
    }
};
println!("Done computing. Results: {:?}", result);
thread.join().expect("Worker thread panicked");
```

## Sync vs async

The main motivation for writing this library was that there were no (known to me) channel
implementations allowing you to seamlessly send messages between a normal thread and an async
task, or the other way around. If message passing is the way you are communicating, of course
that should work smoothly between the sync and async parts of the program!

This library achieves that by having an almost[1] wait-free send operation that can safely
be used in both sync threads and async tasks. The receiver has both thread blocking
receive methods for synchronous usage, and implements `Future` for asynchronous usage.

The receiving endpoint of this channel implements Rust's `Future` trait and can be waited on
in an asynchronous task. This implementation is completely executor/runtime agnostic. It should
be possible to use this library with any executor.

## Footnotes

[1]: See documentation on [Sender::send] for situations where it might not be fully wait-free.

## Implementation correctness

This library uses a lot of unsafe Rust in order to be fast and efficient. I am of the opinion
that Rust code should avoid unsafe, except for low level primitives where the performance
gains can be large and therefore justified. Or FFI where unsafe Rust is needed. I classify
this as a low level primitive.

The test suite and CI for this project uses various ways to try to find potential bugs. First of
all, the tests are executed under valgrind in order to try and detect invalid allocations.
Secondly the tests are ran an extra time with some sleeps injected in the code in order to
try and trigger different execution paths. Lastly, most tests are also executed a third time
with [loom]. Loom is a tool for testing concurrent Rust code, trying all possible
thread sheduling outcomes. It also tracks every memory allocation and free in order to detect
invalid usage, kind of like valgrind.

[loom]: https://crates.io/crates/loom

## My message passing frustrations and dreams

Message passing is a very common and good way of synchronization in a concurrent program. But
using it in Rust is far from smooth at the moment. Let me explain my ideal message passing
library before coming to the problems:

* Have dedicated channel primitives for all the common channel types:
  * Oneshot spsc (this is what this crate implements)
  * Rendezvous spsc and mpmc
  * Bounded mpmc
  * Unbounded mpmc
  * Broadcast mpmc (every consumer see every message)
* All the send and receive methods that can't be completed in a lock-free manner should have both
  thread blocking and async versions of themselves. The async version should be executor agnostic,
  not depend on any one executor and work on all of them.
* All channels should seamlessly and ergonomically allow sending messages between two threads,
  between two async tasks and in both directions between a thread and task.
* Be a dedicated message passing crate with as few dependencies as possible. Many of the current
  channel types are embedded in larger crates or async runtime crates, making the barrier for
  depending on them larger.

Where it makes sense from an API or implementation standpoint, the mpmc channels could also come
with dedicated mpsc and spmc versions. If it allows the API to more exactly express the type's
functionality and/or if the implementation can be made noticeably more efficient.

None of the existing channels that I have found satisfy all the above conditions. The main
thing that is missing is seamless interoperability between the sync and async worlds. All channels
bundled up with async runtimes (`tokio`, `async-std` and `futures`) does not allow thread blocking
send or receive calls. And the standard library and for example
`crossbeam-channel` does not implement `Future` at all, so no async support.

# Licence

MIT OR Apache-2.0
