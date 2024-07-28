use std::{sync::Arc, thread};

use sensor_fuse::{
    lock::parking_lot::{ArcRwSensorWriter, RwSensorWriterExec, SensorCallbackRegister},
    SensorCallbackExec, SensorObserve, SensorWrite,
};

#[test]
fn basic_sensor_observation() {
    let sync = Arc::new((parking_lot::Mutex::new(()), parking_lot::Condvar::new()));
    let sensor_writer = ArcRwSensorWriter::new(0);

    let mut sensor_observer = sensor_writer.spawn_observer();
    let sync_clone = sync.clone();
    let handle = thread::spawn(move || {
        for i in 0..10 {
            let mut guard = sync_clone.0.lock();
            sync_clone.1.wait(&mut guard);
            assert!(sensor_observer.has_changed());
            assert_eq!(*sensor_observer.pull_updated(), i);
            drop(guard);
        }
    });

    for i in 1..10 {
        sensor_writer.update(i);
        sync.1.notify_all();
    }
}
