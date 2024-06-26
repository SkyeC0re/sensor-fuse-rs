pub mod parking_lot;
pub mod std_sync;

use core::{
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicUsize, Ordering},
};

use derived_deref::{Deref, DerefMut};

pub trait DataWriteLock: DataReadLock {
    type WriteGuard<'write>: Deref<Target = Self::Target> + DerefMut
    where
        Self::Target: 'write,
        Self: 'write;

    /// Provides mutable access to the current value inside the lock.
    fn write(&self) -> Self::WriteGuard<'_>;

    /// Downgrade mutable access to immutable access for a write guard originating from this lock instance.
    ///
    /// # Panics
    ///
    /// Behaviour is only well defined if and only if `write_guard` originates from self.
    /// May panic if `wirte_guard` does not originate from `self`. Implementations may also downgrade the guard
    /// regardless or return a new read guard originating from `self`.
    #[inline(always)]
    fn downgrade<'a>(&'a self, write_guard: Self::WriteGuard<'a>) -> Self::ReadGuard<'a> {
        drop(write_guard);
        self.read()
    }
    /// Upgrade immutable access to mutable access for a read guard from this specific lock instance.
    ///
    /// # Panics
    ///
    /// Behaviour is only well defined if and only if `read_guard` originates from self.
    /// May panic if `read_guard` does not originate from `self`. Implementations may also upgrade the guard
    /// regardless or return a new write guard originating from `self`.
    #[inline(always)]
    fn upgrade<'a>(&'a self, read_guard: Self::ReadGuard<'a>) -> Self::WriteGuard<'a> {
        drop(read_guard);
        self.write()
    }
}

pub trait DataReadLock {
    type Target;

    type ReadGuard<'read>: Deref<Target = Self::Target>
    where
        Self::Target: 'read,
        Self: 'read;

    /// Provides at least immutable access to the current value inside the lock.
    fn read(&self) -> Self::ReadGuard<'_>;
}

#[derive(Deref, DerefMut)]
pub struct FalseReadLock<T>(T);

impl<T> DataReadLock for FalseReadLock<T> {
    type Target = T;

    type ReadGuard<'read> = &'read T where T: 'read;

    fn read(&self) -> Self::ReadGuard<'_> {
        self
    }
}

pub trait DataLockFactory<T> {
    type Lock: DataWriteLock<Target = T>;
    /// Contruct a new lock from the given initial value.
    fn new(init: T) -> Self::Lock;
}

pub struct RevisedData<T> {
    pub data: T,
    rev: AtomicUsize,
}

impl<L> RevisedData<L>
where
    L: DataWriteLock,
{
    pub fn new_from<LF: DataLockFactory<L::Target, Lock = L>>(init: L::Target) -> Self {
        Self {
            data: LF::new(init),
            rev: AtomicUsize::default(),
        }
    }

    pub fn spawn_observer(&self) -> RevisedDataObserver<L> {
        RevisedDataObserver {
            inner: self,
            rev: self.rev.load(Ordering::SeqCst),
        }
    }
}

pub struct RevisedDataObserver<'a, L> {
    inner: &'a RevisedData<L>,
    rev: usize,
}

impl<T> RevisedData<T> {
    pub fn update_rev(&self) {
        self.rev.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    }

    pub fn rev(&self) -> usize {
        self.rev.load(core::sync::atomic::Ordering::SeqCst)
    }
}

trait SensorIn {
    type Lock: DataWriteLock;
    /// Provide write access to the underlying sensor data.
    fn write(&self) -> <Self::Lock as DataWriteLock>::WriteGuard<'_>;
    /// Provide at least read access to the underlying sensor data.
    fn read(&self) -> <Self::Lock as DataReadLock>::ReadGuard<'_>;
    /// Update the value of the sensor and notify observers.
    #[inline]
    fn update(&self, sample: <Self::Lock as DataReadLock>::Target) {
        let mut guard = self.write();
        *guard = sample;
        self.mark_all_unseen();
        drop(guard);
    }
    /// Modify the sensor value in place and notify observers.
    #[inline(always)]
    fn modify_with(&self, f: impl FnOnce(&mut <Self::Lock as DataReadLock>::Target)) {
        let mut guard = self.write();
        f(&mut guard);
        self.mark_all_unseen();
        drop(guard);
    }

