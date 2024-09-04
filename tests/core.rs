use async_std::future::timeout;
use futures::executor::block_on;
use paste::paste;
use sensor_fuse::{
    executor::{ExecutionStrategy, RegistrationStrategy},
    lock::{self, DataWriteLock},
    prelude::*,
    RevisedData, SensorWriter, ShareStrategy,
};
use std::ops::Deref;
use std::task::Waker;
use std::time::Duration;
use std::{sync::Arc, thread};
use tokio::sync::watch;

macro_rules! test_core_with_owned_observer {
    ($prefix:ident, $sensor_writer:ty) => {
        paste! {
            #[test]
            fn [<$prefix _closed>]() {
                test_closed::<$sensor_writer, _, _, _>();
            }
        }
    };
}

macro_rules! test_core {
    ($prefix:ident, $sensor_writer:ty) => {
        paste! {
            #[test]
            fn [<$prefix _basic_sensor_observation>]() {
                test_basic_sensor_observation::<$sensor_writer, _, _>();
            }

            #[test]
            fn [<$prefix _basic_sensor_observation_parallel_synced_10_1000>]() {
                test_basic_sensor_observation_parallel_synced::<$sensor_writer, _, _>(10, 1000);
            }

            #[test]
            fn [<$prefix _basic_sensor_observation_parallel_unsynced_10_1000>]() {
                test_basic_sensor_observation_parallel_unsynced::<$sensor_writer, _, _>(10, 1000);
            }

            #[test]
            fn [<$prefix _mapped_sensor>]() {
                test_mapped_sensor::<$sensor_writer, _, _>();
            }

            #[test]
            fn [<$prefix _mapped_sensor_cached>]() {
                test_mapped_sensor_cached::<$sensor_writer, _, _>();
            }

            #[test]
            fn [<$prefix _fused_sensor_sensor>]() {
                test_fused_sensor::<$sensor_writer, _, _>();
            }

            #[test]
            fn [<$prefix _fused_sensor_sensor_cached>]() {
                test_fused_sensor_cached::<$sensor_writer, _, _>();
            }
        }
    };
}

macro_rules! test_core_exec {
    ($prefix:ident, $sensor_writer:ty) => {
        paste! {
            #[test]
            fn [<$prefix _test_async_waiting>]() {
                test_async_waiting::<$sensor_writer, _, _>();
            }

            #[test]
            fn [<$prefix _test_callbacks>]() {
                test_callbacks::<$sensor_writer, _, _>();
            }
        }
    };
}

fn test_basic_sensor_observation<S, L, E>()
where
    for<'a> &'a S: ShareStrategy<'a, Target = (L, E)>,
    L: DataWriteLock<Target = usize>,
    E: ExecutionStrategy<usize>,
    SensorWriter<usize, S, L, E>: From<usize>,
    for<'a> <&'a S as ShareStrategy<'a>>::Shared: Clone,
{
    let sensor_writer = SensorWriter::<usize, S, L, E>::from(0);

    let mut observers: Vec<Box<dyn SensorObserve<Lock = L>>> = Vec::new();

    let observer = sensor_writer.spawn_observer();
    observers.push(Box::new(observer.clone()));
    observers.push(Box::new(observer));
    let ref_observer = sensor_writer.spawn_referenced_observer();
    observers.push(Box::new(ref_observer.clone()));
    observers.push(Box::new(ref_observer));

    *sensor_writer.write() = 1;
    for observer in &mut observers {
        assert!(!observer.has_changed());
        assert_eq!(*observer.borrow(), 1);
    }

    sensor_writer.mark_all_unseen();
    for observer in &mut observers {
        assert!(observer.has_changed());
    }

    sensor_writer.update(2);
    for observer in &mut observers {
        assert!(observer.has_changed());
        assert_eq!(*observer.borrow(), 2);
    }

    sensor_writer.modify_with(|x| *x += 1);
    for observer in &mut observers {
        assert!(observer.has_changed());
        assert_eq!(*observer.pull(), 3);
    }

    assert_eq!(*sensor_writer.read(), 3);
}

