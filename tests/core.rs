use async_std::future::timeout;
use futures::executor::block_on;
use paste::paste;
use rand::random;
use sensor_fuse::{
    lock, prelude::*, sensor_core::SensorCoreAsync, DerefSensorData, SensorWriter, ShareStrategy,
};
use std::{sync::Arc, task::Waker, thread, time::Duration};

static REASONABLE_TIMEOUT_S: u64 = 5;

macro_rules! test_core_with_owned_observer {
    ($prefix:ident, $sensor_writer:ty) => {
        paste! {
            #[test]
            fn [<$prefix _closed>]() {
                test_closed::<$sensor_writer, _>();
            }
        }
    };
}

macro_rules! test_core {
    ($prefix:ident, $sensor_writer:ty) => {
        paste! {
            #[test]
            fn [<$prefix _sensor_observation>]() {
                test_sensor_observation::<$sensor_writer>();
            }

            #[test]
            fn [<$prefix _basic_sensor_observation_parallel_unsynced_10_1000>]() {
                test_basic_sensor_observation_parallel_unsynced::<$sensor_writer>(10, 1000);
            }

            #[test]
            fn [<$prefix _mapped_sensor_observation>]() {
                test_mapped_sensor_observation::<$sensor_writer>();
            }

            #[test]
            fn [<$prefix _fused_sensor_sensor_observation>]() {
                test_fused_sensor_observation::<$sensor_writer>();
            }

        }
    };
}

macro_rules! test_core_exec {
    ($prefix:ident, $sensor_writer:ty) => {
        paste! {
            #[test]
            fn [<$prefix _test_async_chainged>]() {
                test_async_changed::<$sensor_writer>();
            }

            #[test]
            fn [<$prefix _test_mapped_async_changed>]() {
                test_mapped_async_changed::<$sensor_writer>();
            }

            #[test]
            fn [<$prefix _test_fused_async_changed>]() {
                test_fused_async_changed::<$sensor_writer>();
            }

            #[test]
            fn [<$prefix _test_async_wait_for>]() {
                test_async_wait_for::<$sensor_writer>();
            }

            #[test]
            fn [<$prefix _test_mapped_async_wait_for>]() {
                test_mapped_async_wait_for::<$sensor_writer>();
            }

            #[test]
            fn [<$prefix _test_fused_async_wait_for>]() {
                test_fused_async_wait_for::<$sensor_writer>();
            }

            #[test]
            fn [<$prefix _test_callbacks>]() {
                test_callbacks::<$sensor_writer>();
            }
        }
    };
}

// fn test_sensor_observation<S>()
// where
//     for<'a> &'a S: ShareStrategy<'a, Target = usize>,
//     SensorWriter<usize, S>: From<usize>,
//     for<'a> <&'a S as ShareStrategy<'a>>::Shared: Clone,
// {
//     let sensor_writer = SensorWriter::<usize, S>::from(0);

//     let mut observers: Vec<Box<dyn SensorObserve<Lock = <&'_ S as ShareStrategy<'_>>::Lock>>> =
//         Vec::new();

//     let observer = sensor_writer.spawn_observer();
//     observers.push(Box::new(observer.clone()));
//     observers.push(Box::new(observer));
//     let ref_observer = sensor_writer.spawn_referenced_observer();
//     observers.push(Box::new(ref_observer.clone()));
//     observers.push(Box::new(ref_observer));

//     *sensor_writer.write() = 1;
//     for observer in &mut observers {
//         assert!(!observer.has_changed());
//         assert_eq!(*observer.borrow(), 1);
//     }

//     sensor_writer.mark_all_unseen();
//     for observer in &mut observers {
//         assert!(observer.has_changed());
//     }

//     sensor_writer.update(2);
//     for observer in &mut observers {
//         assert!(observer.has_changed());
//         assert_eq!(*observer.borrow(), 2);
//     }

//     sensor_writer.modify_with(|x| *x += 1);
//     for observer in &mut observers {
//         assert!(observer.has_changed());
//         assert_eq!(*observer.pull(), 3);
//     }

