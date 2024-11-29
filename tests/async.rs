use futures::executor::block_on;
use paste::paste;
use sensor_fuse::{
    prelude::*,
    sensor_core::{alloc::AsyncCore, SensorCoreAsync},
    SensorWriter, ShareStrategy,
};
use std::sync::Arc;

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
        *writer.write().await = 1;
        assert_eq!(*writer.read().await, 1);
    });
}

test_basic!(arc_alloc_async, Arc<AsyncCore<_>>);
