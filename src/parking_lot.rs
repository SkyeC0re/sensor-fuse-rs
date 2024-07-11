use std::sync::Arc;

use parking_lot;

use crate::{
    DataLockFactory, DataReadLock, DataWriteLock, ReadGuardSpecifier, RevisedData, Sensor,
    SensorWriter,
};

pub type RwSensorWriter<'share, T> =
    SensorWriter<'share, 'share, RevisedData<parking_lot::RwLock<T>>>;
pub type RwSensor<T> = Sensor<parking_lot::RwLock<T>>;

pub type MutexSensorWriter<'share, T> =
    SensorWriter<'share, 'share, RevisedData<parking_lot::Mutex<T>>>;
pub type MutexSensor<T> = Sensor<parking_lot::Mutex<T>>;

pub type ArcRwSensorWriter<'share, 'state, T> =
    SensorWriter<'share, 'state, Arc<RevisedData<parking_lot::RwLock<T>>>>;
pub type ArcRwSensor<T> = Sensor<Arc<RevisedData<parking_lot::RwLock<T>>>>;

impl<T> RwSensorWriter<'_, T> {
    #[inline(always)]
    pub fn new(init: T) -> Self {
        Self::new_from::<parking_lot::RwLock<T>>(init)
    }
}

impl<T> MutexSensorWriter<'_, T> {
    #[inline(always)]
    pub fn new(init: T) -> Self {
        Self::new_from::<parking_lot::Mutex<T>>(init)
    }
}

impl<'state, T: 'state> ArcRwSensorWriter<'_, 'state, T> {
    #[inline(always)]
    pub fn new(init: T) -> Self {
        Self::new_from::<parking_lot::RwLock<T>>(init)
    }
}

impl<T> ReadGuardSpecifier for parking_lot::RwLock<T> {
    type Target = T;
    type ReadGuard<'read> = parking_lot::RwLockReadGuard<'read, T> where T: 'read;
}
impl<T> DataReadLock for parking_lot::RwLock<T> {
    #[inline(always)]
    fn read(&self) -> Self::ReadGuard<'_> {
        self.read()
    }
}

impl<T> DataWriteLock for parking_lot::RwLock<T> {
    type WriteGuard<'write> = parking_lot::RwLockWriteGuard<'write, T> where T: 'write;

    #[inline(always)]
    fn write(&self) -> Self::WriteGuard<'_> {
        self.write()
    }

    #[inline(always)]
    fn downgrade<'a>(&'a self, write_guard: Self::WriteGuard<'a>) -> Self::ReadGuard<'a> {
        parking_lot::RwLockWriteGuard::downgrade(write_guard)
    }
}

impl<T> DataLockFactory<T> for parking_lot::RwLock<T> {
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
    fn downgrade<'a>(&'a self, write_guard: Self::WriteGuard<'a>) -> Self::ReadGuard<'a> {
        write_guard
    }
    #[inline(always)]
    fn upgrade<'a>(&'a self, read_guard: Self::WriteGuard<'a>) -> Self::WriteGuard<'a> {
        read_guard
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
}

impl<T> DataLockFactory<T> for parking_lot::Mutex<T> {
    type Lock = Self;

    #[inline(always)]
    fn new(init: T) -> Self::Lock {
        Self::new(init)
    }
}