//     assert_eq!(*sensor_writer.read(), 3);
// }

// fn test_basic_sensor_observation_parallel_unsynced<S>(num_threads: usize, num_updates: usize)
// where
//     for<'a> &'a S: ShareStrategy<'a, Target = usize>,
//     for<'a> <&'a S as ShareStrategy<'a>>::Shared: Clone,
//     SensorWriter<usize, S>: From<usize> + Send + Sync + 'static,
// {
//     let sensor_writer = Arc::new(SensorWriter::<usize, S>::from(1));

//     let handles = Vec::from_iter((0..num_threads).map(|_| {
//         let sensor_writer = sensor_writer.clone();
//         thread::spawn(move || {
//             let mut sensor_observer = sensor_writer.spawn_observer();
//             let mut last_seen_value = 0;
//             sensor_observer.mark_unseen();

//             loop {
//                 if !sensor_observer.has_changed() {
//                     continue;
//                 }
//                 let new_value = *sensor_observer.pull();
//                 assert!(last_seen_value < new_value);
//                 if new_value == num_updates {
//                     break;
//                 }
//                 last_seen_value = new_value;
//             }
//         })
//     }));

//     for _ in 1..num_updates {
//         sensor_writer.modify_with(|x| *x += 1);
//     }

//     handles.into_iter().for_each(|h| {
//         h.join().unwrap();
//     });
// }

// fn test_mapped_sensor_observation<S>()
// where
//     for<'a> &'a S: ShareStrategy<'a, Target = usize>,
//     SensorWriter<usize, S>: From<usize>,
// {
//     let sensor_writer = SensorWriter::<usize, S>::from(0);
//     let mut observer = sensor_writer.spawn_observer().map(|x| x + 1);

//     assert_eq!(*observer.borrow(), 1);

//     sensor_writer.update(2);
//     assert!(observer.has_changed());

//     observer.mark_seen();
//     assert!(!observer.inner.has_changed());

//     observer.mark_unseen();
//     assert!(observer.inner.has_changed());

//     let cached = *observer.borrow();
//     assert_eq!(cached, 3);
//     let new = *observer.pull();
//     assert_eq!(new, 3);
// }

// fn test_fused_sensor_observation<S>()
// where
//     for<'a> &'a S: ShareStrategy<'a, Target = usize>,
//     SensorWriter<usize, S>: From<usize>,
// {
//     let sensor_writer_1 = SensorWriter::<usize, S>::from(1);
//     let sensor_writer_2 = SensorWriter::<usize, S>::from(2);

//     let mut observer = sensor_writer_1
//         .spawn_observer()
//         .fuse(sensor_writer_2.spawn_observer(), |x, y| x * y);

//     assert_eq!(*observer.borrow(), 2);

//     sensor_writer_1.update(2);
//     assert!(observer.has_changed());

//     let cached = *observer.borrow();
//     assert_eq!(cached, 4);
//     let new = *observer.pull();
//     assert_eq!(new, 4);

//     assert!(!observer.has_changed());

//     sensor_writer_2.update(3);
//     assert!(observer.has_changed());

//     observer.mark_seen();
//     assert!(!observer.a.has_changed());
//     assert!(!observer.b.has_changed());

//     observer.mark_unseen();
//     assert!(observer.a.has_changed());
//     assert!(observer.b.has_changed());

//     let cached = *observer.borrow();
//     assert_eq!(cached, 6);
//     let new = *observer.pull();
//     assert_eq!(new, 6);

//     assert!(!observer.has_changed());
// }

fn test_closed<S, R>()
where
    R: DerefSensorData<Target = usize>,
    for<'a> &'a S: ShareStrategy<'a, Target = usize, Shared = R>,
    SensorWriter<usize, S>: From<usize>,
{
    let sensor_writer = SensorWriter::<usize, S>::from(1);
    let observer = sensor_writer.spawn_observer();
    drop(sensor_writer);
    assert!(observer.is_closed());
    // assert_eq!(*observer.(), 1);
}

