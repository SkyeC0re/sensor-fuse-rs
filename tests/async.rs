use paste::paste;
use sensor_fuse::{
    prelude::*,
    sensor_core::{alloc::AsyncCore, SensorCoreAsync},
    SensorWriter, ShareStrategy,
};
use std::{sync::Arc, task::Poll};

use wookie::wookie;

macro_rules! test_single_thread {
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

            #[test]
            fn [<$prefix _wait_changed>]() {
                test_wait_changed::<$sensor_writer>();
            }

            #[test]
            fn [<$prefix _wait_for>]() {
                test_wait_for::<$sensor_writer>();
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
    let writer = SensorWriter::<_, S>::from(0);
    let observer = writer.spawn_observer();

    assert!(!observer.has_changed());

    wookie!(read: writer.read());
    match read.poll() {
        Poll::Ready(v) => assert_eq!(*v, 0),
        Poll::Pending => panic!(),
    };

    wookie!(read: observer.read());
    match read.poll() {
        Poll::Ready(v) => assert_eq!(*v, 0),
        Poll::Pending => panic!(),
    };
}

fn test_write_read<S>()
where
    for<'a> &'a S: ShareStrategy<'a, Target = usize>,
    for<'a> <&'a S as ShareStrategy<'a>>::Core: SensorCoreAsync,
    SensorWriter<usize, S>: From<usize>,
{
    let writer = SensorWriter::<_, S>::from(0);
    let observer = writer.spawn_observer();

    wookie!(write: writer.write());

    match write.poll() {
        Poll::Ready(mut guard) => {
            assert_eq!(*guard, 0);
            *guard = 1;
        }
        Poll::Pending => panic!(),
    }

    assert!(!observer.has_changed());

    wookie!(read: writer.read());
    match read.poll() {
        Poll::Ready(v) => assert_eq!(*v, 1),
        Poll::Pending => panic!(),
    };

    wookie!(read: observer.read());
    match read.poll() {
        Poll::Ready(v) => assert_eq!(*v, 1),
        Poll::Pending => panic!(),
    };
}

fn test_modify<S>()
where
    for<'a> &'a S: ShareStrategy<'a, Target = usize>,
    for<'a> <&'a S as ShareStrategy<'a>>::Core: SensorCoreAsync,
    SensorWriter<usize, S>: From<usize>,
{
    let writer = SensorWriter::<_, S>::from(0);
    let observer = writer.spawn_observer();

    wookie!(modify1: writer
    .modify_with(|v| {
        *v += 1;
        false
    }));

    assert_eq!(modify1.poll(), Poll::Ready(false));
    assert!(!observer.has_changed());

    wookie!(modify2: writer
    .modify_with(|v| {
        *v += 1;
        true
    }));

    assert_eq!(modify2.poll(), Poll::Ready(true));
    assert!(observer.has_changed());
}

fn test_wait_changed<S>()
where
    for<'a> &'a S: ShareStrategy<'a, Target = usize>,
    for<'a> <&'a S as ShareStrategy<'a>>::Core: SensorCoreAsync,
    SensorWriter<usize, S>: From<usize>,
{
    let writer = SensorWriter::<_, S>::from(0);
    let observer = writer.spawn_observer();

    wookie!(wait_changed: observer.wait_until_changed());

    wookie!(modify1: writer
    .modify_with(|v| {
        *v += 1;
        false
    }));

    assert_eq!(modify1.poll(), Poll::Ready(false));
    assert!(!observer.has_changed());
    assert_eq!(wait_changed.woken(), 0);
    assert!(wait_changed.poll().is_pending());

    wookie!(modify2: writer
    .modify_with(|v| {
        *v += 1;
        true
    }));

    assert_eq!(modify2.poll(), Poll::Ready(true));
    assert!(observer.has_changed());
    assert_eq!(wait_changed.woken(), 1);
    assert!(wait_changed.poll().is_ready());
}

fn test_wait_for<S>()
where
    for<'a> &'a S: ShareStrategy<'a, Target = usize>,
    for<'a> <&'a S as ShareStrategy<'a>>::Core: SensorCoreAsync,
    SensorWriter<usize, S>: From<usize>,
{
    let writer = SensorWriter::<_, S>::from(0);
    let mut observer = writer.spawn_observer();

    wookie!(wait_for: observer.wait_for(|v| *v == 2, true));

    wookie!(modify1: writer
    .modify_with(|v| {
        *v += 1;
        false
    }));

    assert_eq!(modify1.poll(), Poll::Ready(false));
    assert_eq!(wait_for.woken(), 0);
    assert!(wait_for.poll().is_pending());

    wookie!(modify2: writer
    .modify_with(|v| {
        *v += 1;
        true
    }));

    assert_eq!(modify2.poll(), Poll::Ready(true));
    assert_eq!(wait_for.woken(), 1);
    assert!(wait_for.poll().is_ready());
}

test_single_thread!(arc_alloc_async, Arc<AsyncCore<_>>);
