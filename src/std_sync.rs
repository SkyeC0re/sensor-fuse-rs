use crate::{
    DataLockFactory, DataReadLock, DataWriteLock, ReadGuardSpecifier, RevisedData,
    RevisedDataWriter,
};

pub type RwSensor<'share, T> =
    RevisedDataWriter<'share, 'share, RevisedData<std::sync::RwLock<T>>, std::sync::RwLock<T>>;
pub type MutexSensor<'share, T> =
    RevisedDataWriter<'share, 'share, RevisedData<std::sync::Mutex<T>>, std::sync::Mutex<T>>;

impl<T> RwSensor<'_, T> {
    #[inline(always)]
    pub fn new(init: T) -> Self {
        Self::new_from::<std::sync::RwLock<T>>(init)
    }
}

impl<T> MutexSensor<'_, T> {
    #[inline(always)]
    pub fn new(init: T) -> Self {
        Self::new_from::<std::sync::Mutex<T>>(init)
    }
}

impl<T> ReadGuardSpecifier for std::sync::RwLock<T> {
    type Target = T;
    type ReadGuard<'read> = std::sync::RwLockReadGuard<'read, T> where T: 'read;
}

impl<T> DataReadLock for std::sync::RwLock<T> {
    #[inline(always)]
    fn read(&self) -> Self::ReadGuard<'_> {
        self.read().unwrap()
    }
}

impl<T> DataWriteLock for std::sync::RwLock<T> {
    type WriteGuard<'write> = std::sync::RwLockWriteGuard<'write, T> where T: 'write;

    #[inline(always)]
    fn write(&self) -> Self::WriteGuard<'_> {
        self.write().unwrap()
    }
}

impl<T> DataLockFactory<T> for std::sync::RwLock<T> {
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
}
impl<T> DataWriteLock for std::sync::Mutex<T> {
    type WriteGuard<'write> = std::sync::MutexGuard<'write, T> where T: 'write;

    #[inline(always)]
    fn write(&self) -> Self::WriteGuard<'_> {
        self.lock().unwrap()
    }

    #[inline(always)]
    fn downgrade<'a>(&'a self, write_guard: Self::WriteGuard<'a>) -> Self::ReadGuard<'a> {
        write_guard
    }

    #[inline(always)]
    fn upgrade<'a>(&'a self, read_guard: Self::ReadGuard<'a>) -> Self::WriteGuard<'a> {
        read_guard
    }
}

impl<T> DataLockFactory<T> for std::sync::Mutex<T> {
    type Lock = Self;

    #[inline(always)]
    fn new(init: T) -> Self::Lock {
        Self::new(init)
    }
}
