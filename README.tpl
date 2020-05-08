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

# Licence

{{license}}
