use criterion::{black_box, criterion_group, criterion_main, Criterion};
use std::mem;
use std::time::{Duration, Instant};

criterion_group!(benches, bench);
criterion_main!(benches);

fn bench(c: &mut Criterion) {
    bench_send_recv(c, "zst", || ());
    bench_send_recv(c, "1byte", || 9u8);
    bench_send_recv(c, "4096bytes", || [99u8; 4096]);
    bench_try_recv(c);
    bench_recv_deadline_now(c);
    bench_recv_timeout_zero(c);
}

fn bench_send_recv<T>(c: &mut Criterion, value_name: &str, value: impl Fn() -> T) {
    c.bench_function(&format!("create_channel_{}", value_name), |b| {
        b.iter(oneshot::channel::<T>)
    });
    c.bench_function(&format!("create_and_send_{}", value_name), |b| {
        b.iter(|| {
            let (sender, _receiver) = oneshot::channel();
            sender.send(black_box(value())).unwrap()
        });
    });
    c.bench_function(
        &format!("create_and_send_{}_on_closed_channel", value_name),
        |b| {
            b.iter(|| {
                let (sender, receiver) = oneshot::channel();
                mem::drop(receiver);
                sender.send(black_box(value())).unwrap_err()
            });
        },
    );
    c.bench_function(&format!("create_send_and_recv_{}", value_name), |b| {
        b.iter(|| {
            let (sender, receiver) = oneshot::channel();
            sender.send(black_box(value())).unwrap();
            receiver.recv().unwrap()
        });
    });
    c.bench_function(&format!("create_send_and_recv_ref_{}", value_name), |b| {
        b.iter(|| {
            let (sender, receiver) = oneshot::channel();
            sender.send(black_box(value())).unwrap();
            receiver.recv_ref().unwrap()
        });
    });
}

fn bench_try_recv(c: &mut Criterion) {
    let (sender, receiver) = oneshot::channel::<u128>();
    c.bench_function("try_recv", |b| b.iter(|| receiver.try_recv()));
    mem::drop(sender);
    c.bench_function("try_recv_closed", |b| b.iter(|| receiver.try_recv()));
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
