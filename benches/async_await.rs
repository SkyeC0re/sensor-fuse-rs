use async_std::task::block_on;
use criterion::{criterion_group, criterion_main, Criterion};
use rand::random;
use sensor_fuse::{
    executor::ExecRegister,
    lock::{parking_lot as pl, std_sync as ss},
    DerefSensorData, SensorObserveAsync, SensorWriter, ShareStrategy,
};
use std::{
    hint::black_box,
    sync::atomic::{AtomicUsize, Ordering},
    task::Waker,
};
use tokio::{
    runtime::{self, Runtime},
    sync::watch,
};
#[derive(Default)]
struct ContentionData {
    test_version: usize,
    individual_writes_required: usize,
    writers_completed: usize,
    individual_observations_required: usize,
    observers_completed: AtomicUsize,
}

struct WatchContentionEnvironment {
    reader_handles: Vec<tokio::task::JoinHandle<()>>,
    writer_handles: Vec<tokio::task::JoinHandle<()>>,
    writer: watch::Sender<ContentionData>,
    _runtime: Runtime,
}

impl WatchContentionEnvironment {
    fn new(reader_count: usize, writer_count: usize) -> Self {
        let writer = watch::Sender::new(ContentionData::default());
        let runtime = runtime::Builder::new_multi_thread().build().unwrap();

        let mut reader_handles = Vec::new();
        for _ in black_box(0..reader_count) {
            let mut reader = writer.subscribe();
            reader_handles.push(runtime.spawn(async move {
                let mut test_version = 0;
                let mut observations = 0;
                loop {
                    let mut iteration = 0;
                    let guard = reader
                        .wait_for(|state| {
                            let mut ret = state.test_version == usize::MAX || iteration > 1;

                            if iteration == 1 {
                                ret |= random::<bool>();
                            }

                            iteration += 1;
                            ret
                        })
                        .await
                        .unwrap();

                    if guard.test_version == usize::MAX {
                        break;
                    }

                    if test_version != guard.test_version {
                        test_version = guard.test_version;
                        observations = 0;
                    }

                    if observations <= guard.individual_observations_required {
                        observations += 1;
                        if observations > guard.individual_observations_required {
                            guard.observers_completed.fetch_add(1, Ordering::Relaxed);
                        }
                    }

                    black_box(drop(guard));
                }
            }));
        }

        let mut writer_handles = Vec::new();
        for _ in black_box(0..writer_count) {
            let writer = writer.clone();
            writer_handles.push(runtime.spawn_blocking(move || {
                let mut test_version = 0;
                let mut writes = 0;
                loop {
                    black_box(writer.send_modify(|state| {
                        if test_version != state.test_version {
                            writes = 0;
                            test_version = state.test_version;
                        }

                        if writes <= state.individual_writes_required {
                            writes += 1;
                            if writes > state.individual_writes_required {
                                state.writers_completed += 1;
                            }
                        }
                    }));

                    if test_version == usize::MAX {
                        break;
                    }
                }
            }));
        }

        Self {
            reader_handles,
            writer_handles,
            writer,
            _runtime: runtime,
        }
    }

    fn bench_individual_writes_req(&self, required_writes: usize) {
        let mut observer = self.writer.subscribe();
        self.writer.send_modify(|state| {
            state.test_version += 1;
            state.observers_completed.fetch_and(0, Ordering::Relaxed);
            state.writers_completed = 0;
            state.individual_writes_required = required_writes;
        });

        let writers_len = self.writer_handles.len();

        let _ = black_box(block_on(
            observer.wait_for(|state| state.writers_completed == writers_len),
        ));
    }
}

impl Drop for WatchContentionEnvironment {
    fn drop(&mut self) {
        self.writer
            .send_modify(|state| state.test_version = usize::MAX);
        for reader_handle in self.reader_handles.drain(..) {
            let _ = block_on(reader_handle);
        }

        for writer_handle in self.writer_handles.drain(..) {
            let _ = block_on(writer_handle);
        }
    }
}

struct ContentionEnvironment<S, R>
where
    // Writers must be cloneable and sendable across threads.
    S: Clone + Send + 'static,
    // Observers must be sendable across threads.
    R: DerefSensorData<Target = ContentionData> + Send + 'static,
    // Spawning observers must not borrow writers.
    for<'a> &'a S: ShareStrategy<
        'a,
        Target = ContentionData,
        Lock = R::Lock,
        Executor = R::Executor,
        Shared = R,
    >,
    // Writer must support async.
    for<'a, 'b> <&'a S as ShareStrategy<'a>>::Executor: ExecRegister<&'b Waker>,
    // Writer must be creatable from an initial value.
    SensorWriter<ContentionData, S>: From<ContentionData>,
{
    reader_handles: Vec<tokio::task::JoinHandle<()>>,
    writer_handles: Vec<tokio::task::JoinHandle<()>>,
    writer: SensorWriter<ContentionData, S>,
    _runtime: Runtime,
}

