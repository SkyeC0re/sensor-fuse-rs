#[cfg(feature = "alloc")]
extern crate alloc;

pub mod parking_lot;
// #[cfg(feature = "std")]
// pub mod std_sync;

#[cfg(feature = "alloc")]
use alloc::sync::Arc;
use core::{
    marker::PhantomData,
    ops::{Deref, DerefMut},
};
use derived_deref::{Deref, DerefMut};

use crate::{RevisedData, SensorObserver, SensorWriter};

pub type AbstractSensorObserver<'a, T, L, E> = SensorObserver<T, &'a RevisedData<(L, E)>, L, E>;
pub type AbstractSensorWriter<T, L, E> = SensorWriter<T, RevisedData<(L, E)>, L, E>;

#[cfg(feature = "alloc")]
pub type AbstractArcSensorObserver<T, L, E> = SensorObserver<T, Arc<RevisedData<(L, E)>>, L, E>;
#[cfg(feature = "alloc")]
pub type AbstractArcSensorWriter<T, L, E> = SensorWriter<T, Arc<RevisedData<(L, E)>>, L, E>;

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
    type DowngradedGuard<'read>: Deref<Target = Self::Target>
    where
        Self: 'read;

    /// Provides mutable access to the current value inside the lock.
    fn write(&self) -> Self::WriteGuard<'_>;

    fn try_write(&self) -> Option<Self::WriteGuard<'_>>;

    /// Optional optimization for accessing the executor in callback enabled sensor writers and observers.
    /// This method should by default just return the write guard, but, if the lock allows for it,
    /// should return a read guard which was atomically downgraded from the given write guard.
    fn atomic_downgrade(write_guard: Self::WriteGuard<'_>) -> Self::DowngradedGuard<'_>;

    #[inline]
    fn downgraded(&self) -> Self::DowngradedGuard<'_> {
        Self::atomic_downgrade(self.write())
    }
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
    pub const fn new() -> Self {
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
