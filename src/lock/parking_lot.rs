use std::{cell::UnsafeCell, marker::PhantomData, sync::Arc};

use parking_lot::{self, RwLockWriteGuard};

use crate::{
    callback_manager::{standard::VecBoxManager, ExecGuard},
    CallbackManager, DataReadLock, DataWriteLock, ExecData, ExecLock, Lockshare,
    ReadGuardSpecifier, RevisedData, SensorCallbackExec, SensorObserver, SensorWrite, SensorWriter,
};

use super::{AbstractArcSensorWriter, AbstractSensorWriter};

pub type RwSensorWriter<T> = AbstractSensorWriter<parking_lot::RwLock<T>>;
impl<T> RwSensorWriter<T> {
    #[inline(always)]
    pub const fn new(init: T) -> Self {
        SensorWriter(RevisedData::new(parking_lot::RwLock::new(init)))
    }
}

impl<T> From<T> for RwSensorWriter<T> {
    #[inline(always)]
    fn from(value: T) -> Self {
        Self::new(value)
    }
}
pub type RwSensorWriterExec<T> = AbstractSensorWriter<
    ExecLock<parking_lot::RwLock<ExecData<T, VecBoxManager<T>>>, T, VecBoxManager<T>>,
>;

impl<T> RwSensorWriterExec<T> {
    pub const fn new(init: T) -> Self {
        SensorWriter(RevisedData::new(ExecLock::new(parking_lot::RwLock::new(
            ExecData {
                exec_manager: UnsafeCell::new(VecBoxManager::new()),
                data: init,
            },
        ))))
    }
}
pub type RwSensor<'a, T> =
    SensorObserver<&'a RevisedData<parking_lot::RwLock<T>>, parking_lot::RwLock<T>>;
pub type RwSensorExec<'state, T> = SensorObserver<
    &'state RevisedData<
        ExecLock<parking_lot::RwLock<ExecData<T, VecBoxManager<T>>>, T, VecBoxManager<T>>,
    >,
    ExecLock<parking_lot::RwLock<ExecData<T, VecBoxManager<T>>>, T, VecBoxManager<T>>,
>;

pub type MutexSensorWriter<T> = AbstractSensorWriter<parking_lot::Mutex<T>>;

impl<T> MutexSensorWriter<T> {
    pub fn new(init: T) -> Self {
        SensorWriter(RevisedData::new(parking_lot::Mutex::new(init)))
    }
}

pub type MutexSensor<'a, T> =
    SensorObserver<&'a RevisedData<parking_lot::Mutex<T>>, parking_lot::Mutex<T>>;

pub type ArcRwSensorWriter<T> = AbstractArcSensorWriter<parking_lot::RwLock<T>>;

impl<T> ArcRwSensorWriter<T> {
    pub fn new(init: T) -> Self {
        SensorWriter(Arc::new(RevisedData::new(parking_lot::RwLock::new(init))))
    }
}
impl<T> From<T> for ArcRwSensorWriter<T> {
    #[inline(always)]
    fn from(value: T) -> Self {
        Self::new(value)
    }
}
pub type ArcRwSensor<T> =
    SensorObserver<Arc<RevisedData<parking_lot::RwLock<T>>>, parking_lot::RwLock<T>>;

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

impl<R, T, E> SensorCallbackExec<T>
    for SensorWriter<R, ExecLock<parking_lot::RwLock<ExecData<T, E>>, T, E>>
where
    R: Lockshare<Lock = ExecLock<parking_lot::RwLock<ExecData<T, E>>, T, E>>,
    E: CallbackManager<T>,
{
    fn update_exec(&self, sample: T) {
        let mut guard = self.share_elided_ref().write();
        *guard = sample;
        self.mark_all_unseen();
        let guard = ExecGuard::new(RwLockWriteGuard::downgrade(guard.inner));

        // Atomic downgrade just occured. No other modication can happen.
        unsafe { (*guard.inner.exec_manager.get()).callback(&guard.inner) };
    }

    fn modify_with_exec(&self, f: impl FnOnce(&mut T)) {
        let mut guard = self.share_elided_ref().write();
        f(&mut guard);
        self.mark_all_unseen();
        let guard = ExecGuard::new(RwLockWriteGuard::downgrade(guard.inner));

        // Atomic downgrade just occured. No other modification can happen.
        unsafe {
            (*guard.inner.exec_manager.get()).callback(&guard.inner);
        };
    }

    fn exec(&self) {
        let guard = ExecGuard::new(RwLockWriteGuard::downgrade(
            self.share_elided_ref().write().inner,
        ));

        // Atomic downgrade just occured. No other modification can happen.
        unsafe { (*guard.inner.exec_manager.get()).callback(&guard.inner) };
    }
}

impl<R, T, E> SensorCallbackExec<T>
    for SensorWriter<R, ExecLock<parking_lot::Mutex<ExecData<T, E>>, T, E>>
where
    R: Lockshare<Lock = ExecLock<parking_lot::Mutex<ExecData<T, E>>, T, E>>,
    E: CallbackManager<T>,
{
    fn update_exec(&self, sample: T) {
        let mut guard = self.share_elided_ref().write();
        *guard = sample;
        self.mark_all_unseen();

        let ExecData { exec_manager, data } = &mut *guard.inner;
        exec_manager.get_mut().callback(data);
    }

    fn modify_with_exec(&self, f: impl FnOnce(&mut T)) {
        let mut guard = self.share_elided_ref().write();
        f(&mut guard);
        self.mark_all_unseen();

        let ExecData { exec_manager, data } = &mut *guard.inner;
        exec_manager.get_mut().callback(data);
    }

    fn exec(&self) {
        let mut guard = self.share_elided_ref().write();

        let ExecData { exec_manager, data } = &mut *guard.inner;
        exec_manager.get_mut().callback(data);
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