fn test_async_changed<S>()
where
    for<'a> &'a S: ShareStrategy<'a, Target = usize>,
    for<'a> <&'a S as ShareStrategy<'a>>::Shared: Send,
    for<'a> <&'a S as ShareStrategy<'a>>::Core: SensorCoreAsync,
    SensorWriter<usize, S>: 'static + Send + Sync + From<usize>,
{
    let ping_send = Arc::new(SensorWriter::<usize, S>::from(1));
    let pong_send = Arc::new(SensorWriter::<usize, S>::from(1));
    let mut ping_recv = ping_send.spawn_observer();
    let mut pong_recv = pong_send.spawn_observer();
    thread::scope(|s| {
        let handle = s.spawn({
            let pong_send = pong_send.clone();
            move || {
                for i in 2..1000 {
                    let _ = block_on(timeout(
                        Duration::from_secs(REASONABLE_TIMEOUT_S),
                        ping_recv.wait_until_changed(),
                    ))
                    .unwrap();

                    if *block_on(ping_recv.read()) != i {
                        pong_send.update(0);
                        panic!();
                    }

                    if random::<bool>() {
                        thread::sleep(Duration::from_millis(1));
                    }
                    pong_send.update(i);
                }
            }
        });

        for i in 2..1000 {
            if random::<bool>() {
                thread::sleep(Duration::from_millis(1));
            }
            ping_send.update(i);

            let _ = block_on(timeout(
                Duration::from_secs(REASONABLE_TIMEOUT_S),
                pong_recv.wait_until_changed(),
            ))
            .unwrap();

            if *block_on(pong_recv.read()) != i {
                ping_send.update(0);
                panic!();
            }
        }
        handle.join().unwrap();
    });
}

fn test_mapped_async_changed<S>()
where
    for<'a> &'a S: ShareStrategy<'a, Target = usize>,
    for<'a> <&'a S as ShareStrategy<'a>>::Shared: Send,
    for<'a> <&'a S as ShareStrategy<'a>>::Core: SensorCoreAsync,
    SensorWriter<usize, S>: 'static + Send + Sync + From<usize>,
{
    let ping_send = Arc::new(SensorWriter::<usize, S>::from(1));
    let pong_send = Arc::new(SensorWriter::<usize, S>::from(1));
    let mut ping_recv = ping_send.spawn_observer().map(|x| 2 * x);
    let mut pong_recv = pong_send.spawn_observer().map(|x| 2 * x);
    thread::scope(|s| {
        let handle = s.spawn({
            let pong_send = pong_send.clone();
            move || {
                for i in 2..1000 {
                    let _ = block_on(timeout(
                        Duration::from_secs(REASONABLE_TIMEOUT_S),
                        ping_recv.wait_until_changed(),
                    ))
                    .unwrap();

                    if *block_on(ping_recv.read()) != 2 * i {
                        pong_send.update(0);
                        panic!();
                    }

                    if random::<bool>() {
                        thread::sleep(Duration::from_millis(1));
                    }
                    pong_send.update(i);
                }
            }
        });

        for i in 2..1000 {
            if random::<bool>() {
                thread::sleep(Duration::from_millis(1));
            }
            ping_send.update(i);

            let _ = block_on(timeout(
                Duration::from_secs(REASONABLE_TIMEOUT_S),
                pong_recv.wait_until_changed(),
            ))
            .unwrap();

            if *block_on(pong_recv.read()) != 2 * i {
                ping_send.update(0);
                panic!();
            }
        }
        handle.join().unwrap();
    });
}