fn test_basic_sensor_observation_parallel_synced<S, L, E>(num_threads: usize, num_updates: usize)
where
    for<'a> &'a S: ShareStrategy<'a, Target = (L, E)>,
    L: DataWriteLock<Target = usize>,
    E: ExecutionStrategy<usize>,
    SensorWriter<usize, S, L, E>: From<usize> + Send + Sync + 'static,
    for<'a> <&'a S as ShareStrategy<'a>>::Shared: Clone,
{
    let sync = Arc::new((
        parking_lot::Mutex::new(Some(0)),
        parking_lot::Condvar::new(),
        parking_lot::Condvar::new(),
    ));

    let sensor_writer = Arc::new(SensorWriter::<usize, S, L, E>::from(0));

    let handles = Vec::from_iter((0..num_threads).map(|_| {
        let sensor_writer_clone = sensor_writer.clone();

        let sync_clone = sync.clone();
        thread::spawn(move || {
            let mut sensor_observer = sensor_writer_clone.spawn_observer();
            sensor_observer.mark_unseen();
            let (mutex, observer_start, writer_start) = &*sync_clone;
            for i in 0..num_updates {
                let mut guard = mutex.lock();
                if !sensor_observer.has_changed() {
                    *guard = None;
                    drop(guard);
                    writer_start.notify_one();
                    panic!("Value change not registered.");
                }

                if *sensor_observer.pull() != i {
                    *guard = None;
                    drop(guard);
                    writer_start.notify_one();
                    panic!("Unexpected value found.");
                }

                if let Some(mut count) = *guard {
                    count += 1;
                    *guard = Some(count);
                    if count == num_threads {
                        writer_start.notify_one();
                    }
                }
                observer_start.wait(&mut guard);
            }
            drop(sensor_observer);
        })
    }));

    let (mutex, observer_start, writer_start) = &*sync;
    for i in 1..=num_updates {
        let mut guard = mutex.lock();
        if let Some(count) = *guard {
            if count != num_threads {
                writer_start.wait(&mut guard);
            }
        } else {
            observer_start.notify_all();
            panic!();
        }
        *guard = Some(0);
        drop(guard);
        sensor_writer.update(i);
        assert_eq!(observer_start.notify_all(), num_threads);
    }

    handles.into_iter().for_each(|h| {
        h.join().unwrap();
    });
}

fn test_basic_sensor_observation_parallel_unsynced<S, L, E>(num_threads: usize, num_updates: usize)
where
    for<'a> &'a S: ShareStrategy<'a, Target = (L, E)>,
    L: DataWriteLock<Target = usize>,
    E: ExecutionStrategy<usize>,
    SensorWriter<usize, S, L, E>: From<usize> + Send + Sync + 'static,
    for<'a> <&'a S as ShareStrategy<'a>>::Shared: Clone,
{
    let sensor_writer = Arc::new(SensorWriter::<usize, S, L, E>::from(0));

    let handles = Vec::from_iter((0..num_threads).map(|_| {
        let sensor_writer_clone = sensor_writer.clone();
        thread::spawn(move || {
            let mut sensor_observer = sensor_writer_clone.spawn_observer();
            let mut last_seen_value = 0;
            sensor_observer.mark_unseen();

            loop {
                if !sensor_observer.has_changed() {
                    continue;
                }
                let new_value = *sensor_observer.borrow();
                assert!(last_seen_value <= new_value);
                if new_value == num_updates {
                    break;
                }
                last_seen_value = new_value;
            }
        })
    }));

    for _ in 0..num_updates {
        sensor_writer.modify_with(|x| *x += 1);
    }

    handles.into_iter().for_each(|h| {
        h.join().unwrap();
    });
}

fn test_mapped_sensor<S, L, E>()
where
    for<'a> &'a S: ShareStrategy<'a, Target = (L, E)>,
    L: DataWriteLock<Target = usize>,
    E: ExecutionStrategy<usize>,
    SensorWriter<usize, S, L, E>: From<usize>,
{
    let sensor_writer = SensorWriter::<usize, S, L, E>::from(0);
    let mut observer = sensor_writer.spawn_observer().map(|x| x + 1);

    assert!(!observer.is_cached());
    assert_eq!(*observer.borrow(), 1);

    sensor_writer.update(2);
    assert!(observer.has_changed());

    observer.mark_seen();
    assert!(!observer.inner.has_changed());

    observer.mark_unseen();
    assert!(observer.inner.has_changed());

    let cached = *observer.borrow();
    assert_eq!(cached, 3);
    let new = *observer.pull();
    assert_eq!(new, 3);
}

