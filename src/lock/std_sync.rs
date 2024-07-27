use crate::{
    DataLockFactory, DataReadLock, DataWriteLock, ReadGuardSpecifier, RevisedData, SensorObserver,
    SensorWriter,
};

pub type RwSensorWriter<'share, T> =
    SensorWriter<'share, 'share, RevisedData<std::sync::RwLock<T>>, std::sync::RwLock<T>>;

pub type RwSensor<T> = SensorObserver<RevisedData<std::sync::RwLock<T>>>;
pub type MutexSensorWriter<'share, T> =
    SensorWriter<'share, 'share, RevisedData<std::sync::Mutex<T>>, std::sync::Mutex<T>>;
pub type MutexSensor<T> = SensorObserver<RevisedData<std::sync::Mutex<T>>>;

impl<T> ReadGuardSpecifier for std::sync::RwLock<T> {
    type Target = T;
    type ReadGuard<'read> = std::sync::RwLockReadGuard<'read, T> where T: 'read;
}

impl<T> DataReadLock for std::sync::RwLock<T> {
    #[inline(always)]
    fn read(&self) -> Self::ReadGuard<'_> {
        self.read().unwrap()
    }

    fn try_read(&self) -> Option<Self::ReadGuard<'_>> {
        self.try_read().ok()
    }
}

impl<T> DataWriteLock for std::sync::RwLock<T> {
    type WriteGuard<'write> = std::sync::RwLockWriteGuard<'write, T> where T: 'write;

    #[inline(always)]
    fn write(&self) -> Self::WriteGuard<'_> {
        self.write().unwrap()
    }

    fn try_write(&self) -> Option<Self::WriteGuard<'_>> {
        self.try_write().ok()
    }
}

impl<T> DataLockFactory for std::sync::RwLock<T> {
    type Lock = Self;

    #[inline(always)]
    fn new(init: T) -> Self::Lock {
        Self::new(init)
    }
}

impl<T> ReadGuardSpecifier for std::sync::Mutex<T> {
    type Target = T;
    type ReadGuard<'read> = std::sync::MutexGuard<'read, T> where T: 'read;
}

impl<T> DataReadLock for std::sync::Mutex<T> {
    #[inline(always)]
    fn read(&self) -> Self::ReadGuard<'_> {
        self.lock().unwrap()
    }

    #[inline(always)]
    fn try_read(&self) -> Option<Self::ReadGuard<'_>> {
        self.try_lock().ok()
    }
}
impl<T> DataWriteLock for std::sync::Mutex<T> {
    type WriteGuard<'write> = std::sync::MutexGuard<'write, T> where T: 'write;

    #[inline(always)]
    fn write(&self) -> Self::WriteGuard<'_> {
        self.lock().unwrap()
    }

    #[inline(always)]
    fn try_write(&self) -> Option<Self::WriteGuard<'_>> {
        self.try_lock().ok()
    }
}

impl<T> DataLockFactory for std::sync::Mutex<T> {
    type Lock = Self;

    #[inline(always)]
    fn new(init: T) -> Self::Lock {
        Self::new(init)
    }
}
