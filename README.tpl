# {{crate}}

{{readme}}

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

{{license}}
