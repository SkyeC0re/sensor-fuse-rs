use paste::paste;
use sensor_fuse::{
    prelude::*, sensor_core::alloc::AsyncCore, DerefSensorData, SensorWriter, ShareStrategy,
};
use std::sync::Arc;

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
            fn [<$prefix _change_observation>]() {
                test_change_observation::<$sensor_writer>();
            }

            #[test]
            fn [<$prefix _try_read_write>]() {
                test_try_read_write::<$sensor_writer>();
            }

        }
    };
}

fn test_closed<S, R>()
where
    R: DerefSensorData<Target = usize>,
    for<'a> &'a S: ShareStrategy<'a, Target = usize, Shared = R>,
    SensorWriter<usize, S>: From<usize>,
{
    let writer = SensorWriter::<usize, S>::from(0);
    let observer = writer.spawn_observer();
    drop(writer);
    assert!(observer.is_closed());
}

fn test_change_observation<S>()
where
    for<'a> &'a S: ShareStrategy<'a, Target = usize>,
    SensorWriter<usize, S>: From<usize>,
{
    let writer = SensorWriter::<usize, S>::from(0);
    let observer = writer.spawn_observer();
    assert!(!observer.has_changed());
    writer.mark_all_unseen();
    assert!(observer.has_changed());
}

fn test_try_read_write<S>()
where
    for<'a> &'a S: ShareStrategy<'a, Target = usize>,
    SensorWriter<usize, S>: From<usize>,
{
    let writer = SensorWriter::<usize, S>::from(0);
    let write_guard = writer.try_write().unwrap();
    assert!(writer.try_read().is_none());
    drop(write_guard);
    assert!(writer.try_read().is_some());
}

test_core!(arc_alloc_async, Arc<AsyncCore<_>>);
test_core_with_owned_observer!(arc_alloc_async, Arc<AsyncCore<_>>);
