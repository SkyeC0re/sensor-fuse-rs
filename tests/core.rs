use paste::paste;
use sensor_fuse::lock::{self, DataWriteLock};
use sensor_fuse::{prelude::*, SensorObserver};
use sensor_fuse::{Lockshare, SensorWriter};
use std::{sync::Arc, thread};

macro_rules! test_core {
    ($prefix:ident, $sensor_writer:ty) => {
        paste! {

            #[test]
            fn [<$prefix _basic_sensor_observation_synced_10_1000>]() {
                test_basic_sensor_observation_synced::<$sensor_writer>(10, 1000);
            }

            #[test]
            fn [<$prefix _basic_sensor_observation_unsynced_10_1000>]() {
                test_basic_sensor_observation_unsynced::<$sensor_writer>(10, 1000);
            }

            #[test]
            fn [<$prefix _mapped_sensor>]() {
                test_mapped_sensor::<$sensor_writer>();
            }

            #[test]
            fn [<$prefix _mapped_sensor_cached>]() {
                test_mapped_sensor_cached::<$sensor_writer>();
            }

            #[test]
            fn [<$prefix _fused_sensor_sensor>]() {
                test_fused_sensor::<$sensor_writer>();
            }

            #[test]
            fn [<$prefix _fused_sensor_sensor_cached>]() {
                test_fused_sensor_cached::<$sensor_writer>();
            }
        }
    };
}

macro_rules! test_core_with_owned_observer {
    ($prefix:ident, $sensor_writer:ty) => {
        paste! {
            #[test]
            fn [<$prefix _closed>]() {
                test_closed::<$sensor_writer>();
            }
        }
    };
}

fn test_basic_sensor_observation_synced<R: Lockshare + Send + Sync>(
    num_threads: usize,
    num_updates: usize,
) where
    R::Lock: DataWriteLock<Target = usize> + 'static,
    SensorWriter<R, R::Lock>: 'static + Send + Sync + From<usize>,
{
    let sync = Arc::new((
        parking_lot::Mutex::new(Some(0)),
        parking_lot::Condvar::new(),
        parking_lot::Condvar::new(),
    ));

    let sensor_writer = Arc::new(SensorWriter::from(0));

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

                if *sensor_observer.pull_updated() != i {
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

fn test_basic_sensor_observation_unsynced<R: Lockshare + Send + Sync>(
    num_threads: usize,
    num_updates: usize,
) where
    R::Lock: DataWriteLock<Target = usize> + 'static,
    SensorWriter<R, R::Lock>: 'static + Send + Sync + From<usize>,
{
    let sensor_writer = Arc::new(SensorWriter::from(0));

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
                let new_value = *sensor_observer.pull();
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

fn test_mapped_sensor<R: Lockshare + Send + Sync>()
where
    R::Lock: DataWriteLock<Target = usize> + 'static,
    SensorWriter<R, R::Lock>: 'static + Send + Sync + From<usize>,
{
    let sensor_writer = SensorWriter::from(0);
    let mut observer = sensor_writer.spawn_observer().map(|x| x + 1);

    assert!(!observer.is_cached());
    assert_eq!(*observer.pull(), 1);

    sensor_writer.update(2);
    assert!(observer.has_changed());

    let cached = *observer.pull();
    assert_eq!(cached, 3);
    let new = *observer.pull_updated();
    assert_eq!(new, 3);
}

fn test_mapped_sensor_cached<R: Lockshare + Send + Sync>()
where
    R::Lock: DataWriteLock<Target = usize> + 'static,
    SensorWriter<R, R::Lock>: 'static + Send + Sync + From<usize>,
{
    let sensor_writer = SensorWriter::from(0);
    let mut observer = sensor_writer.spawn_observer().map_cached(|x| x + 1);

    assert!(observer.is_cached());
    assert_eq!(*observer.pull(), 1);

    sensor_writer.update(2);
    assert!(observer.has_changed());

    let cached = *observer.pull();
    assert_eq!(cached, 1);
    let new = *observer.pull_updated();
    assert_eq!(new, 3);
}

fn test_fused_sensor<R: Lockshare + Send + Sync>()
where
    R::Lock: DataWriteLock<Target = usize> + 'static,
    SensorWriter<R, R::Lock>: 'static + Send + Sync + From<usize>,
{
    let sensor_writer_1 = SensorWriter::from(1);
    let sensor_writer_2 = SensorWriter::from(2);

    let mut observer = sensor_writer_1
        .spawn_observer()
        .fuse(sensor_writer_2.spawn_observer(), |x, y| x * y);

    assert!(!observer.is_cached());
    assert_eq!(*observer.pull(), 2);

    sensor_writer_1.update(2);
    assert!(observer.has_changed());

    let cached = *observer.pull();
    assert_eq!(cached, 4);
    let new = *observer.pull_updated();
    assert_eq!(new, 4);

    assert!(!observer.has_changed());

    sensor_writer_2.update(3);
    assert!(observer.has_changed());

    let cached = *observer.pull();
    assert_eq!(cached, 6);
    let new = *observer.pull_updated();
    assert_eq!(new, 6);

    assert!(!observer.has_changed());
}

fn test_fused_sensor_cached<R: Lockshare + Send + Sync>()
where
    R::Lock: DataWriteLock<Target = usize> + 'static,
    SensorWriter<R, R::Lock>: 'static + Send + Sync + From<usize>,
{
    let sensor_writer_1 = Arc::new(SensorWriter::from(1));
    let sensor_writer_2 = Arc::new(SensorWriter::from(2));

    let mut observer = sensor_writer_1
        .spawn_observer()
        .fuse_cached(sensor_writer_2.spawn_observer(), |x, y| x * y);

    assert!(observer.is_cached());
    assert_eq!(*observer.pull(), 2);

    sensor_writer_1.update(2);
    assert!(observer.has_changed());

    let cached = *observer.pull();
    assert_eq!(cached, 2);
    let new = *observer.pull_updated();
    assert_eq!(new, 4);

    assert!(!observer.has_changed());

    sensor_writer_2.update(3);
    assert!(observer.has_changed());

    let cached = *observer.pull();
    assert_eq!(cached, 4);
    let new = *observer.pull_updated();
    assert_eq!(new, 6);

    assert!(!observer.has_changed());
}

fn test_closed<R: Lockshare + Send + Sync>()
where
    R::Lock: DataWriteLock<Target = usize> + 'static,
    SensorWriter<R, R::Lock>: 'static + Send + Sync + From<usize>,
    for<'a> R::Shared<'a>: 'static,
{
    let sensor_writer = SensorWriter::from(1);
    let mut observer = sensor_writer.spawn_observer();
    drop(sensor_writer);
    assert!(observer.is_closed());
    assert_eq!(*observer.pull(), 1);
}

test_core!(pl_rwl, lock::parking_lot::RwSensorData<_>);
test_core!(pl_arc_rwl, Arc<lock::parking_lot::RwSensorData<_>>);
test_core_with_owned_observer!(pl_arc_rwl, Arc<lock::parking_lot::RwSensorData<_>>);
test_core!(pl_mtx, lock::parking_lot::MutexSensorData<_>);
test_core!(pl_arc_mtx, Arc<lock::parking_lot::MutexSensorData<_>>);
test_core_with_owned_observer!(pl_arc_mtx, Arc<lock::parking_lot::MutexSensorData<_>>);