fn test_fused_async_changed<S>()
where
    for<'a> &'a S: ShareStrategy<'a, Target = usize>,
    for<'a> <&'a S as ShareStrategy<'a>>::Shared: Send,
    for<'a> <&'a S as ShareStrategy<'a>>::Core: SensorCoreAsync,
    SensorWriter<usize, S>: 'static + Send + Sync + From<usize>,
{
    let ping1_send = Arc::new(SensorWriter::<usize, S>::from(1));
    let ping2_send = Arc::new(SensorWriter::<usize, S>::from(1));
    let pong1_send = Arc::new(SensorWriter::<usize, S>::from(1));
    let pong2_send = Arc::new(SensorWriter::<usize, S>::from(1));
    let ping_recv = ping1_send
        .spawn_observer()
        .fuse(ping2_send.spawn_observer(), |x, y| *x + *y - 1);
    let pong_recv = pong1_send
        .spawn_observer()
        .fuse(pong2_send.spawn_observer(), |x, y| *x + *y - 1);
    thread::scope(|s| {
        let handle = s.spawn({
            let pong1_send = pong1_send.clone();
            let pong2_send = pong2_send.clone();
            move || {
                block_on(async {
                    for i in 2..1000 {
                        let _ = timeout(
                            Duration::from_secs(REASONABLE_TIMEOUT_S),
                            ping_recv.wait_until_changed(),
                        )
                        .await
                        .unwrap();

                        if *ping_recv.read().await != i {
                            pong1_send.update(0);
                            pong2_send.update(0);
                            panic!();
                        }

                        if random::<bool>() {
                            thread::sleep(Duration::from_millis(1));
                        }
                        if i % 2 == 0 {
                            pong1_send
                                .modify_with(|x| {
                                    *x += 1;
                                    true
                                })
                                .await;
                        } else {
                            pong2_send
                                .modify_with(|x| {
                                    *x += 1;
                                    true
                                })
                                .await;
                        }
                    }
                })
            }
        });
        block_on(async {
            for i in 2..1000 {
                if random::<bool>() {
                    thread::sleep(Duration::from_millis(1));
                }
                if i % 2 == 0 {
                    ping1_send
                        .modify_with(|x| {
                            *x += 1;
                            true
                        })
                        .await;
                } else {
                    ping2_send
                        .modify_with(|x| {
                            *x += 1;
                            true
                        })
                        .await;
                }

                let _ = timeout(
                    Duration::from_secs(REASONABLE_TIMEOUT_S),
                    pong_recv.wait_until_changed(),
                )
                .await
                .unwrap();

                if *pong_recv.read().await != i {
                    ping1_send.update(0).await;
                    ping2_send.update(0).await;
                    panic!();
                }
            }
            handle.join().unwrap();
        })
    });
}

// fn test_async_wait_for<S>()
// where
//     for<'a> &'a S: ShareStrategy<'a, Target = usize>,
//     for<'a> <&'a S as ShareStrategy<'a>>::Shared: Send,
//     for<'a> <&'a S as ShareStrategy<'a>>::Core: SensorCoreAsync,
//     SensorWriter<usize, S>: 'static + Send + Sync + From<usize>,
// {
//     let ping_send = Arc::new(SensorWriter::<usize, S>::from(1));
//     let pong_send = Arc::new(SensorWriter::<usize, S>::from(1));
//     let mut ping_recv = ping_send.spawn_observer();
//     let mut pong_recv = pong_send.spawn_observer();
//     thread::scope(|s| {
//         let handle = s.spawn({
//             let pong_send = pong_send.clone();
//             move || {
//                 for i in 2..1000 {
//                     let _ = block_on(timeout(
//                         Duration::from_secs(REASONABLE_TIMEOUT_S),
//                         ping_recv.wait_for(|x| *x == i),
//                     ))
//                     .unwrap();

//                     if random::<bool>() {
//                         thread::sleep(Duration::from_millis(1));
//                     }
//                     pong_send.update(i);
//                 }
//             }
//         });

//         for i in 2..1000 {
//             if random::<bool>() {
//                 thread::sleep(Duration::from_millis(1));
//             }
//             ping_send.update(i);

