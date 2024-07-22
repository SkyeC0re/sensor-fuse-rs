use std::sync::Arc;

use parking_lot;

use crate::{
    DataLockFactory, DataReadLock, DataWriteLock, ReadGuardSpecifier, RevisedData, SensorObserver,
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
