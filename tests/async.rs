use paste::paste;
use sensor_fuse::{
    prelude::*,
    sensor_core::{alloc::AsyncCore, SensorCoreAsync},
    SensorWriter, ShareStrategy,
};
use std::{ops::Deref, pin::Pin, sync::Arc, task::Poll};

use wookie::{wookie, Wookie};

macro_rules! test_single_thread {
    ($prefix:ident, $sensor_writer:ty) => {
        paste! {
            #[test]
            fn [<$prefix _read>]() {
                test_read::<_, $sensor_writer>();
            }

            #[test]
            fn [<$prefix _write_read>]() {
                test_write_read::<_, $sensor_writer>();
            }

            #[test]
            fn [<$prefix _modify>]() {
                test_modify::<_, $sensor_writer>();
            }

            #[test]
            fn [<$prefix _wait_changed>]() {
                test_wait_changed::<_, $sensor_writer>();
            }

            #[test]
            fn [<$prefix _wait_for>]() {
                test_wait_for::<_, $sensor_writer>();
            }

            #[test]
            fn [<$prefix _mapped_read>]() {
                test_mapped_read::<_, $sensor_writer>();
            }

            #[test]
            fn [<$prefix _mapped_wait_changed>]() {
                test_mapped_wait_changed::<_, $sensor_writer>();
            }

            #[test]
            fn [<$prefix _mapped_wait_for>]() {
                test_mapped_wait_for::<_, $sensor_writer>();
            }

            #[test]
            fn [<$prefix _fused_read>]() {
                test_fused_read::<_, $sensor_writer>();
            }

            #[test]
            fn [<$prefix _fused_wait_changed>]() {
                test_fused_wait_changed::<_, $sensor_writer>();
            }

            #[test]
            fn [<$prefix _fused_wait_for>]() {
                test_fused_wait_for::<_, $sensor_writer>();
            }
        }
    };
}

