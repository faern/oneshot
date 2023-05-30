use criterion::{black_box, criterion_group, criterion_main, Criterion, BenchmarkGroup, BenchmarkId, Throughput, BatchSize};
use std::mem;
use std::time::{Duration, Instant};
use criterion::measurement::WallTime;

criterion_group!(benches, bench);
criterion_main!(benches);

macro_rules! bench_send_and_recv {
    ($c:expr; $($size:expr),+ $(,)?) => {
        // Sanity check that all $values are of $type.
        {
            let mut group = $c.benchmark_group("create_channel");
            $(create::<$size>(&mut group);)+
            group.finish();
        }
        {
            let mut group = $c.benchmark_group("send");
            $(send::<$size>(&mut group);)+
            group.finish();
        }
        {
            let mut group = $c.benchmark_group("send_on_closed");
            $(send_on_closed::<$size>(&mut group);)+
            group.finish();
        }
        {
            let mut group = $c.benchmark_group("send_and_recv");
            $(send_and_recv::<$size>(&mut group);)+
            group.finish();
        }
        {
            let mut group = $c.benchmark_group("send_and_recv_ref");
            $(send_and_recv_ref::<$size>(&mut group);)+
            group.finish();
        }
        {
            let mut group = $c.benchmark_group("recv");
            $(recv::<$size>(&mut group);)+
            group.finish();
        }
    };
}

fn create<const N: usize>(group: &mut BenchmarkGroup<WallTime>) {
    group.bench_with_input(BenchmarkId::from_parameter(N), &N, |b, _| {
        b.iter(oneshot::channel::<[u8; N]>);
    });
}

fn send<const N: usize>(group: &mut BenchmarkGroup<WallTime>) {
    group.throughput(Throughput::Bytes(N as u64));
    group.bench_with_input(BenchmarkId::from_parameter(N), &N, |b, _| {
        b.iter_batched(
            || oneshot::channel::<[u8; N]>(),
            |(sender, _receiver)| {
                sender.send(black_box([0b10101010u8; N])).unwrap()
            },
            BatchSize::SmallInput,
        );
    });
}

fn send_on_closed<const N: usize>(group: &mut BenchmarkGroup<WallTime>) {
    group.throughput(Throughput::Bytes(N as u64));
    group.bench_with_input(BenchmarkId::from_parameter(N), &N, |b, _| {
        b.iter_batched(
            || oneshot::channel::<[u8; N]>(),
            |(sender, receiver)| {
                drop(receiver);
                sender.send(black_box([0b10101010u8; N])).unwrap_err()
            },
            BatchSize::SmallInput,
        );
    });
}

fn send_and_recv<const N: usize>(group: &mut BenchmarkGroup<WallTime>) {
    group.throughput(Throughput::Bytes(N as u64));
    group.bench_with_input(BenchmarkId::from_parameter(N), &N, |b, _| {
        b.iter_batched(
            || oneshot::channel::<[u8; N]>(),
            |(sender, receiver)| {
                sender.send(black_box([0b10101010u8; N])).unwrap();
                receiver.recv().unwrap()
            },
            BatchSize::SmallInput,
        );
    });
}

fn send_and_recv_ref<const N: usize>(group: &mut BenchmarkGroup<WallTime>) {
    group.throughput(Throughput::Bytes(N as u64));
    group.bench_with_input(BenchmarkId::from_parameter(N), &N, |b, _| {
        b.iter_batched(
            || oneshot::channel::<[u8; N]>(),
            |(sender, receiver)| {
                sender.send(black_box([0b10101010u8; N])).unwrap();
                receiver.recv_ref().unwrap()
            },
            BatchSize::SmallInput,
        );
    });
}

fn recv<const N: usize>(group: &mut BenchmarkGroup<WallTime>) {
    group.throughput(Throughput::Bytes(N as u64));
    group.bench_with_input(BenchmarkId::from_parameter(N), &N, |b, _| {
        b.iter_batched(
            || {
                let (sender, receiver) = oneshot::channel::<[u8; N]>();
                sender.send(black_box([0b10101010u8; N])).unwrap();
                receiver
            },
            |receiver| receiver.recv().unwrap(),
            BatchSize::SmallInput,
        );
    });
}

fn bench(c: &mut Criterion) {
    bench_send_and_recv!(c;
        0,
        1,
        8,
        16,
        64,
        1024,
        4096,
    );

    bench_try_recv(c);
    bench_recv_deadline_now(c);
    bench_recv_timeout_zero(c);
}

fn bench_try_recv(c: &mut Criterion) {
    let (sender, receiver) = oneshot::channel::<u128>();
    c.bench_function("try_recv_empty", |b| {
        b.iter(|| receiver.try_recv().unwrap_err())
    });
    mem::drop(sender);
    c.bench_function("try_recv_empty_closed", |b| {
        b.iter(|| receiver.try_recv().unwrap_err())
    });
}

fn bench_recv_deadline_now(c: &mut Criterion) {
    let now = Instant::now();
    {
        let (_sender, receiver) = oneshot::channel::<u128>();
        c.bench_function("recv_deadline_now", |b| {
            b.iter(|| receiver.recv_deadline(now).unwrap_err())
        });
    }
    {
        let (sender, receiver) = oneshot::channel::<u128>();
        mem::drop(sender);
        c.bench_function("recv_deadline_now_closed", |b| {
            b.iter(|| receiver.recv_deadline(now).unwrap_err())
        });
    }
}

fn bench_recv_timeout_zero(c: &mut Criterion) {
    let zero = Duration::from_nanos(0);
    {
        let (_sender, receiver) = oneshot::channel::<u128>();
        c.bench_function("recv_timeout_zero", |b| {
            b.iter(|| receiver.recv_timeout(zero).unwrap_err())
        });
    }
    {
        let (sender, receiver) = oneshot::channel::<u128>();
        mem::drop(sender);
        c.bench_function("recv_timeout_zero_closed", |b| {
            b.iter(|| receiver.recv_timeout(zero).unwrap_err())
        });
    }
}
