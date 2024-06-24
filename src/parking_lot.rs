use parking_lot;

use crate::{DataLockFactory, DataReadLock, DataWriteLock, RevisedData};

pub type RwSensor<T> = RevisedData<parking_lot::RwLock<T>>;
pub type MutexSensor<T> = RevisedData<parking_lot::Mutex<T>>;

impl<T> RwSensor<T> {
    #[inline(always)]
    pub fn new(init: T) -> Self {
        Self::new_from::<parking_lot::RwLock<T>>(init)
    }
}

impl<T> MutexSensor<T> {
    #[inline(always)]
    pub fn new(init: T) -> Self {
        Self::new_from::<parking_lot::Mutex<T>>(init)
    }
}

impl<T> DataReadLock for parking_lot::RwLock<T> {
    type Target = T;
    type ReadGuard<'read> = parking_lot::RwLockReadGuard<'read, T> where T: 'read;
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

impl<T> DataReadLock for parking_lot::Mutex<T> {
    type Target = T;
    type ReadGuard<'read> = parking_lot::MutexGuard<'read, T> where T: 'read;

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
