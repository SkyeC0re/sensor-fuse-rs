use paste::paste;
use sensor_fuse::{
    prelude::*,
    sensor_core::{alloc::AsyncCore, no_alloc::AsyncSingleCore, SensorCore},
    SensorWriter, ShareStrategy,
};
use std::sync::Arc;

macro_rules! test_core {
    ($prefix:ident, $core:ty) => {
        paste! {
            #[test]
            fn [<$prefix _change_observation>]() {
                test_change_observation::<_, $core>();
            }

            #[test]
            fn [<$prefix _try_read_write>]() {
                test_try_read_write::<_, $core>();
            }

            #[test]
            fn [<$prefix _asdclosedd>]() {
                test_closed::<_, Arc<$core>>();
            }
        }
    };
}

fn test_closed<C, S>()
where
    C: SensorCore<Target = usize> + From<usize>,
    S: From<C>,
    // Require that observers may outlast the last writer by requiring that the lifetime of the shared data
    // is independent of the lifetime with which the writer is borrowed to create an observer.
    for<'a> &'a S: ShareStrategy<'a, Core = C, Shared = Arc<C>>,
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

test_core!(alloc_async, AsyncCore<_>);

test_core!(no_alloc_async, AsyncSingleCore<_>);