    /// Mark the current sensor value as unseen to all observers.
    fn mark_all_unseen(&self);
}

pub trait SensorOut {
    type Lock: DataReadLock;

    /// Returns the latest value obtainable by the sensor. The sesnor's internal cache is guaranteed to
    /// be updated after this call if the sensor is cached.
    #[inline(always)]
    fn pull_updated(&mut self) -> <Self::Lock as DataReadLock>::ReadGuard<'_> {
        self.mark_seen();
        self.pull()
    }
    /// Returns the current cached value of the sensor. This is guaranteed to be the latest value if the sensor
    /// is not cached.
    fn pull(&self) -> <Self::Lock as DataReadLock>::ReadGuard<'_>;
    /// Mark the current sensor data as seen.
    fn mark_seen(&mut self);
    /// Mark the current sensor data as unseen.
    fn mark_unseen(&mut self);
    fn has_changed(&self) -> bool;
    /// Returns true if `pull` may produce stale results.
    fn cached(&self) -> bool;

    fn fuse<B, T, F>(self, other: B, f: F) -> RevisedDataObserverFused<Self, B, T, F>
    where
        Self: Sized,
        B: SensorOut,
        F: FnMut(
            &<<Self as SensorOut>::Lock as DataReadLock>::Target,
            &<<B as SensorOut>::Lock as DataReadLock>::Target,
        ) -> T,
    {
        RevisedDataObserverFused::fuse_with(self, other, f)
    }

    fn map<T, F>(self, f: F) -> RevisedDataObserverMapped<Self, T, F>
    where
        Self: Sized,
        F: FnMut(&<<Self as SensorOut>::Lock as DataReadLock>::Target) -> T,
    {
        RevisedDataObserverMapped::map_with(self, f)
    }
}

impl<T, L> SensorIn for RevisedData<L>
where
    L: DataWriteLock<Target = T>,
{
    type Lock = L;

    #[inline(always)]
    fn write(&self) -> L::WriteGuard<'_> {
        self.data.write()
    }

    #[inline(always)]
    fn read(&self) -> L::ReadGuard<'_> {
        self.data.read()
    }

    #[inline(always)]
    fn mark_all_unseen(&self) {
        self.update_rev()
    }
}

impl<T, L, S> SensorIn for S
where
    L: DataWriteLock<Target = T>,
    S: Deref<Target = RevisedData<L>> + ?Sized,
{
    type Lock = L;
    #[inline(always)]
    fn write(&self) -> L::WriteGuard<'_> {
        self.data.write()
    }

    #[inline(always)]
    fn read(&self) -> L::ReadGuard<'_> {
        self.data.read()
    }

    #[inline(always)]
    fn mark_all_unseen(&self) {
        self.update_rev()
    }
}

impl<'a, T, L> SensorOut for RevisedDataObserver<'a, L>
where
    L: DataReadLock<Target = T>,
{
    type Lock = L;

    #[inline(always)]
    fn pull(&self) -> L::ReadGuard<'_> {
        self.inner.data.read()
    }

    #[inline(always)]
    fn mark_seen(&mut self) {
        self.rev = self.inner.rev.load(Ordering::SeqCst);
    }

    #[inline(always)]
    fn mark_unseen(&mut self) {
        self.rev = self.inner.rev.load(Ordering::SeqCst).wrapping_sub(1);
    }

    #[inline(always)]
    fn has_changed(&self) -> bool {
        self.rev != self.inner.rev.load(Ordering::SeqCst)
    }
    #[inline(always)]
    fn cached(&self) -> bool {
        false
    }
}

pub struct RevisedDataObserverMapped<A, T, F> {
    a: A,
    cached: FalseReadLock<T>,
    map: F,
}

