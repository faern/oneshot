# oneshot

Oneshot spsc channel. The sender's send method is non-blocking, lock- and wait-free.
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

## Sync vs async

The main thing motivating this library was that there were no (known to me) channel
implementations allowing you to seamlessly send messages between a normal thread and an async
task, or the other way around. If message passing is the way you are communicating, of course
that should work smoothly between the sync and async parts of the program!

This library achieves that by having a completely wait-free send operation that can safely
be used in both sync threads and async tasks. The receiver has both thread blocking
receive methods for synchronous usage, and implements `Future` for asynchronous usage.

## Asynchronous support

The receiving endpoint of this channel implements Rust's `Future` trait and can be waited on
in an asynchronous task. This implementation is completely executor/runtime agnostic. It should
be possible to use this library with any executor.

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

### Atomic ordering

The library currently uses sequential consistency for all atomic operations. This is to be somewhat
conservative for now. If you understand atomic memory orderings really well, please feel free
to suggest possible relaxations, I know there are many. Just motivate them well.

# Licence

MIT OR Apache-2.0