fn test_mapped_sensor_cached<S, L, E>()
where
    for<'a> &'a S: ShareStrategy<'a, Target = (L, E)>,
    L: DataWriteLock<Target = usize>,
    E: ExecutionStrategy<usize>,
    SensorWriter<usize, S, L, E>: From<usize>,
{
    let sensor_writer = SensorWriter::<usize, S, L, E>::from(0);
    let mut observer = sensor_writer.spawn_observer().map_cached(|x| x + 1);

    assert!(observer.is_cached());
    assert_eq!(*observer.borrow(), 1);

    sensor_writer.update(2);
    assert!(observer.has_changed());

    observer.mark_seen();
    assert!(!observer.inner.has_changed());

    observer.mark_unseen();
    assert!(observer.inner.has_changed());

    let cached = *observer.borrow();
    assert_eq!(cached, 1);
    let new = *observer.pull();
    assert_eq!(new, 3);
}

fn test_fused_sensor<S, L, E>()
where
    for<'a> &'a S: ShareStrategy<'a, Target = (L, E)>,
    L: DataWriteLock<Target = usize>,
    E: ExecutionStrategy<usize>,
    SensorWriter<usize, S, L, E>: From<usize>,
{
    let sensor_writer_1 = SensorWriter::<usize, S, L, E>::from(1);
    let sensor_writer_2 = SensorWriter::<usize, S, L, E>::from(2);

    let mut observer = sensor_writer_1
        .spawn_observer()
        .fuse(sensor_writer_2.spawn_observer(), |x, y| x * y);

    assert!(!observer.is_cached());
    assert_eq!(*observer.borrow(), 2);

    sensor_writer_1.update(2);
    assert!(observer.has_changed());

    let cached = *observer.borrow();
    assert_eq!(cached, 4);
    let new = *observer.pull();
    assert_eq!(new, 4);

    assert!(!observer.has_changed());

    sensor_writer_2.update(3);
    assert!(observer.has_changed());

    observer.mark_seen();
    assert!(!observer.a.has_changed());
    assert!(!observer.b.has_changed());

    observer.mark_unseen();
    assert!(observer.a.has_changed());
    assert!(observer.b.has_changed());

    let cached = *observer.borrow();
    assert_eq!(cached, 6);
    let new = *observer.pull();
    assert_eq!(new, 6);

    assert!(!observer.has_changed());
}

fn test_fused_sensor_cached<S, L, E>()
where
    for<'a> &'a S: ShareStrategy<'a, Target = (L, E)>,
    L: DataWriteLock<Target = usize>,
    E: ExecutionStrategy<usize>,
    SensorWriter<usize, S, L, E>: From<usize>,
{
    let sensor_writer_1 = Arc::new(SensorWriter::<usize, S, L, E>::from(1));
    let sensor_writer_2 = Arc::new(SensorWriter::<usize, S, L, E>::from(2));

    let mut observer = sensor_writer_1
        .spawn_observer()
        .fuse_cached(sensor_writer_2.spawn_observer(), |x, y| x * y);

    assert!(observer.is_cached());
    assert_eq!(*observer.borrow(), 2);

    sensor_writer_1.update(2);
    assert!(observer.has_changed());

    let cached = *observer.borrow();
    assert_eq!(cached, 2);
    let new = *observer.pull();
    assert_eq!(new, 4);

    assert!(!observer.has_changed());

    sensor_writer_2.update(3);
    assert!(observer.has_changed());

    observer.mark_seen();
    assert!(!observer.a.has_changed());
    assert!(!observer.b.has_changed());

    observer.mark_unseen();
    assert!(observer.a.has_changed());
    assert!(observer.b.has_changed());

    let cached = *observer.borrow();
    assert_eq!(cached, 4);
    let new = *observer.pull();
    assert_eq!(new, 6);

    assert!(!observer.has_changed());
}

fn test_closed<S, R, L, E>()
where
    R: Deref<Target = RevisedData<(L, E)>>,
    for<'a> &'a S: ShareStrategy<'a, Target = (L, E), Shared = R>,
    L: DataWriteLock<Target = usize>,
    E: ExecutionStrategy<usize>,
    SensorWriter<usize, S, L, E>: From<usize>,
{
    let sensor_writer = SensorWriter::<usize, S, L, E>::from(1);
    let mut observer = sensor_writer.spawn_observer();
    drop(sensor_writer);
    assert!(observer.is_closed());
    assert_eq!(*observer.borrow(), 1);
}