impl<A, T, F> RevisedDataObserverMapped<A, T, F>
where
    A: SensorOut,
    F: FnMut(&<A::Lock as DataReadLock>::Target) -> T,
{
    #[inline(always)]
    pub fn map_with(a: A, mut f: F) -> Self {
        let cached = FalseReadLock(f(&a.pull()));
        Self { a, cached, map: f }
    }
}

impl<A, T, F> SensorOut for RevisedDataObserverMapped<A, T, F>
where
    A: SensorOut,
    F: FnMut(&<A::Lock as DataReadLock>::Target) -> T,
{
    type Lock = FalseReadLock<T>;

    #[inline]
    fn pull_updated(&mut self) -> <Self::Lock as DataReadLock>::ReadGuard<'_> {
        *self.cached = (self.map)(&self.a.pull_updated());
        &self.cached
    }

    #[inline(always)]
    fn pull(&self) -> <Self::Lock as DataReadLock>::ReadGuard<'_> {
        &self.cached
    }

    #[inline(always)]
    fn mark_seen(&mut self) {
        self.a.mark_seen();
    }

    #[inline(always)]
    fn mark_unseen(&mut self) {
        self.a.mark_unseen();
    }

    #[inline(always)]
    fn has_changed(&self) -> bool {
        self.a.has_changed()
    }

    #[inline(always)]
    fn cached(&self) -> bool {
        true
    }
}
pub struct RevisedDataObserverFused<A, B, T, F> {
    a: A,
    b: B,
    cached: FalseReadLock<T>,
    fuse: F,
}

impl<A, B, T, F> RevisedDataObserverFused<A, B, T, F>
where
    A: SensorOut,
    B: SensorOut,
    F: FnMut(&<A::Lock as DataReadLock>::Target, &<B::Lock as DataReadLock>::Target) -> T,
{
    #[inline(always)]
    pub fn fuse_with(a: A, b: B, mut f: F) -> Self {
        let cached = FalseReadLock(f(&a.pull(), &b.pull()));
        Self {
            a,
            b,
            cached,
            fuse: f,
        }
    }
}

impl<A, B, T, F> SensorOut for RevisedDataObserverFused<A, B, T, F>
where
    A: SensorOut,
    B: SensorOut,
    F: FnMut(&<A::Lock as DataReadLock>::Target, &<B::Lock as DataReadLock>::Target) -> T,
{
    type Lock = FalseReadLock<T>;

    #[inline]
    fn pull_updated(&mut self) -> <Self::Lock as DataReadLock>::ReadGuard<'_> {
        *self.cached = (self.fuse)(&self.a.pull_updated(), &self.b.pull_updated());
        &self.cached
    }

    #[inline(always)]
    fn pull(&self) -> <Self::Lock as DataReadLock>::ReadGuard<'_> {
        &self.cached
    }

    #[inline(always)]
    fn mark_seen(&mut self) {
        self.a.mark_seen();
        self.b.mark_seen();
    }

    #[inline(always)]
    fn mark_unseen(&mut self) {
        self.a.mark_unseen();
        self.b.mark_unseen();
    }

    #[inline(always)]
    fn has_changed(&self) -> bool {
        self.a.has_changed() || self.b.has_changed()
    }

    #[inline(always)]
    fn cached(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use crate::{parking_lot::RwSensor, SensorIn, SensorOut};

    #[test]
    fn test_1() {
        let s1 = RwSensor::new(-3);
        let s2 = RwSensor::new(5);

        let mut x = s1
            .spawn_observer()
            .fuse(s2.spawn_observer(), |x, y| *x + *y);

        assert!(!x.has_changed());

        assert_eq!(*x.pull(), 2);
        s1.update(3);
        assert!(x.has_changed());
        assert_eq!(*x.pull(), 2);
        assert_eq!(*x.pull_updated(), 8);
        assert!(!x.has_changed());
        let x = x.map(|x| x + 2);

        assert_eq!(*x.pull(), 10);
    }
}
