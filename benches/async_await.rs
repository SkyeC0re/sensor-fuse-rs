use criterion::{criterion_group, criterion_main, Criterion};
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
                    });

                    // spin_sleep::sleep(Duration::from_micros(10));
                    black_box(for _ in 0..1000 {
                        std::hint::spin_loop();
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

    fn bench_individual_observations_req(&self, required_observations: usize) {
        let mut observer = self.writer.subscribe();
        self.writer.send_modify(|state| {
            state.test_version += 1;
            state.observers_completed.fetch_and(0, Ordering::Relaxed);
            state.writers_completed = 0;
            state.individual_observations_required = required_observations;
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
        let runtime = runtime::Builder::new_multi_thread().build().unwrap();

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
                        .wait_for(
                            |state: &ContentionData| {
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
                            },
                            true,
                        )
                        .await
                        .guard;

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

                        true
                    }));

                    // spin_sleep::sleep(Duration::from_micros(10));
                    black_box(for _ in 0..1000 {
                        std::hint::spin_loop();
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

    fn bench_individual_writes_req(&self, required_writes: usize) {
        block_on(async {
            let mut observer = self.writer.spawn_observer();
            self.writer
                .modify_with(|state| {
                    state.test_version += 1;
                    state.observers_completed.fetch_and(0, Ordering::Relaxed);
                    state.writers_completed = 0;
                    state.individual_writes_required = required_writes;
                    true
                })
                .await;

            let writers_len = self.writer_handles.len();

            let _ = black_box(
                observer
                    .wait_for(|state| state.writers_completed == writers_len, true)
                    .await,
            );
        });
    }

    fn bench_individual_observations_req(&self, required_observations: usize) {
        block_on(async {
            let mut observer = self.writer.spawn_observer();
            self.writer
                .modify_with(|state| {
                    state.test_version += 1;
                    state.observers_completed.fetch_and(0, Ordering::Relaxed);
                    state.writers_completed = 0;
                    state.individual_observations_required = required_observations;
                    true
                })
                .await;

            let readers_len = self.reader_handles.len();

            let _ = observer
                .wait_for(
                    |state| state.observers_completed.load(Ordering::Relaxed) == readers_len,
                    true,
                )
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

// fn pl_rw_write(c: &mut Criterion) {
//     let env = ContentionEnvironment::<pl::ArcRwSensorDataExec<ContentionData>, _>::new(100, 10);

//     c.bench_function("pl_r100_w10_2000_write", |b| {
//         b.iter(|| {
//             env.bench_individual_writes_req(2000);
//         });
//     });
// }

// fn pl_rw_low_contention_write(c: &mut Criterion) {
//     let env = ContentionEnvironment::<pl::ArcRwSensorDataExec<ContentionData>, _>::new(15, 7);

//     c.bench_function("pl_r5_w5_2000_write", |b| {
//         b.iter(|| {
//             env.bench_individual_writes_req(2000);
//         });
//     });
// }

// fn ss_rw_write(c: &mut Criterion) {
//     let env = ContentionEnvironment::<ss::ArcRwSensorDataExec<ContentionData>, _>::new(100, 10);

//     c.bench_function("ss_r100_w10_2000_write", |b| {
//         b.iter(|| {
//             env.bench_individual_writes_req(2000);
//         });
//     });
// }

// fn ss_rw_low_contention_write(c: &mut Criterion) {
//     let env = ContentionEnvironment::<ss::ArcRwSensorDataExec<ContentionData>, _>::new(15, 7);

//     c.bench_function("ss_r5_w5_2000_write", |b| {
//         b.iter(|| {
//             env.bench_individual_writes_req(2000);
//         });
//     });
// }

// fn tw_write(c: &mut Criterion) {
//     let env = WatchContentionEnvironment::new(100, 10);

//     c.bench_function("tw_r100_w10_2000_write", |b| {
//         b.iter(|| {
//             env.bench_individual_writes_req(2000);
//         });
//     });
// }

// fn tw_low_contention_write(c: &mut Criterion) {
//     let env = WatchContentionEnvironment::new(15, 7);

//     c.bench_function("tw_r5_w5_2000_write", |b| {
//         b.iter(|| {
//             env.bench_individual_writes_req(2000);
//         });
//     });
// }

// fn pl_rw_observation(c: &mut Criterion) {
//     let env = ContentionEnvironment::<pl::ArcRwSensorDataExec<ContentionData>, _>::new(100, 10);

//     c.bench_function("pl_r100_w10_2000_observation", |b| {
//         b.iter(|| {
//             env.bench_individual_observations_req(2000);
//         });
//     });
// }

// fn pl_rw_low_contention_observation(c: &mut Criterion) {
//     let env = ContentionEnvironment::<pl::ArcRwSensorDataExec<ContentionData>, _>::new(15, 7);

//     c.bench_function("pl_r5_w5_2000_observation", |b| {
//         b.iter(|| {
//             env.bench_individual_observations_req(2000);
//         });
//     });
// }

// fn ss_rw_observation(c: &mut Criterion) {
//     let env = ContentionEnvironment::<ss::ArcRwSensorDataExec<ContentionData>, _>::new(100, 10);

//     c.bench_function("ss_r100_w10_2000_observation", |b| {
//         b.iter(|| {
//             env.bench_individual_observations_req(2000);
//         });
//     });
// }

fn ss_rw_low_contention_observation(c: &mut Criterion) {
    let env = ContentionEnvironment::<Arc<AsyncCore<ContentionData>>, _>::new(5, 2);

    c.bench_function("ss_r5_w5_2000_observation", |b| {
        b.iter(|| {
            env.bench_individual_observations_req(2000);
        });
    });
}

// fn tw_observation(c: &mut Criterion) {
//     let env = WatchContentionEnvironment::new(100, 10);

//     c.bench_function("tw_r100_w10_2000_observation", |b| {
//         b.iter(|| {
//             env.bench_individual_observations_req(2000);
//         });
//     });
// }

fn tw_low_contention_observation(c: &mut Criterion) {
    let env = WatchContentionEnvironment::new(5, 2);

    c.bench_function("tw_r5_w5_2000_observation", |b| {
        b.iter(|| {
            env.bench_individual_observations_req(2000);
        });
    });
}

criterion_group!(
    benches,
    // pl_rw_write,
    // ss_rw_write,
    // tw_write,
    // pl_rw_low_contention_write,
    // ss_rw_low_contention_write,
    // tw_low_contention_write,
    // pl_rw_observation,
    // ss_rw_observation,
    // tw_observation,
    // pl_rw_low_contention_observation,
    ss_rw_low_contention_observation,
    tw_low_contention_observation,
);
criterion_main!(benches);
