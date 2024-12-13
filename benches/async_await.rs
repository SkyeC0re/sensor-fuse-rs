use criterion::{
    criterion_group, criterion_main, measurement::WallTime, BenchmarkGroup, Criterion,
};
use futures::executor::block_on;
use rand::random;
use sensor_fuse::{
    sensor_core::{alloc::AsyncCore, SensorCoreAsync},
    DerefSensorData, SensorObserveAsync, SensorWriter, ShareStrategy,
};
use std::{
    hint::black_box,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};
use tokio::{
    runtime::{self, Runtime},
    sync::watch,
};
#[derive(Default)]
struct ContentionData {
    test_version: usize,
    data_version: usize,
    individual_writes_required: usize,
    writers_completed: usize,
    individual_observations_required: usize,
    observers_completed: AtomicUsize,
    writer_spin_loop_iters: usize,
    reader_spin_loop_iters: usize,
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
        let runtime = runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .build()
            .unwrap();

        let mut reader_handles = Vec::new();
        for _ in 0..reader_count {
            let mut reader = writer.subscribe();
            reader_handles.push(runtime.spawn(async move {
                let mut test_version = 0;
                let mut observations = 0;
                let mut last_observed_data = 0;
                loop {
                    let mut iteration = 0;
                    let guard = reader
                        .wait_for(|state| {
                            if state.test_version == usize::MAX {
                                return true;
                            }

                            if iteration > 0 && last_observed_data == state.data_version {
                                return false;
                            }

                            last_observed_data = state.data_version;
                            let mut ret = iteration > 1;

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

                    black_box(for _ in 0..guard.reader_spin_loop_iters {
                        std::hint::spin_loop();
                    });
                }
            }));
        }

        let mut writer_handles = Vec::new();
        for _ in 0..writer_count {
            let writer = writer.clone();
            writer_handles.push(runtime.spawn_blocking(move || {
                let mut test_version = 0;
                let mut writes = 0;
                loop {
                    writer.send_modify(|state| {
                        state.data_version = state.data_version.wrapping_add(1);
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

                        black_box(for _ in 0..state.writer_spin_loop_iters {
                            std::hint::spin_loop();
                        });
                    });

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

    fn bench_individual_writes_req(&self, required_writes: usize, observer_spin_loops: usize) {
        let mut observer = self.writer.subscribe();
        self.writer.send_modify(|state| {
            state.test_version += 1;
            state.observers_completed.fetch_and(0, Ordering::Relaxed);
            state.writers_completed = 0;
            state.individual_writes_required = required_writes;
            state.reader_spin_loop_iters = observer_spin_loops;
            state.writer_spin_loop_iters = 0;
        });

        let writers_len = self.writer_handles.len();

        let _ = black_box(block_on(
            observer.wait_for(|state| state.writers_completed == writers_len),
        ));
    }

    fn bench_individual_observations_req(
        &self,
        required_observations: usize,
        writer_spin_loops: usize,
    ) {
        let mut observer = self.writer.subscribe();
        self.writer.send_modify(|state| {
            state.test_version += 1;
            state.observers_completed.fetch_and(0, Ordering::Relaxed);
            state.writers_completed = 0;
            state.individual_observations_required = required_observations;
            state.reader_spin_loop_iters = 0;
            state.writer_spin_loop_iters = writer_spin_loops;
        });

        let readers_len = self.reader_handles.len();

        let _ = black_box(block_on(observer.wait_for(|state| {
            state.observers_completed.load(Ordering::Relaxed) == readers_len
        })));
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
    for<'a> &'a S: ShareStrategy<'a, Target = ContentionData, Shared = R, Core = R::Core>,
    // Core must support async.
    R::Core: SensorCoreAsync,
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
    R: DerefSensorData<Target = ContentionData, Core = AsyncCore<ContentionData>> + Send + 'static,
    for<'a> &'a S: ShareStrategy<'a, Target = ContentionData, Shared = R, Core = R::Core>,
    SensorWriter<ContentionData, S>: From<ContentionData>,
{
    fn new(reader_count: usize, writer_count: usize) -> Self {
        let writer = SensorWriter::<ContentionData, S>::from(ContentionData::default());
        let runtime = runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .build()
            .unwrap();

        let mut reader_handles = Vec::new();
        for _ in 0..reader_count {
            let mut reader = writer.spawn_observer();
            reader_handles.push(runtime.spawn(async move {
                let mut test_version = 0;
                let mut observations = 0;
                let mut last_observed_data = 0;
                loop {
                    let mut iteration = 0;
                    let guard = reader
                        .wait_for(|state: &ContentionData| {
                            if state.test_version == usize::MAX {
                                return true;
                            }

                            if iteration > 0 && last_observed_data == state.data_version {
                                return false;
                            }

                            last_observed_data = state.data_version;
                            let mut ret = iteration > 1;

                            if iteration == 1 {
                                ret |= random::<bool>();
                            }

                            iteration += 1;
                            ret
                        })
                        .await
                        .0;

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

                    black_box(for _ in 0..guard.reader_spin_loop_iters {
                        std::hint::spin_loop();
                    });
                }
            }));
        }

        let mut writer_handles = Vec::new();
        for _ in 0..writer_count {
            let writer = writer.clone();
            writer_handles.push(runtime.spawn_blocking(move || {
                let mut test_version = 0;
                let mut writes = 0;
                loop {
                    block_on(writer.modify_with(|state| {
                        state.data_version = state.data_version.wrapping_add(1);
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

                        black_box(for _ in 0..state.writer_spin_loop_iters {
                            std::hint::spin_loop();
                        });

                        true
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

    fn bench_individual_writes_req(&self, required_writes: usize, observer_spin_loops: usize) {
        block_on(async {
            let mut observer = self.writer.spawn_observer();
            self.writer
                .modify_with(|state| {
                    state.test_version += 1;
                    state.observers_completed.fetch_and(0, Ordering::Relaxed);
                    state.writers_completed = 0;
                    state.individual_writes_required = required_writes;
                    state.reader_spin_loop_iters = observer_spin_loops;
                    state.writer_spin_loop_iters = 0;
                    true
                })
                .await;

            let writers_len = self.writer_handles.len();

            let _ = black_box(
                observer
                    .wait_for(|state| state.writers_completed == writers_len)
                    .await,
            );
        });
    }

    fn bench_individual_observations_req(
        &self,
        required_observations: usize,
        writer_spin_loops: usize,
    ) {
        block_on(async {
            let mut observer = self.writer.spawn_observer();
            self.writer
                .modify_with(|state| {
                    state.test_version += 1;
                    state.observers_completed.fetch_and(0, Ordering::Relaxed);
                    state.writers_completed = 0;
                    state.individual_observations_required = required_observations;
                    state.reader_spin_loop_iters = 0;
                    state.writer_spin_loop_iters = writer_spin_loops;
                    true
                })
                .await;

            let readers_len = self.reader_handles.len();

            let _ = observer
                .wait_for(|state| state.observers_completed.load(Ordering::Relaxed) == readers_len)
                .await;
        });
    }
}

impl<S, R> Drop for ContentionEnvironment<S, R>
where
    // Writers must be cloneable and sendable across threads.
    S: Clone + Send + 'static,
    // Observers must be sendable across threads.
    R: DerefSensorData<Target = ContentionData> + Send + 'static,
    // Spawning observers must not borrow writers.
    for<'a> &'a S: ShareStrategy<'a, Target = ContentionData, Shared = R, Core = R::Core>,
    // Core must support async.
    R::Core: SensorCoreAsync,
    // Writer must be creatable from an initial value.
    SensorWriter<ContentionData, S>: From<ContentionData>,
{
    fn drop(&mut self) {
        block_on(async {
            self.writer
                .modify_with(|state| {
                    state.test_version = usize::MAX;
                    true
                })
                .await;
            for reader_handle in self.reader_handles.drain(..) {
                let _ = reader_handle.await;
            }

            for writer_handle in self.writer_handles.drain(..) {
                let _ = writer_handle.await;
            }
        })
    }
}

fn arc_async_r5_w5_o50_s0_observation(c: &mut BenchmarkGroup<WallTime>) {
    let env = ContentionEnvironment::<Arc<AsyncCore<ContentionData>>, _>::new(5, 5);

    c.bench_function("arc_async_alloc_r5_w5_o50_s0_observation", |b| {
        b.iter(|| {
            env.bench_individual_observations_req(50, 0);
        });
    });
}

fn tokio_watch_r5_w5_o50_s0_observation(c: &mut BenchmarkGroup<WallTime>) {
    let env = WatchContentionEnvironment::new(5, 5);

    c.bench_function("tokio_watch_r5_w5_o50_s0_observation", |b| {
        b.iter(|| {
            env.bench_individual_observations_req(50, 0);
        });
    });
}

fn arc_async_r5_w5_o50_s10_observation(c: &mut BenchmarkGroup<WallTime>) {
    let env = ContentionEnvironment::<Arc<AsyncCore<ContentionData>>, _>::new(5, 5);

    c.bench_function("arc_async_alloc_r5_w5_o50_s10_observation", |b| {
        b.iter(|| {
            env.bench_individual_observations_req(50, 10);
        });
    });
}

fn tokio_watch_r5_w5_o50_s10_observation(c: &mut BenchmarkGroup<WallTime>) {
    let env = WatchContentionEnvironment::new(5, 5);

    c.bench_function("tokio_watch_r5_w5_o50_s10_observation", |b| {
        b.iter(|| {
            env.bench_individual_observations_req(50, 10);
        });
    });
}

fn arc_async_r5_w5_o50_s100_observation(c: &mut BenchmarkGroup<WallTime>) {
    let env = ContentionEnvironment::<Arc<AsyncCore<ContentionData>>, _>::new(10, 10);

    c.bench_function("arc_async_alloc_r5_w5_o50_s100_observation", |b| {
        b.iter(|| {
            env.bench_individual_observations_req(50, 100);
        });
    });
}

fn tokio_watch_r5_w5_o50_s100_observation(c: &mut BenchmarkGroup<WallTime>) {
    let env = WatchContentionEnvironment::new(10, 10);

    c.bench_function("tokio_watch_r5_w5_o50_s100_observation", |b| {
        b.iter(|| {
            env.bench_individual_observations_req(50, 100);
        });
    });
}

fn arc_async_r5_w5_o50_s0_writes(c: &mut BenchmarkGroup<WallTime>) {
    let env = ContentionEnvironment::<Arc<AsyncCore<ContentionData>>, _>::new(5, 5);

    c.bench_function("arc_async_alloc_r5_w5_o50_s0_writes", |b| {
        b.iter(|| {
            env.bench_individual_writes_req(50, 0);
        });
    });
}

fn tokio_watch_r5_w5_o50_s0_writes(c: &mut BenchmarkGroup<WallTime>) {
    let env = WatchContentionEnvironment::new(5, 5);

    c.bench_function("tokio_watch_r5_w5_o50_s0_writes", |b| {
        b.iter(|| {
            env.bench_individual_writes_req(50, 0);
        });
    });
}

fn arc_async_r5_w5_o50_s10_writes(c: &mut BenchmarkGroup<WallTime>) {
    let env = ContentionEnvironment::<Arc<AsyncCore<ContentionData>>, _>::new(5, 5);

    c.bench_function("arc_async_alloc_r5_w5_o50_s10_writes", |b| {
        b.iter(|| {
            env.bench_individual_writes_req(50, 10);
        });
    });
}

fn tokio_watch_r5_w5_o50_s10_writes(c: &mut BenchmarkGroup<WallTime>) {
    let env = WatchContentionEnvironment::new(5, 5);

    c.bench_function("tokio_watch_r5_w5_o50_s10_writes", |b| {
        b.iter(|| {
            env.bench_individual_writes_req(50, 10);
        });
    });
}

fn arc_async_r5_w5_o50_s100_writes(c: &mut BenchmarkGroup<WallTime>) {
    let env = ContentionEnvironment::<Arc<AsyncCore<ContentionData>>, _>::new(10, 10);

    c.bench_function("arc_async_alloc_r5_w5_o50_s100_writes", |b| {
        b.iter(|| {
            env.bench_individual_writes_req(50, 100);
        });
    });
}

fn tokio_watch_r5_w5_o50_s100_writes(c: &mut BenchmarkGroup<WallTime>) {
    let env = WatchContentionEnvironment::new(10, 10);

    c.bench_function("tokio_watch_r5_w5_o50_s100_writes", |b| {
        b.iter(|| {
            env.bench_individual_writes_req(50, 100);
        });
    });
}

pub fn bench_reads() {
    let mut criterion = Criterion::default();
    let mut group = criterion.benchmark_group("reads");

    group.sampling_mode(criterion::SamplingMode::Linear);

    // arc_async_r5_w5_o50_s0_observation(&mut group);
    // tokio_watch_r5_w5_o50_s0_observation(&mut group);
    // arc_async_r5_w5_o50_s10_observation(&mut group);
    // tokio_watch_r5_w5_o50_s10_observation(&mut group);
    arc_async_r5_w5_o50_s100_observation(&mut group);
    tokio_watch_r5_w5_o50_s100_observation(&mut group);
    group.finish();
}

pub fn bench_writes() {
    let mut criterion = Criterion::default();
    let mut group = criterion.benchmark_group("writes");

    group.sampling_mode(criterion::SamplingMode::Linear);

    // arc_async_r5_w5_o50_s0_writes(&mut group);
    // tokio_watch_r5_w5_o50_s0_writes(&mut group);
    // arc_async_r5_w5_o50_s10_writes(&mut group);
    // tokio_watch_r5_w5_o50_s10_writes(&mut group);
    arc_async_r5_w5_o50_s100_writes(&mut group);
    tokio_watch_r5_w5_o50_s100_writes(&mut group);
    group.finish();
}

criterion_main!(bench_writes, bench_reads);
