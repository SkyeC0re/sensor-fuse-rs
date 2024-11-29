use futures::executor::block_on;
use paste::paste;
use sensor_fuse::{
    prelude::*,
    sensor_core::{alloc::AsyncCore, SensorCoreAsync},
    SensorWriter, ShareStrategy,
};
use std::{sync::Arc, thread};

macro_rules! test_basic {
    ($prefix:ident, $sensor_writer:ty) => {
        paste! {
            #[test]
            fn [<$prefix _read>]() {
                test_read::<$sensor_writer>();
            }

            #[test]
            fn [<$prefix _write_read>]() {
                test_write_read::<$sensor_writer>();
            }

            #[test]
            fn [<$prefix _modify>]() {
                test_modify::<$sensor_writer>();
            }
        }
    };
}

fn test_read<S>()
where
    for<'a> &'a S: ShareStrategy<'a, Target = usize>,
    for<'a> <&'a S as ShareStrategy<'a>>::Core: SensorCoreAsync,
    SensorWriter<usize, S>: From<usize>,
{
    block_on(async {
        let writer = SensorWriter::<usize, S>::from(0);
        assert_eq!(*writer.read().await, 0);
    });
}

fn test_write_read<S>()
where
    for<'a> &'a S: ShareStrategy<'a, Target = usize>,
    for<'a> <&'a S as ShareStrategy<'a>>::Core: SensorCoreAsync,
    SensorWriter<usize, S>: From<usize>,
{
    block_on(async {
        let writer = SensorWriter::<usize, S>::from(0);
        let observer = writer.spawn_observer();

        *writer.write().await = 1;
        assert!(!observer.has_changed());
        assert_eq!(*observer.read().await, 1);
    });
}

fn test_modify<S>()
where
    for<'a> &'a S: ShareStrategy<'a, Target = usize>,
    for<'a> <&'a S as ShareStrategy<'a>>::Core: SensorCoreAsync,
    SensorWriter<usize, S>: From<usize>,
{
    block_on(async {
        let writer = SensorWriter::<usize, S>::from(0);
        let observer = writer.spawn_observer();

        writer
            .modify_with(|v| {
                *v += 1;
                false
            })
            .await;
        assert!(!observer.has_changed());
        assert_eq!(*observer.read().await, 1);

        writer
            .modify_with(|v| {
                *v += 1;
                true
            })
            .await;
        assert!(observer.has_changed());
        assert_eq!(*observer.read().await, 2);
    });
}

test_basic!(arc_alloc_async, Arc<AsyncCore<_>>);
