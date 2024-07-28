use sensor_fuse::lock::parking_lot::ArcRwSensorWriter;
use sensor_fuse::prelude::*;
use std::{sync::Arc, thread};

#[test]
fn basic_sensor_observation_synced_10_10() {
    test_basic_sensor_observation_synced(10, 10);
}

#[track_caller]
fn test_basic_sensor_observation_synced(num_threads: usize, num_updates: usize) {
    let sync = Arc::new((
        parking_lot::Mutex::new(0),
        parking_lot::Condvar::new(),
        parking_lot::Condvar::new(),
    ));
    let sensor_writer = ArcRwSensorWriter::new(0);

    let handles = Vec::from_iter((0..num_threads).map(|_| {
        let mut sensor_observer = sensor_writer.spawn_observer();
        sensor_observer.mark_unseen();
        let sync_clone = sync.clone();
        thread::spawn(move || {
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