//             let _ = block_on(timeout(
//                 Duration::from_secs(REASONABLE_TIMEOUT_S),
//                 pong_recv.wait_for(|x| *x == i),
//             ))
//             .unwrap();
//         }
//         handle.join().unwrap();
//     });
// }

// fn test_mapped_async_wait_for<S>()
// where
//     for<'a> &'a S: ShareStrategy<'a, Target = usize>,
//     for<'a> <&'a S as ShareStrategy<'a>>::Shared: Send,
//     for<'a> <&'a S as ShareStrategy<'a>>::Core: SensorCoreAsync,
//     SensorWriter<usize, S>: 'static + Send + Sync + From<usize>,
// {
//     let ping_send = Arc::new(SensorWriter::<usize, S>::from(1));
//     let pong_send = Arc::new(SensorWriter::<usize, S>::from(1));
//     let mut ping_recv = ping_send.spawn_observer().map(|x| 2 * x);
//     let mut pong_recv = pong_send.spawn_observer().map(|x| 2 * x);
//     thread::scope(|s| {
//         let handle = s.spawn({
//             let pong_send = pong_send.clone();
//             move || {
//                 if random::<bool>() {
//                     thread::sleep(Duration::from_millis(1));
//                 }
//                 for i in 2..1000 {
//                     let _ = block_on(timeout(
//                         Duration::from_secs(REASONABLE_TIMEOUT_S),
//                         ping_recv.wait_for(|x| *x == 2 * i),
//                     ))
//                     .unwrap();

//                     pong_send.update(i);
//                 }
//             }
//         });

//         for i in 2..1000 {
//             if random::<bool>() {
//                 thread::sleep(Duration::from_millis(1));
//             }
//             ping_send.update(i);

//             let _ = block_on(timeout(
//                 Duration::from_secs(REASONABLE_TIMEOUT_S),
//                 pong_recv.wait_for(|x| *x == 2 * i),
//             ))
//             .unwrap();
//         }
//         handle.join().unwrap();
//     });
// }

// fn test_fused_async_wait_for<S>()
// where
//     for<'a> &'a S: ShareStrategy<'a, Target = usize>,
//     for<'a> <&'a S as ShareStrategy<'a>>::Shared: Send,
//     for<'a> <&'a S as ShareStrategy<'a>>::Core: SensorCoreAsync,
//     SensorWriter<usize, S>: 'static + Send + Sync + From<usize>,
// {
//     let ping1_send = Arc::new(SensorWriter::<usize, S>::from(1));
//     let ping2_send = Arc::new(SensorWriter::<usize, S>::from(1));
//     let pong1_send = Arc::new(SensorWriter::<usize, S>::from(1));
//     let pong2_send = Arc::new(SensorWriter::<usize, S>::from(1));
//     let mut ping_recv = ping1_send
//         .spawn_observer()
//         .fuse(ping2_send.spawn_observer(), |x, y| *x + *y - 1);
//     let mut pong_recv = pong1_send
//         .spawn_observer()
//         .fuse(pong2_send.spawn_observer(), |x, y| *x + *y - 1);
//     thread::scope(|s| {
//         let handle = s.spawn({
//             let pong1_send = pong1_send.clone();
//             let pong2_send = pong2_send.clone();
//             move || {
//                 for i in 2..1000 {
//                     let _ = block_on(timeout(
//                         Duration::from_secs(REASONABLE_TIMEOUT_S),
//                         ping_recv.wait_for(|x| *x == i),
//                     ))
//                     .unwrap();

//                     if random::<bool>() {
//                         thread::sleep(Duration::from_millis(1));
//                     }
//                     if i % 2 == 0 {
//                         pong1_send.modify_with(|x| *x += 1);
//                     } else {
//                         pong2_send.modify_with(|x| *x += 1);
//                     }
//                 }
//             }
//         });

//         for i in 2..1000 {
//             if random::<bool>() {
//                 thread::sleep(Duration::from_millis(1));
//             }
//             if i % 2 == 0 {
//                 ping1_send.modify_with(|x| *x += 1);
//             } else {
//                 ping2_send.modify_with(|x| *x += 1);
//             }

