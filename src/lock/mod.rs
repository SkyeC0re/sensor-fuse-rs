pub mod parking_lot;
// pub mod std_sync;

use std::{
    marker::PhantomData,
    ops::{Deref, DerefMut},
    sync::Arc,
};

use derived_deref::{Deref, DerefMut};

use crate::{RevisedData, SensorObserver, SensorWriter};

pub type AbstractSensorObserver<'a, L> = SensorObserver<&'a RevisedData<L>, L>;
pub type AbstractSensorWriter<L> = SensorWriter<RevisedData<L>, L>;

pub type AbstractArcSensorObserver<L> = SensorObserver<Arc<RevisedData<L>>, L>;
pub type AbstractArcSensorWriter<L> = SensorWriter<Arc<RevisedData<L>>, L>;

pub trait ReadGuardSpecifier {
    type Target;

    type ReadGuard<'read>: Deref<Target = Self::Target>
    where
        Self: 'read;
}

pub trait DataReadLock: ReadGuardSpecifier {
    /// Provides at least immutable access to the current value inside the lock.
    fn read(&self) -> Self::ReadGuard<'_>;

    fn try_read(&self) -> Option<Self::ReadGuard<'_>>;
}
pub trait DataWriteLock: DataReadLock {
    type WriteGuard<'write>: Deref<Target = Self::Target> + DerefMut
    where
        Self: 'write;

    /// Provides mutable access to the current value inside the lock.
    fn write(&self) -> Self::WriteGuard<'_>;

    fn try_write(&self) -> Option<Self::WriteGuard<'_>>;
}

#[derive(Deref, DerefMut, Clone)]
#[repr(transparent)]
pub struct FalseReadLock<T>(pub T);

impl<T> ReadGuardSpecifier for FalseReadLock<T> {
    type Target = T;

    type ReadGuard<'read> = &'read T where T: 'read;
}

impl<T> DataReadLock for FalseReadLock<T> {
    fn read(&self) -> Self::ReadGuard<'_> {
        self
    }

    fn try_read(&self) -> Option<Self::ReadGuard<'_>> {
        Some(self)
    }
}

#[derive(Default, Clone, Copy)]
pub struct OwnedFalseLock<T>(PhantomData<T>);

impl<T> OwnedFalseLock<T> {
    #[inline(always)]
    pub fn new() -> Self {
        Self(PhantomData)
    }
}

impl<T> ReadGuardSpecifier for OwnedFalseLock<T> {
    type Target = T;

    type ReadGuard<'read> = OwnedData<T> where T: 'read;
}

#[derive(Deref, DerefMut, Clone)]
#[repr(transparent)]
pub struct OwnedData<T>(pub T);

impl<T> OwnedData<T> {
    #[inline(always)]
    pub fn into_inner(self) -> T {
        self.0
    }
}
