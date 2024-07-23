use std::sync::Arc;

use parking_lot::{self, RwLockWriteGuard};

use crate::{
    CallbackManager, DataLockFactory, DataReadLock, DataWriteLock, ExecData, ExecGuard, ExecLock,
    Lockshare, ReadGuardSpecifier, RevisedData, SensorCallbackWrite, SensorObserver, SensorWrite,
    SensorWriter,
};

pub type RwSensorWriter<'share, T> =
    SensorWriter<'share, 'share, RevisedData<parking_lot::RwLock<T>>>;
// pub type RwSensorWriterExec<'share, T> =
//     SensorWriterExec<'share, 'share, RevisedData<parking_lot::RwLock<T>>>;
pub type RwSensor<T> = SensorObserver<parking_lot::RwLock<T>>;

pub type MutexSensorWriter<'share, T> =
    SensorWriter<'share, 'share, RevisedData<parking_lot::Mutex<T>>>;
pub type MutexSensor<T> = SensorObserver<parking_lot::Mutex<T>>;

pub type ArcRwSensorWriter<'share, 'state, T> =
    SensorWriter<'share, 'state, Arc<RevisedData<parking_lot::RwLock<T>>>>;
pub type ArcRwSensor<T> = SensorObserver<Arc<RevisedData<parking_lot::RwLock<T>>>>;

impl<T> ReadGuardSpecifier for parking_lot::RwLock<T> {
    type Target = T;
    type ReadGuard<'read> = parking_lot::RwLockReadGuard<'read, T> where T: 'read;
}
impl<T> DataReadLock for parking_lot::RwLock<T> {
    #[inline(always)]
    fn read(&self) -> Self::ReadGuard<'_> {
        self.read()
    }

    #[inline(always)]
    fn try_read(&self) -> Option<Self::ReadGuard<'_>> {
        self.try_read()
    }
}

impl<T> DataWriteLock for parking_lot::RwLock<T> {
    type WriteGuard<'write> = parking_lot::RwLockWriteGuard<'write, T> where T: 'write;

    #[inline(always)]
    fn write(&self) -> Self::WriteGuard<'_> {
        self.write()
    }

    #[inline(always)]
    fn try_write(&self) -> Option<Self::WriteGuard<'_>> {
        self.try_write()
    }
}

impl<'share, 'state, R, T, E> SensorCallbackWrite for SensorWriter<'share, 'state, R>
where
    R: Lockshare<'share, 'state, Lock = ExecLock<parking_lot::RwLock<ExecData<T, E>>, T, E>>,
    E: CallbackManager<Target = T>,
{
    fn update_exec(&self, sample: T) {
        let mut guard = self.share_elided_ref().data.write();
        *guard = sample;
        self.mark_all_unseen();
        let guard = ExecGuard {
            inner: RwLockWriteGuard::downgrade(guard.inner),
            _types: std::marker::PhantomData,
        };

        // Atomic downgrade just occured. No other modication call could be at this point.
        unsafe { (*guard.inner.exec_manager.get()).callback(&guard.inner.data) };
    }

    fn modify_with_exec(&self, f: impl FnOnce(&mut <Self::Lock as ReadGuardSpecifier>::Target)) {
        let mut guard = self.share_elided_ref().data.write();
        f(&mut guard);
        self.mark_all_unseen();
        let guard = ExecGuard {
            inner: RwLockWriteGuard::downgrade(guard.inner),
            _types: std::marker::PhantomData,
        };

        // Atomic downgrade just occured. No other modification will happen.
        unsafe {
            (*guard.inner.exec_manager.get()).callback(&guard.inner.data);
        };
    }

    fn exec(&self) {
        let guard = ExecGuard {
            inner: RwLockWriteGuard::downgrade(self.share_elided_ref().data.write().inner),
            _types: std::marker::PhantomData,
        };

        // Atomic downgrade just occured. No other modification will happen.
        unsafe { (*guard.inner.exec_manager.get()).callback(&guard.inner.data) };
    }

    fn register<F: 'static + FnMut(&<Self::Lock as ReadGuardSpecifier>::Target) -> bool>(
        &self,
        f: F,
    ) {
        self.share_elided_ref()
            .data
            .write()
            .inner
            .exec_manager
            .get_mut()
            .register(f);
    }
}

impl<T> DataLockFactory for parking_lot::RwLock<T> {
    type Lock = Self;

    #[inline(always)]
    fn new(init: T) -> Self::Lock {
        Self::new(init)
    }
}

impl<T> DataWriteLock for parking_lot::Mutex<T> {
    type WriteGuard<'write> = parking_lot::MutexGuard<'write, T> where T: 'write;
    #[inline(always)]
    fn write(&self) -> Self::WriteGuard<'_> {
        self.lock()
    }

    #[inline(always)]
    fn try_write(&self) -> Option<Self::WriteGuard<'_>> {
        self.try_lock()
    }
}

impl<T> ReadGuardSpecifier for parking_lot::Mutex<T> {
    type Target = T;
    type ReadGuard<'read> = parking_lot::MutexGuard<'read, T> where T: 'read;
}

impl<T> DataReadLock for parking_lot::Mutex<T> {
    #[inline(always)]
    fn read(&self) -> Self::ReadGuard<'_> {
        self.lock()
    }

    #[inline(always)]
    fn try_read(&self) -> Option<Self::ReadGuard<'_>> {
        self.try_lock()
    }
}

impl<T> DataLockFactory for parking_lot::Mutex<T> {
    type Lock = Self;

    #[inline(always)]
    fn new(init: T) -> Self::Lock {
        Self::new(init)
    }
}
