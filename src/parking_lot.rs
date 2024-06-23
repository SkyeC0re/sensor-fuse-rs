use parking_lot;

use crate::{DataLock, RevisedData, RevisedDataObserverFused};

pub type Sensor<T> = RevisedData<parking_lot::RwLock<T>>;
pub type FusedSensorObserver<A, B, O, F> =
    RevisedDataObserverFused<A, B, parking_lot::RwLock<O>, F>;

impl<T> DataLock for parking_lot::RwLock<T> {
    type Target = T;
    type ReadGuard<'read> = parking_lot::RwLockReadGuard<'read, T> where T: 'read;
    type WriteGuard<'write> = parking_lot::RwLockWriteGuard<'write, T> where T: 'write;

    #[inline(always)]
    fn new(init: T) -> Self {
        Self::new(init)
    }

    #[inline(always)]
    fn read(&self) -> Self::ReadGuard<'_> {
        self.read()
    }

    #[inline(always)]
    fn write(&self) -> Self::WriteGuard<'_> {
        self.write()
    }

    #[inline(always)]
    fn downgrade<'a>(&'a self, write_guard: Self::WriteGuard<'a>) -> Self::ReadGuard<'a> {
        parking_lot::RwLockWriteGuard::downgrade(write_guard)
    }
}
