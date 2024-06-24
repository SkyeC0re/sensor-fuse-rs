use crate::{DataLockFactory, DataReadLock, DataWriteLock, RevisedData};

pub type RwSensor<T> = RevisedData<std::sync::RwLock<T>>;
pub type MutexSensor<T> = RevisedData<std::sync::Mutex<T>>;

impl<T> RwSensor<T> {
    #[inline(always)]
    pub fn new(init: T) -> Self {
        Self::new_from::<std::sync::RwLock<T>>(init)
    }
}

impl<T> MutexSensor<T> {
    #[inline(always)]
    pub fn new(init: T) -> Self {
        Self::new_from::<std::sync::Mutex<T>>(init)
    }
}

impl<T> DataReadLock for std::sync::RwLock<T> {
    type Target = T;
    type ReadGuard<'read> = std::sync::RwLockReadGuard<'read, T> where T: 'read;

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

impl<T> DataReadLock for std::sync::Mutex<T> {
    type Target = T;
    type ReadGuard<'read> = std::sync::MutexGuard<'read, T> where T: 'read;

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