fn test_read<C, S>()
where
    C: SensorCoreAsync<Target = usize> + From<usize>,
    S: Deref<Target = C> + From<C>,
    for<'a> &'a S: ShareStrategy<'a, Core = C>,
{
    let writer = SensorWriter::from_value(0);
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

fn test_write_read<C, S>()
where
C: SensorCoreAsync<Target = usize> + From<usize>,
S: Deref<Target = C> + From<C>,
for<'a> &'a S: ShareStrategy<'a, Core = C>,
{
    let writer = SensorWriter::from_value(0);
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

fn test_modify<C, S>()
where
C: SensorCoreAsync<Target = usize> + From<usize>,
S: Deref<Target = C> + From<C>,
for<'a> &'a S: ShareStrategy<'a, Core = C>,
{
    let writer = SensorWriter::<_, S>::from_value(0);
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

fn test_wait_changed<C, S>()
where
C: SensorCoreAsync<Target = usize> + From<usize>,
S: Deref<Target = C> + From<C>,
for<'a> &'a S: ShareStrategy<'a, Core = C>,
{
    let writer = SensorWriter::from_value(0);
    let observer = writer.spawn_observer();

    wookie!(wait_changed: observer.wait_until_changed());
    assert!(wait_changed.poll().is_pending());

    wookie!(modify1: writer
    .modify_with(|v| {
        *v = 1;
        false
    }));

    assert_eq!(modify1.poll(), Poll::Ready(false));
    assert!(!observer.has_changed());
    assert_eq!(wait_changed.woken(), 0);
    assert!(wait_changed.poll().is_pending());

    wookie!(modify2: writer
    .modify_with(|v| {
        *v = 2;
        true
    }));

    assert_eq!(modify2.poll(), Poll::Ready(true));
    assert!(observer.has_changed());
    assert_eq!(wait_changed.woken(), 1);
    assert!(wait_changed.poll().is_ready());
}

fn test_wait_for<C, S>()
where
C: SensorCoreAsync<Target = usize> + From<usize>,
S: Deref<Target = C> + From<C>,
for<'a> &'a S: ShareStrategy<'a, Core = C>,
{
    let writer = SensorWriter::from_value(0);
    let mut observer = writer.spawn_observer();

    wookie!(wait_for: observer.wait_for(|v| *v == 2));
    assert!(wait_for.poll().is_pending());

    wookie!(modify1: writer
    .modify_with(|v| {
        *v = 1;
        true
    }));

    assert_eq!(modify1.poll(), Poll::Ready(true));
    assert_eq!(wait_for.woken(), 1);
    assert!(wait_for.poll().is_pending());

    wookie!(modify2: writer
    .modify_with(|v| {
        *v = 2;
        true
    }));

    assert_eq!(modify2.poll(), Poll::Ready(true));
    assert_eq!(wait_for.woken(), 2);
    match wait_for.poll() {
        Poll::Ready((v, status)) => {
            assert_eq!(*v, 2);
            assert!(status.success());
        }
        Poll::Pending => panic!(),
    };
}

fn test_mapped_read<C, S>()
where
C: SensorCoreAsync<Target = usize> + From<usize>,
S: Deref<Target = C> + From<C>,
for<'a> &'a S: ShareStrategy<'a, Core = C>,
{
    let writer = SensorWriter::from_value(0);
    let observer = writer.spawn_observer().map(|v| *v + 10);

    wookie!(write: writer.write());

    match write.poll() {
        Poll::Ready(mut guard) => {
            assert_eq!(*guard, 0);
            *guard = 1;
        }
        Poll::Pending => panic!(),
    }

    assert!(!observer.has_changed());

    wookie!(read: observer.read());
    match read.poll() {
        Poll::Ready(v) => assert_eq!(*v, 11),
        Poll::Pending => panic!(),
    };
}

fn test_mapped_wait_changed<C, S>()
where
C: SensorCoreAsync<Target = usize> + From<usize>,
S: Deref<Target = C> + From<C>,
for<'a> &'a S: ShareStrategy<'a, Core = C>,
{
    let writer = SensorWriter::from_value(0);
    let observer = writer.spawn_observer().map(|v| *v + 10);

    wookie!(wait_changed: observer.wait_until_changed());
    assert!(wait_changed.poll().is_pending());

    wookie!(modify1: writer
    .modify_with(|v| {
        *v = 1;
        false
    }));

    assert_eq!(modify1.poll(), Poll::Ready(false));
    assert!(!observer.has_changed());
    assert_eq!(wait_changed.woken(), 0);
    assert!(wait_changed.poll().is_pending());

    wookie!(modify2: writer
    .modify_with(|v| {
        *v = 2;
        true
    }));

    assert_eq!(modify2.poll(), Poll::Ready(true));
    assert!(observer.has_changed());
    assert_eq!(wait_changed.woken(), 1);
    assert!(wait_changed.poll().is_ready());
}

fn test_mapped_wait_for<C, S>()
where
C: SensorCoreAsync<Target = usize> + From<usize>,
S: Deref<Target = C> + From<C>,
for<'a> &'a S: ShareStrategy<'a, Core = C>,
{
    let writer = SensorWriter::<_, S>::from_value(0);
    let mut observer = writer.spawn_observer().map(|v| *v + 10);

    wookie!(wait_for: observer.wait_for(|v| *v == 12));
    assert!(wait_for.poll().is_pending());

    wookie!(modify1: writer
    .modify_with(|v| {
        *v = 1;
        true
    }));

    assert_eq!(modify1.poll(), Poll::Ready(true));
    assert_eq!(wait_for.woken(), 1);
    assert!(wait_for.poll().is_pending());

    wookie!(modify2: writer
    .modify_with(|v| {
        *v = 2;
        true
    }));

    assert_eq!(modify2.poll(), Poll::Ready(true));
    assert_eq!(wait_for.woken(), 2);

    match wait_for.poll() {
        Poll::Ready((v, status)) => {
            assert_eq!(*v, 12);
            assert!(status.success());
        }
        Poll::Pending => panic!(),
    };
}

fn test_fused_read<C, S>()
where
C: SensorCoreAsync<Target = usize> + From<usize>,
S: Deref<Target = C> + From<C>,
for<'a> &'a S: ShareStrategy<'a, Core = C>,
{
    let writer = SensorWriter::from_value(0);
    let observer = writer.spawn_observer().map(|v| *v + 10);

    wookie!(write: writer.write());

    match write.poll() {
        Poll::Ready(mut guard) => {
            assert_eq!(*guard, 0);
            *guard = 1;
        }
        Poll::Pending => panic!(),
    }

    assert!(!observer.has_changed());

    wookie!(read: observer.read());
    match read.poll() {
        Poll::Ready(v) => assert_eq!(*v, 11),
        Poll::Pending => panic!(),
    };
}

fn test_fused_wait_changed<C, S>()
where
C: SensorCoreAsync<Target = usize> + From<usize>,
S: Deref<Target = C> + From<C>,
for<'a> &'a S: ShareStrategy<'a, Core = C>,
{
    let writer1 = SensorWriter::from_value(0);
    let writer2 = SensorWriter::from_value(0);
    let mut observer = writer1
        .spawn_observer()
        .fuse(writer2.spawn_observer(), |x, y| *x + *y);

    let mut wait_changed_data = Wookie::new(observer.wait_until_changed());
    let mut wait_changed = unsafe { Pin::new_unchecked(&mut wait_changed_data) };
    assert!(wait_changed.poll().is_pending());

    wookie!(modify1: writer1
    .modify_with(|v| {
        *v = 1;
        false
    }));

    wookie!(modify2: writer2
    .modify_with(|v| {
        *v = 1;
        false
    }));

    assert_eq!(modify1.poll(), Poll::Ready(false));
    assert_eq!(modify2.poll(), Poll::Ready(false));
    assert!(!observer.has_changed());
    assert_eq!(wait_changed.woken(), 0);
    assert!(wait_changed.poll().is_pending());

    wookie!(modify1: writer1
    .modify_with(|v| {
        *v = 2;
        true
    }));

    assert_eq!(modify1.poll(), Poll::Ready(true));
    assert!(observer.has_changed());
    assert_eq!(wait_changed.woken(), 1);
    assert!(wait_changed.poll().is_ready());
    drop(wait_changed_data);

    observer.mark_seen();

    let mut wait_changed_data = Wookie::new(observer.wait_until_changed());
    let mut wait_changed = unsafe { Pin::new_unchecked(&mut wait_changed_data) };
    assert!(wait_changed.poll().is_pending());

    wookie!(modify2: writer2
    .modify_with(|v| {
        *v = 2;
        true
    }));

    assert_eq!(modify2.poll(), Poll::Ready(true));
    assert!(observer.has_changed());
    assert_eq!(wait_changed.woken(), 1);
    assert!(wait_changed.poll().is_ready());
}

fn test_fused_wait_for<C, S>()
where
C: SensorCoreAsync<Target = usize> + From<usize>,
S: Deref<Target = C> + From<C>,
for<'a> &'a S: ShareStrategy<'a, Core = C>,
{
    let writer1 = SensorWriter::from_value(0);
    let writer2 = SensorWriter::from_value(0);
    let mut observer = writer1
        .spawn_observer()
        .fuse(writer2.spawn_observer(), |x, y| *x + *y);

    let mut wait_for_data = Wookie::new(observer.wait_for(|v| *v > 2));
    let mut wait_for = unsafe { Pin::new_unchecked(&mut wait_for_data) };
    assert!(wait_for.poll().is_pending());

    wookie!(modify1: writer1
    .modify_with(|v| {
        *v = 1;
        false
    }));

    wookie!(modify2: writer2
    .modify_with(|v| {
        *v = 1;
        false
    }));

    assert_eq!(modify1.poll(), Poll::Ready(false));
    assert_eq!(modify2.poll(), Poll::Ready(false));
    assert_eq!(wait_for.woken(), 0);
    assert!(wait_for.poll().is_pending());

    wookie!(modify1: writer1
    .modify_with(|v| {
        *v = 2;
        true
    }));

    assert_eq!(modify1.poll(), Poll::Ready(true));
    assert_eq!(wait_for.woken(), 1);

    match wait_for.poll() {
        Poll::Ready((v, status)) => {
            assert_eq!(*v, 3);
            assert!(status.success());
        }
        Poll::Pending => panic!(),
    };

    drop(wait_for);
    drop(wait_for_data);

    observer.mark_seen();

    let mut wait_for_data = Wookie::new(observer.wait_for(|v| *v > 3));
    let mut wait_for = unsafe { Pin::new_unchecked(&mut wait_for_data) };
    assert!(wait_for.poll().is_pending());

    wookie!(modify2: writer2
    .modify_with(|v| {
        *v = 2;
        true
    }));

    assert_eq!(modify2.poll(), Poll::Ready(true));
    assert_eq!(wait_for.woken(), 1);

    match wait_for.poll() {
        Poll::Ready((v, status)) => {
            assert_eq!(*v, 4);
            assert!(status.success());
        }
        Poll::Pending => panic!(),
    };
}

test_single_thread!(arc_alloc_async, Arc<AsyncCore<_>>);
