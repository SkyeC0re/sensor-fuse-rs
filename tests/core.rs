use paste::paste;
use sensor_fuse::{
    prelude::*, sensor_core::{alloc::AsyncCore, SensorCore}, SensorWriter, ShareStrategy,
};
use std::{ops::Deref, sync::Arc};

macro_rules! test_core_with_owned_observer {
    ($prefix:ident, $sensor_writer:ty) => {
        paste! {
            #[test]
            fn [<$prefix _closed>]() {
                test_closed::<_, $sensor_writer, _>();
            }
        }
    };
}

macro_rules! test_core {
    ($prefix:ident, $sensor_writer:ty) => {
        paste! {
            #[test]
            fn [<$prefix _change_observation>]() {
                test_change_observation::<_, $sensor_writer>();
            }

            #[test]
            fn [<$prefix _try_read_write>]() {
                test_try_read_write::<_, $sensor_writer>();
            }

        }
    };
}

fn test_closed<C, S, R>()
where
    C: SensorCore<Target = usize> + From<usize>,
    S: From<C>,
    R: Deref<Target = C>,
    // Require that observers may outlast the last writer by requiring that the lifetime of the shared data
    // is independent of the lifetime with which the writer is borrowed to create an observer.
    for<'a> &'a S: ShareStrategy<'a, Core = C, Shared = R>,
{
    let writer = SensorWriter::from_value(0);
    let observer = writer.spawn_observer();
    drop(writer);
    assert!(observer.is_closed());
}

fn test_change_observation<C, S>()
where
C: SensorCore<Target = usize> + From<usize>,
S: From<C>,
for<'a> &'a S: ShareStrategy<'a, Core = C>,
{
    let writer = SensorWriter::from_value(0);
    let observer = writer.spawn_observer();
    assert!(!observer.has_changed());
    writer.mark_all_unseen();
    assert!(observer.has_changed());
}

fn test_try_read_write<C, S>()
where
C: SensorCore<Target = usize> + From<usize>,
S: From<C>,
for<'a> &'a S: ShareStrategy<'a, Core = C>,
{
    let writer = SensorWriter::from_value(0);
    let write_guard = writer.try_write().unwrap();
    assert!(writer.try_read().is_none());
    drop(write_guard);
    assert!(writer.try_read().is_some());
}

test_core!(arc_alloc_async, Arc<AsyncCore<_>>);
test_core_with_owned_observer!(arc_alloc_async, Arc<AsyncCore<_>>);