fn test_async_waiting<S, L, E>()
where
    for<'a> &'a S: ShareStrategy<'a, Target = (L, E)>,
    L: DataWriteLock<Target = usize>,
    for<'a> E: ExecutionStrategy<usize> + RegistrationStrategy<&'a Waker>,
    SensorWriter<usize, S, L, E>: 'static + Send + Sync + From<usize>,
{
    let sync_send = watch::Sender::new(());
    let mut sync_recv = sync_send.subscribe();
    let sensor_writer = Arc::new(SensorWriter::<usize, S, L, E>::from(1));

    let sensor_writer_clone = sensor_writer.clone();
    let handle = thread::spawn(move || {
        let _ = block_on(timeout(Duration::from_secs(1), sync_recv.changed()))
            .expect("Timeout waiting for initial confirmation");

        sensor_writer_clone.update(5);

        let _ = block_on(timeout(Duration::from_secs(1), sync_recv.changed()))
            .expect("Timeout waiting for secondary confirmation");

        sensor_writer_clone.modify_with(|x| *x += 1);
    });

    let mut observer = sensor_writer.as_ref().spawn_observer();
    sync_send.send_replace(());
    block_on(timeout(
        Duration::from_secs(1),
        observer.wait_until_changed(),
    ))
    .expect("Timeout occured waiting for first update.");

    assert!(observer.has_changed());
    assert_eq!(*observer.pull(), 5);

    sync_send.send_replace(());
    block_on(timeout(
        Duration::from_secs(1),
        observer.wait_for(|x| *x == 6),
    ))
    .expect("Timeout occured waiting for second update.");

    assert!(!observer.has_changed());
    assert_eq!(*observer.borrow(), 6);

    handle.join().unwrap();
}

fn test_callbacks<S, L, E>()
where
    for<'a> &'a S: ShareStrategy<'a, Target = (L, E)>,
    L: DataWriteLock<Target = usize>,
    E: RegistrationStrategy<Box<dyn Send + FnMut(&usize) -> bool>> + ExecutionStrategy<usize>,
    SensorWriter<usize, S, L, E>: 'static + Send + Sync + From<usize>,
{
    let sensor_writer = SensorWriter::<usize, S, L, E>::from(1);
    let writer_callback_state = Arc::new(parking_lot::Mutex::new(1));
    let observer_callback_state = Arc::new(parking_lot::Mutex::new(1));

    let writer_callback_state_clone = writer_callback_state.clone();
    sensor_writer.register(Box::new(move |x| {
        let mut guard = writer_callback_state_clone.as_ref().lock();
        if *guard == 0 {
            return false;
        }

        *guard = *x;
        return true;
    }));
    let observer = sensor_writer.spawn_observer();
    let observer_callback_state_clone = observer_callback_state.clone();
    observer.register(Box::new(move |x| {
        let mut guard = observer_callback_state_clone.as_ref().lock();
        if *guard == 0 {
            return false;
        }

        *guard = *x;

        return true;
    }));

    sensor_writer.update(2);
    assert_eq!(*writer_callback_state.as_ref().lock(), 2);
    assert_eq!(*observer_callback_state.as_ref().lock(), 2);

    *sensor_writer.write() = 3;
    assert_eq!(*writer_callback_state.as_ref().lock(), 2);
    assert_eq!(*observer_callback_state.as_ref().lock(), 2);

    sensor_writer.mark_all_unseen();
    assert_eq!(*writer_callback_state.as_ref().lock(), 3);
    assert_eq!(*observer_callback_state.as_ref().lock(), 3);

    sensor_writer.modify_with(|x| *x += 1);
    assert_eq!(*writer_callback_state.as_ref().lock(), 4);
    assert_eq!(*observer_callback_state.as_ref().lock(), 4);

    sensor_writer.update(2);
    assert_eq!(*writer_callback_state.as_ref().lock(), 2);
    assert_eq!(*observer_callback_state.as_ref().lock(), 2);

    *sensor_writer.write() = 3;
    assert_eq!(*writer_callback_state.as_ref().lock(), 2);
    assert_eq!(*observer_callback_state.as_ref().lock(), 2);

    sensor_writer.mark_all_unseen();
    assert_eq!(*writer_callback_state.as_ref().lock(), 3);
    assert_eq!(*observer_callback_state.as_ref().lock(), 3);

    sensor_writer.modify_with(|x| *x += 1);
    assert_eq!(*writer_callback_state.as_ref().lock(), 4);
    assert_eq!(*observer_callback_state.as_ref().lock(), 4);

    *writer_callback_state.as_ref().lock() = 0;
    *observer_callback_state.as_ref().lock() = 0;
    sensor_writer.update(2);
    assert_eq!(*writer_callback_state.as_ref().lock(), 0);
    assert_eq!(*observer_callback_state.as_ref().lock(), 0);
}

