use async_std::future::timeout;
use futures::executor::block_on;
use paste::paste;
use sensor_fuse::{
    executor::{BoxedFn, ExecManager, ExecRegister},
    lock::{self, DataWriteLock},
    prelude::*,
    DerefSensorData, RawSensorData, SensorWriter, ShareStrategy, SharedSensorData,
};
use std::{
    hint::black_box,
    ops::Deref,
    sync::{
        atomic::{compiler_fence, fence, Ordering},
        Arc,
    },
    task::Waker,
    thread,
    time::Duration,
};
use tokio::sync::watch;

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
            fn [<$prefix _basic_sensor_observation>]() {
                test_basic_sensor_observation::<$sensor_writer>();
            }

            #[test]
            fn [<$prefix _basic_sensor_observation_parallel_unsynced_10_1000>]() {
                test_basic_sensor_observation_parallel_unsynced::<$sensor_writer>(10, 1000);
            }

            #[test]
            fn [<$prefix _mapped_sensor>]() {
                test_mapped_sensor::<$sensor_writer>();
            }

            #[test]
            fn [<$prefix _fused_sensor_sensor>]() {
                test_fused_sensor::<$sensor_writer>();
            }

        }
    };
}

macro_rules! test_core_exec {
    ($prefix:ident, $sensor_writer:ty) => {
        paste! {
            #[test]
            fn [<$prefix _test_async_waiting>]() {
                test_async_waiting::<$sensor_writer>();
            }

            #[test]
            fn [<$prefix _test_callbacks>]() {
                test_callbacks::<$sensor_writer>();
            }
        }
    };
}

fn test_basic_sensor_observation<S>()
where
    S: SharedSensorData<usize>,
    for<'a> &'a S: ShareStrategy<'a, Data = RawSensorData<S::Lock, S::Executor>>,
    SensorWriter<usize, S>: From<usize>,
    for<'a> <&'a S as ShareStrategy<'a>>::Shared: Clone,
{
    let sensor_writer = SensorWriter::<usize, S>::from(0);

    let mut observers: Vec<Box<dyn SensorObserve<Lock = S::Lock>>> = Vec::new();

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

fn test_basic_sensor_observation_parallel_unsynced<S>(num_threads: usize, num_updates: usize)
where
    S: SharedSensorData<usize>,
    for<'a> &'a S: ShareStrategy<'a, Data = RawSensorData<S::Lock, S::Executor>>,
    SensorWriter<usize, S>: From<usize> + Send + Sync + 'static,
    for<'a> <&'a S as ShareStrategy<'a>>::Shared: Clone,
{
    let sensor_writer = Arc::new(SensorWriter::<usize, S>::from(1));

    let handles = Vec::from_iter((0..num_threads).map(|_| {
        let sensor_writer = sensor_writer.clone();
        thread::spawn(move || {
            let sensor_observer = sensor_writer.spawn_observer();
            let mut last_seen_value = 0;
            sensor_observer.mark_unseen();

            loop {
                if !sensor_observer.has_changed() {
                    continue;
                }
                let new_value = *sensor_observer.pull();
                assert!(last_seen_value < new_value);
                if new_value == num_updates {
                    break;
                }
                last_seen_value = new_value;
            }
        })
    }));

    for _ in 1..num_updates {
        sensor_writer.modify_with(|x| *x += 1);
    }

    handles.into_iter().for_each(|h| {
        h.join().unwrap();
    });
}

fn test_mapped_sensor<S>()
where
    S: SharedSensorData<usize>,
    for<'a> &'a S: ShareStrategy<'a, Data = RawSensorData<S::Lock, S::Executor>>,
    SensorWriter<usize, S>: From<usize>,
{
    let sensor_writer = SensorWriter::<usize, S>::from(0);
    let observer = sensor_writer.spawn_observer().map(|x| x + 1);

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

fn test_fused_sensor<S>()
where
    S: SharedSensorData<usize>,
    for<'a> &'a S: ShareStrategy<'a, Data = RawSensorData<S::Lock, S::Executor>>,
    SensorWriter<usize, S>: From<usize>,
{
    let sensor_writer_1 = SensorWriter::<usize, S>::from(1);
    let sensor_writer_2 = SensorWriter::<usize, S>::from(2);

    let observer = sensor_writer_1
        .spawn_observer()
        .fuse(sensor_writer_2.spawn_observer(), |x, y| x * y);

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

fn test_closed<S, R>()
where
    R: DerefSensorData<usize, Lock = S::Lock, Executor = S::Executor>,
    S: SharedSensorData<usize>,
    for<'a> &'a S: ShareStrategy<'a, Data = RawSensorData<S::Lock, S::Executor>, Shared = R>,
    SensorWriter<usize, S>: From<usize>,
{
    let sensor_writer = SensorWriter::<usize, S>::from(1);
    let observer = sensor_writer.spawn_observer();
    drop(sensor_writer);
    assert!(observer.is_closed());
    assert_eq!(*observer.borrow(), 1);
}

fn test_async_waiting<S>()
where
    S: SharedSensorData<usize>,
    for<'a> &'a S: ShareStrategy<'a, Data = RawSensorData<S::Lock, S::Executor>>,
    for<'a> S::Executor: ExecRegister<S::Lock, &'a Waker>,
    SensorWriter<usize, S>: 'static + Send + Sync + From<usize>,
{
    let sync_send = watch::Sender::new(());
    let ping_send = Arc::new(SensorWriter::<usize, S>::from(1));
    for _ in 0..50 {
        let ping_recv = ping_send.as_ref().spawn_observer();
        let mut sync_recv = sync_send.subscribe();
        sync_recv.mark_unchanged();
        let handle = thread::spawn({
            let ping_send = ping_send.clone();
            move || {
                for _ in 0..50 {
                    let _ = block_on(timeout(Duration::from_secs(1), sync_recv.changed())).unwrap();

                    fence(Ordering::SeqCst);
                    ping_send.update(5);

                    let _ = block_on(timeout(Duration::from_secs(1), sync_recv.changed())).unwrap();

                    fence(Ordering::SeqCst);
                    ping_send.modify_with(|x| *x += 1);
                }
            }
        });

        for _ in 0..50 {
            ping_recv.mark_seen();
            let unused = black_box(ping_recv.wait_until_changed());

            black_box(sync_send.send_replace(()));

            fence(Ordering::SeqCst);
            block_on(timeout(
                Duration::from_secs(1),
                ping_recv.wait_until_changed(),
            ))
            .unwrap()
            .unwrap();

            assert!(ping_recv.has_changed());
            assert_eq!(*ping_recv.pull(), 5);
            drop(unused);

            black_box(sync_send.send_replace(()));

            fence(Ordering::SeqCst);
            if block_on(timeout(
                Duration::from_secs(1),
                ping_recv.wait_for(|x| *x == 6),
            ))
            .unwrap()
            .is_err()
            {
                panic!();
            }

            assert!(!ping_recv.has_changed());
            assert_eq!(*ping_recv.borrow(), 6);
        }
        handle.join().unwrap();
    }
}

fn test_callbacks<S>()
where
    S: SharedSensorData<usize>,
    for<'a> &'a S: ShareStrategy<'a, Data = RawSensorData<S::Lock, S::Executor>>,
    S::Executor: ExecRegister<S::Lock, BoxedFn<usize>>,
    SensorWriter<usize, S>: 'static + Send + Sync + From<usize>,
{
    let sensor_writer = SensorWriter::<usize, S>::from(1);
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
