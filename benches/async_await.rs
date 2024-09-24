use async_std::task::block_on;
use criterion::{criterion_group, criterion_main, Bencher, Criterion};
use sensor_fuse::{
    executor::ExecRegister,
    lock::{parking_lot as pl, std_sync as ss},
    DerefSensorData, SensorObserveAsync, SensorWriter, ShareStrategy,
};
use std::{hint::black_box, mem, task::Waker, thread, time::Duration};
use tokio::runtime::{self, Runtime};

fn setup_env<S, R>(
    readers: usize,
    writers: usize,
    max_value: usize,
) -> (Runtime, SensorWriter<usize, S>)
where
    S: Clone + Send + 'static,
    R: DerefSensorData<Target = usize> + Send + 'static,
    for<'a> &'a S:
        ShareStrategy<'a, Target = usize, Lock = R::Lock, Executor = R::Executor, Shared = R>,
    for<'a, 'b> <&'a S as ShareStrategy<'a>>::Executor: ExecRegister<&'b Waker>,
    SensorWriter<usize, S>: From<usize>,
{
    let r = runtime::Builder::new_multi_thread().build().unwrap();

    let writer = SensorWriter::from(0usize);

    for _ in 0..writers {
        let writer = writer.clone();
        thread::spawn(move || loop {
            writer.modify_with(|x| {
                if *x >= max_value {
                    return;
                }
                *x += 1;
            });
        });
    }

    for _ in 0..readers {
        let mut reader = writer.spawn_observer();
        r.spawn(async move {
            loop {
                let mut first = false;
                let guard = reader
                    .wait_for(|_| {
                        let mut retval = true;
                        mem::swap(&mut first, &mut retval);
                        retval
                    })
                    .await;
                black_box(drop(guard));
            }
        });
    }

    (r, writer)
}

fn bench_time_to_max_val<S, R>(writer: SensorWriter<usize, S>, max_value: usize)
where
    S: Clone + Send + 'static,
    R: DerefSensorData<Target = usize> + Send + 'static,
    for<'a> &'a S:
        ShareStrategy<'a, Target = usize, Lock = R::Lock, Executor = R::Executor, Shared = R>,
    for<'a, 'b> <&'a S as ShareStrategy<'a>>::Executor: ExecRegister<&'b Waker>,
    SensorWriter<usize, S>: From<usize>,
{
    let mut observer = writer.spawn_referenced_observer();
    writer.update(0);
    let _r = black_box(block_on(observer.wait_for(|x| *x == max_value)));
}

fn pl_rw(c: &mut Criterion) {
    let (_r, writer) = setup_env::<pl::ArcRwSensorDataExec<usize>, _>(10, 100, 20000);
    c.bench_function("pl_w10_r100_20000", |b| {
        b.iter(|| {
            bench_time_to_max_val::<pl::ArcRwSensorDataExec<usize>, _>(writer.clone(), 20000)
        });
    });
}

fn pl_rw_low_contention(c: &mut Criterion) {
    let (_r, writer) = setup_env::<pl::ArcRwSensorDataExec<usize>, _>(1, 5, 20000);
    c.bench_function("pl_w5_r5_20000", |b| {
        b.iter(|| {
            bench_time_to_max_val::<pl::ArcRwSensorDataExec<usize>, _>(writer.clone(), 20000)
        });
    });
}

fn ss_rw(c: &mut Criterion) {
    let (_r, writer) = setup_env::<ss::ArcRwSensorDataExec<usize>, _>(10, 100, 20000);
    c.bench_function("ss_w10_r100_20000", |b| {
        b.iter(|| {
            bench_time_to_max_val::<ss::ArcRwSensorDataExec<usize>, _>(writer.clone(), 20000)
        });
    });
}

fn ss_rw_low_contention(c: &mut Criterion) {
    let (_r, writer) = setup_env::<ss::ArcRwSensorDataExec<usize>, _>(1, 5, 20000);
    c.bench_function("ss_w5_r5_20000", |b| {
        b.iter(|| {
            bench_time_to_max_val::<ss::ArcRwSensorDataExec<usize>, _>(writer.clone(), 20000)
        });
    });
}

criterion_group!(
    benches,
    pl_rw,
    ss_rw,
    pl_rw_low_contention,
    ss_rw_low_contention
);
criterion_main!(benches);