/*** parking_lot locks ***/

test_core!(pl_rwl, lock::parking_lot::RwSensorData<_>);

test_core!(pl_arc_rwl, lock::parking_lot::ArcRwSensorData<_>);
test_core_with_owned_observer!(pl_arc_rwl, lock::parking_lot::ArcRwSensorData<_>);

test_core!(pl_mtx, lock::parking_lot::MutexSensorData<_>);

test_core!(pl_arc_mtx, lock::parking_lot::ArcMutexSensorData<_>);
test_core_with_owned_observer!(pl_arc_mtx, lock::parking_lot::ArcMutexSensorData<_>);

test_core!(pl_rwl_exec, lock::parking_lot::RwSensorDataExec<_>);
test_core_exec!(pl_rwl_exec, lock::parking_lot::RwSensorDataExec<_>);

test_core!(pl_arc_rwl_exec, lock::parking_lot::ArcRwSensorDataExec<_>);
test_core_with_owned_observer!(
    pl_arc_rwl_exec,
    lock::parking_lot::ArcMutexSensorDataExec<_>
);
test_core_exec!(pl_arc_rwl_exec, lock::parking_lot::ArcRwSensorDataExec<_>);

test_core!(pl_mtx_exec, lock::parking_lot::MutexSensorDataExec<_>);
test_core_exec!(pl_mtx_exec, lock::parking_lot::MutexSensorDataExec<_>);

test_core!(
    pl_arc_mtx_exec,
    lock::parking_lot::ArcMutexSensorDataExec<_>
);
test_core_with_owned_observer!(
    pl_arc_mtx_exec,
    lock::parking_lot::ArcMutexSensorDataExec<_>
);
test_core_exec!(
    pl_arc_mtx_exec,
    lock::parking_lot::ArcMutexSensorDataExec<_>
);

/*** std_sync locks ***/

test_core!(ss_rwl, lock::std_sync::RwSensorData<_>);

test_core!(ss_arc_rwl, lock::std_sync::ArcRwSensorData<_>);
test_core_with_owned_observer!(ss_arc_rwl, lock::std_sync::ArcRwSensorData<_>);

test_core!(ss_mtx, lock::std_sync::MutexSensorData<_>);

test_core!(ss_arc_mtx, lock::std_sync::ArcMutexSensorData<_>);
test_core_with_owned_observer!(ss_arc_mtx, lock::std_sync::ArcMutexSensorData<_>);

test_core!(ss_rwl_exec, lock::std_sync::RwSensorDataExec<_>);
test_core_exec!(ss_rwl_exec, lock::std_sync::RwSensorDataExec<_>);

test_core!(ss_arc_rwl_exec, lock::std_sync::ArcRwSensorDataExec<_>);
test_core_with_owned_observer!(ss_arc_rwl_exec, lock::std_sync::ArcMutexSensorDataExec<_>);
test_core_exec!(ss_arc_rwl_exec, lock::std_sync::ArcRwSensorDataExec<_>);

test_core!(ss_mtx_exec, lock::std_sync::MutexSensorDataExec<_>);
test_core_exec!(ss_mtx_exec, lock::std_sync::MutexSensorDataExec<_>);

test_core!(ss_arc_mtx_exec, lock::std_sync::ArcMutexSensorDataExec<_>);
test_core_with_owned_observer!(ss_arc_mtx_exec, lock::std_sync::ArcMutexSensorDataExec<_>);
test_core_exec!(ss_arc_mtx_exec, lock::std_sync::ArcMutexSensorDataExec<_>);
