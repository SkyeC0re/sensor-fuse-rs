use paste::paste;
use sensor_fuse::lock::DataWriteLock;
use sensor_fuse::prelude::*;
use sensor_fuse::{lock, Lockshare, SensorWriter};
use std::{
    cell::OnceCell,
    sync::{Arc, OnceLock},
    thread,
};

// macro_rules! test_all_with_sensor_writer {
//     ($prefix:ident, $sensor_writer:ty) => {
//         paste! {
//             #[test]
//             fn [<$prefix _basic_sensor_observation_synced_10_10>]() {
//                 test_basic_sensor_observation_synced!($sensor_writer, 10, 10);
//             }
//         }
//     };
// }

// #[test]
// fn pl_basic_sensor_observation_synced() {
//     test_basic_sensor_observation_synced<>(10, 10);
// }

fn test_basic_sensor_observation_synced<
    'share,
    R: Lockshare<Lock = L> + Send + Sync,
    L: DataWriteLock<Target = usize> + 'static,
>(
    num_threads: usize,
    num_updates: usize,
) where
    SensorWriter<R, L>: 'static + Send + Sync + From<usize>,
{
    let sync = Arc::new((
        parking_lot::Mutex::new(0),
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
                    *guard += 1;
                    drop(guard);
                    writer_start.notify_one();
                    panic!("Value change not registered.");
                }

                if *sensor_observer.pull_updated() != i {
                    *guard += 1;
                    drop(guard);
                    writer_start.notify_one();
                    panic!("Unexpected value found.");
                }

                *guard += 1;
                if *guard == num_threads {
                    assert!(writer_start.notify_one());
                }
                observer_start.wait(&mut guard);
            }
            drop(sensor_observer);
        })
    }));

    let (mutex, observer_start, writer_start) = &*sync;
    for i in 1..=num_updates {
        let mut guard = mutex.lock();
        writer_start.wait(&mut guard);
        sensor_writer.update(i);
        assert_eq!(*guard, num_threads);
        *guard = 0;
        drop(guard);
        assert_eq!(observer_start.notify_all(), num_threads);
    }

    handles.into_iter().for_each(|h| {
        h.join().unwrap();
    });
}

// test_all_with_sensor_writer!(pl_rwl, lock::parking_lot::ArcRwSensorWriter<usize>);
// test_all_with_sensor_writer!(pl_mtx, lock::parking_lot::MutexSensorWriter<usize>);