impl<S, R> ContentionEnvironment<S, R>
where
    S: Clone + Send + 'static,
    R: DerefSensorData<Target = ContentionData> + Send + 'static,
    for<'a> &'a S: ShareStrategy<
        'a,
        Target = ContentionData,
        Lock = R::Lock,
        Executor = R::Executor,
        Shared = R,
    >,
    for<'a, 'b> <&'a S as ShareStrategy<'a>>::Executor: ExecRegister<&'b Waker>,
    SensorWriter<ContentionData, S>: From<ContentionData>,
{
    fn new(reader_count: usize, writer_count: usize) -> Self {
        let writer = SensorWriter::<ContentionData, S>::from(ContentionData::default());
        let runtime = runtime::Builder::new_multi_thread().build().unwrap();

        let mut reader_handles = Vec::new();
        for _ in black_box(0..reader_count) {
            let mut reader = writer.spawn_observer();
            reader_handles.push(runtime.spawn(async move {
                let mut test_version = 0;
                let mut observations = 0;
                loop {
                    let mut iteration = 0;
                    let guard = match reader
                        .wait_for(|state| {
                            let mut ret = state.test_version == usize::MAX || iteration > 1;

                            if iteration == 1 {
                                ret |= random::<bool>();
                            }

                            iteration += 1;
                            ret
                        })
                        .await
                    {
                        Ok(guard) => guard,
                        Err(guard) => guard,
                    };

                    if guard.test_version == usize::MAX {
                        break;
                    }

                    if test_version != guard.test_version {
                        test_version = guard.test_version;
                        observations = 0;
                    }

                    if observations <= guard.individual_observations_required {
                        observations += 1;
                        if observations > guard.individual_observations_required {
                            guard.observers_completed.fetch_add(1, Ordering::Relaxed);
                        }
                    }

                    black_box(drop(guard));
                }
            }));
        }

        let mut writer_handles = Vec::new();
        for _ in black_box(0..writer_count) {
            let writer = writer.clone();
            writer_handles.push(runtime.spawn_blocking(move || {
                let mut test_version = 0;
                let mut writes = 0;
                loop {
                    black_box(writer.modify_with(|state| {
                        if test_version != state.test_version {
                            writes = 0;
                            test_version = state.test_version;
                        }

                        if writes <= state.individual_writes_required {
                            writes += 1;
                            if writes > state.individual_writes_required {
                                state.writers_completed += 1;
                            }
                        }
                    }));

                    if test_version == usize::MAX {
                        break;
                    }
                }
            }));
        }

        Self {
            reader_handles,
            writer_handles,
            writer,
            _runtime: runtime,
        }
    }

    fn bench_individual_writes_req(&self, required_writes: usize) {
        let mut observer = self.writer.spawn_observer();
        self.writer.modify_with(|state| {
            state.test_version += 1;
            state.observers_completed.fetch_and(0, Ordering::Relaxed);
            state.writers_completed = 0;
            state.individual_writes_required = required_writes;
        });

        let writers_len = self.writer_handles.len();

        let _ = black_box(block_on(
            observer.wait_for(|state| state.writers_completed == writers_len),
        ));
    }
}

impl<S, R> Drop for ContentionEnvironment<S, R>
where
    S: Clone + Send + 'static,
    R: DerefSensorData<Target = ContentionData> + Send + 'static,
    for<'a> &'a S: ShareStrategy<
        'a,
        Target = ContentionData,
        Lock = R::Lock,
        Executor = R::Executor,
        Shared = R,
    >,
    for<'a, 'b> <&'a S as ShareStrategy<'a>>::Executor: ExecRegister<&'b Waker>,
    SensorWriter<ContentionData, S>: From<ContentionData>,
{
    fn drop(&mut self) {
        self.writer
            .modify_with(|state| state.test_version = usize::MAX);
        for reader_handle in self.reader_handles.drain(..) {
            let _ = block_on(reader_handle);
        }

        for writer_handle in self.writer_handles.drain(..) {
            let _ = block_on(writer_handle);
        }
    }
}

fn pl_rw(c: &mut Criterion) {
    let env = ContentionEnvironment::<pl::ArcRwSensorDataExec<ContentionData>, _>::new(100, 10);

    c.bench_function("pl_r100_w10_2000", |b| {
        b.iter(|| {
            env.bench_individual_writes_req(2000);
        });
    });
}

fn pl_rw_low_contention(c: &mut Criterion) {
    let env = ContentionEnvironment::<pl::ArcRwSensorDataExec<ContentionData>, _>::new(5, 5);

    c.bench_function("pl_r5_w5_2000", |b| {
        b.iter(|| {
            env.bench_individual_writes_req(2000);
        });
    });
}

fn ss_rw(c: &mut Criterion) {
    let env = ContentionEnvironment::<ss::ArcRwSensorDataExec<ContentionData>, _>::new(100, 10);

    c.bench_function("ss_r100_w10_2000", |b| {
        b.iter(|| {
            env.bench_individual_writes_req(2000);
        });
    });
}

fn ss_rw_low_contention(c: &mut Criterion) {
    let env = ContentionEnvironment::<ss::ArcRwSensorDataExec<ContentionData>, _>::new(5, 5);

    c.bench_function("ss_r5_w5_2000", |b| {
        b.iter(|| {
            env.bench_individual_writes_req(2000);
        });
    });
}

fn tw(c: &mut Criterion) {
    let env = WatchContentionEnvironment::new(100, 10);

    c.bench_function("tw_r100_w10_2000", |b| {
        b.iter(|| {
            env.bench_individual_writes_req(2000);
        });
    });
}

fn tw_low_contention(c: &mut Criterion) {
    let env = WatchContentionEnvironment::new(5, 5);

    c.bench_function("tw_r5_w5_2000", |b| {
        b.iter(|| {
            env.bench_individual_writes_req(2000);
        });
    });
}

criterion_group!(
    benches,
    pl_rw,
    ss_rw,
    tw,
    pl_rw_low_contention,
    ss_rw_low_contention,
    tw_low_contention,
);
criterion_main!(benches);