//             let _ = block_on(timeout(
//                 Duration::from_secs(REASONABLE_TIMEOUT_S),
//                 pong_recv.wait_for(|x| *x == i),
//             ))
//             .unwrap();
//         }
//         handle.join().unwrap();
//     });
// }

/*** parking_lot locks ***/

// test_core!(pl_rwl, lock::parking_lot::RwSensorData<_>);

// test_core!(pl_arc_rwl, lock::parking_lot::ArcRwSensorData<_>);
// test_core_with_owned_observer!(pl_arc_rwl, lock::parking_lot::ArcRwSensorData<_>);

// test_core!(pl_mtx, lock::parking_lot::MutexSensorData<_>);

// test_core!(pl_arc_mtx, lock::parking_lot::ArcMutexSensorData<_>);
// test_core_with_owned_observer!(pl_arc_mtx, lock::parking_lot::ArcMutexSensorData<_>);

// test_core!(pl_rwl_exec, lock::parking_lot::RwSensorDataExec<_>);
// test_core_exec!(pl_rwl_exec, lock::parking_lot::RwSensorDataExec<_>);

// test_core!(pl_arc_rwl_exec, lock::parking_lot::ArcRwSensorDataExec<_>);
// test_core_with_owned_observer!(
//     pl_arc_rwl_exec,
//     lock::parking_lot::ArcMutexSensorDataExec<_>
// );
// test_core_exec!(pl_arc_rwl_exec, lock::parking_lot::ArcRwSensorDataExec<_>);

// test_core!(pl_mtx_exec, lock::parking_lot::MutexSensorDataExec<_>);
// test_core_exec!(pl_mtx_exec, lock::parking_lot::MutexSensorDataExec<_>);

// test_core!(
//     pl_arc_mtx_exec,
//     lock::parking_lot::ArcMutexSensorDataExec<_>
// );
// test_core_with_owned_observer!(
//     pl_arc_mtx_exec,
//     lock::parking_lot::ArcMutexSensorDataExec<_>
// );
// test_core_exec!(
//     pl_arc_mtx_exec,
//     lock::parking_lot::ArcMutexSensorDataExec<_>
// );

// /*** std_sync locks ***/
// test_core!(ss_rwl, lock::std_sync::RwSensorData<_>);

// test_core!(ss_arc_rwl, lock::std_sync::ArcRwSensorData<_>);
// test_core_with_owned_observer!(ss_arc_rwl, lock::std_sync::ArcRwSensorData<_>);

// test_core!(ss_mtx, lock::std_sync::MutexSensorData<_>);

// test_core!(ss_arc_mtx, lock::std_sync::ArcMutexSensorData<_>);
// test_core_with_owned_observer!(ss_arc_mtx, lock::std_sync::ArcMutexSensorData<_>);

// test_core!(ss_rwl_exec, lock::std_sync::RwSensorDataExec<_>);
// test_core_exec!(ss_rwl_exec, lock::std_sync::RwSensorDataExec<_>);

// test_core!(ss_arc_rwl_exec, lock::std_sync::ArcRwSensorDataExec<_>);
// test_core_with_owned_observer!(ss_arc_rwl_exec, lock::std_sync::ArcMutexSensorDataExec<_>);
// test_core_exec!(ss_arc_rwl_exec, lock::std_sync::ArcRwSensorDataExec<_>);

// test_core!(ss_mtx_exec, lock::std_sync::MutexSensorDataExec<_>);
// test_core_exec!(ss_mtx_exec, lock::std_sync::MutexSensorDataExec<_>);

// test_core!(ss_arc_mtx_exec, lock::std_sync::ArcMutexSensorDataExec<_>);
// test_core_with_owned_observer!(ss_arc_mtx_exec, lock::std_sync::ArcMutexSensorDataExec<_>);
// test_core_exec!(ss_arc_mtx_exec, lock::std_sync::ArcMutexSensorDataExec<_>);
