use criterion::{black_box, criterion_group, criterion_main, Criterion};
use std::mem;
use std::time::{Duration, Instant};

criterion_group!(benches, bench);
criterion_main!(benches);

macro_rules! bench_send_and_recv {
    ($c:expr, $($type:ty => $value:expr);+) => {
        // Sanity check that all $values are of $type.
        $(let _: $type = $value;)*
        $(bench_create_channel::<$type>($c, stringify!($type));)*
        $(bench_create_and_send($c, stringify!($type), || $value);)*
        $(bench_create_and_send_on_closed($c, stringify!($type), || $value);)*
        $(bench_create_send_and_recv($c, stringify!($type), || $value);)*
        $(bench_create_send_and_recv_ref($c, stringify!($type), || $value);)*
    };
}

fn bench(c: &mut Criterion) {
    bench_send_and_recv!(c,
        () => ();
        u8 => 7u8;
        usize => 9876usize;
        u128 => 1234567u128;
        [u8; 64] => [0b10101010u8; 64];
        [u8; 4096] => [0b10101010u8; 4096]
    );

    bench_try_recv(c);
    bench_recv_deadline_now(c);
    bench_recv_timeout_zero(c);
}

fn bench_create_channel<T>(c: &mut Criterion, value_name: &str) {
    c.bench_function(&format!("create_channel<{}>", value_name), |b| {
        b.iter(oneshot::channel::<T>)
    });
}

fn bench_create_and_send<T>(c: &mut Criterion, value_name: &str, value_fn: impl Fn() -> T) {
    c.bench_function(&format!("create_and_send<{}>", value_name), |b| {
        b.iter(|| {
            let (sender, _receiver) = oneshot::channel();
            sender.send(black_box(value_fn())).unwrap()
        });
    });
}

fn bench_create_and_send_on_closed<T>(
    c: &mut Criterion,
    value_name: &str,
    value_fn: impl Fn() -> T,
) {
    c.bench_function(&format!("create_and_send_on_closed<{}>", value_name), |b| {
        b.iter(|| {
            let (sender, receiver) = oneshot::channel();
            mem::drop(receiver);
            sender.send(black_box(value_fn())).unwrap_err()
        });
    });
}

fn bench_create_send_and_recv<T>(c: &mut Criterion, value_name: &str, value_fn: impl Fn() -> T) {
    c.bench_function(&format!("create_send_and_recv<{}>", value_name), |b| {
        b.iter(|| {
            let (sender, receiver) = oneshot::channel();
            sender.send(black_box(value_fn())).unwrap();
            receiver.recv().unwrap()
        });
    });
}
fn bench_create_send_and_recv_ref<T>(
    c: &mut Criterion,
    value_name: &str,
    value_fn: impl Fn() -> T,
) {
    c.bench_function(&format!("create_send_and_recv_ref<{}>", value_name), |b| {
        b.iter(|| {
            let (sender, receiver) = oneshot::channel();
            sender.send(black_box(value_fn())).unwrap();
            receiver.recv_ref().unwrap()
        });
    });
}

fn bench_try_recv(c: &mut Criterion) {
    let (sender, receiver) = oneshot::channel::<u128>();
    c.bench_function("try_recv_empty", |b| b.iter(|| receiver.try_recv()));
    mem::drop(sender);
    c.bench_function("try_recv_empty_closed", |b| b.iter(|| receiver.try_recv()));
}

fn bench_recv_deadline_now(c: &mut Criterion) {
    let now = Instant::now();
    {
        let (_sender, receiver) = oneshot::channel::<u128>();
        c.bench_function("recv_deadline_now", |b| {
            b.iter(|| receiver.recv_deadline(now))
        });
    }
    {
        let (sender, receiver) = oneshot::channel::<u128>();
        mem::drop(sender);
        c.bench_function("recv_deadline_now_closed", |b| {
            b.iter(|| receiver.recv_deadline(now))
        });
    }
}

fn bench_recv_timeout_zero(c: &mut Criterion) {
    let zero = Duration::from_nanos(0);
    {
        let (_sender, receiver) = oneshot::channel::<u128>();
        c.bench_function("recv_timeout_zero", |b| {
            b.iter(|| receiver.recv_timeout(zero))
        });
    }
    {
        let (sender, receiver) = oneshot::channel::<u128>();
        mem::drop(sender);
        c.bench_function("recv_timeout_zero_closed", |b| {
            b.iter(|| receiver.recv_timeout(zero))
        });
    }
}
