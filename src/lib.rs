pub mod parking_lot;
pub mod std_sync;

use core::marker::PhantomData;
use core::{
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicUsize, Ordering},
};
use std::sync::Arc;

use derived_deref::{Deref, DerefMut};

const CLOSED_BIT: usize = 1;
const STEP_SIZE: usize = 2;

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
    /// Behaviour is only well defined if and only if `write_guard` originates from `self`.
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
    /// Behaviour is only well defined if and only if `read_guard` originates from `self`.
    /// May panic if `read_guard` does not originate from `self`. Implementations may also upgrade the guard
    /// regardless or return a new write guard originating from `self`.
    #[inline(always)]
    fn upgrade<'a>(&'a self, read_guard: Self::ReadGuard<'a>) -> Self::WriteGuard<'a> {
        drop(read_guard);
        self.write()
    }
}

pub trait ReadGuardSpecifier {
    type Target;

    type ReadGuard<'read>: Deref<Target = Self::Target>
    where
        Self::Target: 'read,
        Self: 'read;
}

pub trait DataReadLock: ReadGuardSpecifier {
    /// Provides at least immutable access to the current value inside the lock.
    fn read(&self) -> Self::ReadGuard<'_>;
}

#[derive(Deref, DerefMut, Clone)]
pub struct FalseReadLock<T>(T);

impl<T> ReadGuardSpecifier for FalseReadLock<T> {
    type Target = T;

    type ReadGuard<'read> = &'read T where T: 'read;
}

impl<T> DataReadLock for FalseReadLock<T> {
    fn read(&self) -> Self::ReadGuard<'_> {
        self
    }
}
#[derive(Deref, DerefMut, Clone)]
pub struct OwnedData<T>(T);

impl<T> OwnedData<T> {
    #[inline(always)]
    pub fn into_inner(self) -> T {
        self.0
    }
}

#[derive(Default, Clone, Copy)]
pub struct OwnedFalseLock<T>(PhantomData<T>);
impl<T> ReadGuardSpecifier for OwnedFalseLock<T> {
    type Target = T;

    type ReadGuard<'read> = OwnedData<T> where T: 'read;
}

pub trait DataLockFactory<T> {
    type Lock: DataWriteLock<Target = T>;
    /// Contruct a new lock from the given initial value.
    fn new(init: T) -> Self::Lock;
}

pub struct RevisedData<T> {
    pub data: T,
    version: AtomicUsize,
}

impl<T> RevisedData<T> {
    #[inline(always)]
    pub fn new(data: T) -> Self {
        Self {
            data,
            version: Default::default(),
        }
    }

    #[inline(always)]
    pub fn update_version(&self, step_size: usize) {
        self.version
            .fetch_add(step_size, core::sync::atomic::Ordering::Release);
    }

    #[inline(always)]
    pub fn version(&self) -> usize {
        self.version.load(core::sync::atomic::Ordering::Acquire)
    }
}

pub struct RevisedDataWriter<'share, 'state, R, L>(R, PhantomData<(&'share Self, &'state L)>)
where
    R: StateShare<'share, 'state, L>,
    L: DataReadLock;

impl<'share, 'state, R, L> Drop for RevisedDataWriter<'share, 'state, R, L>
where
    R: StateShare<'share, 'state, L>,
    L: DataReadLock,
{
    fn drop(&mut self) {
        self.0
            .share_elided_ref()
            .version
            .fetch_or(CLOSED_BIT, Ordering::Release);
    }
}

impl<'share, 'state, R, L> RevisedDataWriter<'share, 'state, R, L>
where
    R: StateShare<'share, 'state, L>,
    L: DataWriteLock,
{
    #[inline(always)]
    pub fn new_from<LF: DataLockFactory<L::Target, Lock = L>>(init: L::Target) -> Self {
        Self(R::new(LF::new(init)), PhantomData)
    }

    #[inline(always)]
    pub fn spawn_referenced_observer(&self) -> RevisedDataObserver<&RevisedData<L>, L> {
        RevisedDataObserver {
            inner: self.0.share_elided_ref(),
            version: self.0.share_elided_ref().version(),
        }
    }

    #[inline(always)]
    fn spawn_observer(&'share self) -> RevisedDataObserver<R::Shared, L> {
        let inner = self.0.share_lock();
        RevisedDataObserver {
            version: inner.version(),
            inner,
        }
    }
}

pub trait StateShare<'share, 'state, L>
where
    L: 'state + DataReadLock,
{
    type Shared: Deref<Target = RevisedData<L>> + 'state;

    fn new(lock: L) -> Self;

    fn share_elided_ref(&self) -> &RevisedData<L>;

    fn share_lock(&'share self) -> Self::Shared;
}

impl<'share, L: 'share> StateShare<'share, 'share, L> for RevisedData<L>
where
    L: DataReadLock,
{
    type Shared = &'share Self;

    #[inline(always)]
    fn share_lock(&'share self) -> &Self {
        self
    }

    #[inline(always)]
    fn new(lock: L) -> Self {
        RevisedData::new(lock)
    }

    #[inline(always)]
    fn share_elided_ref(&self) -> &Self {
        &self
    }
}

impl<'share, L: 'static> StateShare<'share, 'static, L> for Arc<RevisedData<L>>
where
    L: 'static + DataReadLock,
{
    type Shared = Self;

    #[inline(always)]
    fn share_lock(&'share self) -> Self {
        self.clone()
    }

    #[inline(always)]
    fn new(lock: L) -> Self {
        Arc::new(RevisedData::new(lock))
    }

    #[inline(always)]
    fn share_elided_ref(&self) -> &RevisedData<L> {
        self
    }
}

#[derive(Clone)]
pub struct RevisedDataObserver<R, L>
where
    R: Deref<Target = RevisedData<L>>,
{
    inner: R,
    version: usize,
}

impl<R, L> RevisedDataObserver<R, L>
where
    R: Deref<Target = RevisedData<L>>,
{
    #[inline(always)]
    fn is_closed(&self) -> bool {
        self.inner.version() & CLOSED_BIT == CLOSED_BIT
    }
}

trait SensorIn {
    type Lock: DataWriteLock;
    /// Provide write access to the underlying sensor data.
    fn write(&self) -> <Self::Lock as DataWriteLock>::WriteGuard<'_>;
    /// Provide at least read access to the underlying sensor data.
    fn read(&self) -> <Self::Lock as ReadGuardSpecifier>::ReadGuard<'_>;
    /// Update the value of the sensor and notify observers.
    #[inline]
    fn update(&self, sample: <Self::Lock as ReadGuardSpecifier>::Target) {
        let mut guard = self.write();
        *guard = sample;
        self.mark_all_unseen();
        drop(guard);
    }
    /// Modify the sensor value in place and notify observers.
    #[inline]
    fn modify_with(&self, f: impl FnOnce(&mut <Self::Lock as ReadGuardSpecifier>::Target)) {
        let mut guard = self.write();
        f(&mut guard);
        self.mark_all_unseen();
        drop(guard);
    }

    /// Mark the current sensor value as unseen to all observers.
    fn mark_all_unseen(&self);
}

pub trait SensorOut {
    type Lock: ReadGuardSpecifier + ?Sized;

    /// Returns the latest value obtainable by the sensor. The sesnor's internal cache is guaranteed to
    /// be updated after this call if the sensor is cached.
    fn pull_updated(&mut self) -> <Self::Lock as ReadGuardSpecifier>::ReadGuard<'_>;
    /// Returns the current cached value of the sensor. This is guaranteed to be the latest value if the sensor
    /// is not cached.
    fn pull(&mut self) -> <Self::Lock as ReadGuardSpecifier>::ReadGuard<'_>;
    /// Mark the current sensor data as seen.
    fn mark_seen(&mut self);
    /// Mark the current sensor data as unseen.
    fn mark_unseen(&mut self);
    fn has_changed(&self) -> bool;
    /// Returns true if `pull` may produce stale results.
    fn is_cached(&self) -> bool;

    fn fuse<B, T, F>(self, other: B, f: F) -> RevisedDataObserverFused<Self, B, T, F>
    where
        Self: Sized,
        B: SensorOut,
        F: FnMut(
            &<<Self as SensorOut>::Lock as ReadGuardSpecifier>::Target,
            &<<B as SensorOut>::Lock as ReadGuardSpecifier>::Target,
        ) -> T,
    {
        RevisedDataObserverFused::fuse_with(self, other, f)
    }

    fn fuse_cached<B, T, F>(self, other: B, f: F) -> RevisedDataObserverFusedCached<Self, B, T, F>
    where
        Self: Sized,
        B: SensorOut,
        F: FnMut(
            &<<Self as SensorOut>::Lock as ReadGuardSpecifier>::Target,
            &<<B as SensorOut>::Lock as ReadGuardSpecifier>::Target,
        ) -> T,
    {
        RevisedDataObserverFusedCached::fuse_with(self, other, f)
    }

    fn map<T, F>(self, f: F) -> RevisedDataObserverMapped<Self, T, F>
    where
        Self: Sized,
        F: FnMut(&<<Self as SensorOut>::Lock as ReadGuardSpecifier>::Target) -> T,
    {
        RevisedDataObserverMapped::map_with(self, f)
    }

    fn map_cached<T, F>(self, f: F) -> RevisedDataObserverMappedCached<Self, T, F>
    where
        Self: Sized,
        F: FnMut(&<<Self as SensorOut>::Lock as ReadGuardSpecifier>::Target) -> T,
    {
        RevisedDataObserverMappedCached::map_with(self, f)
    }
}

impl<'share, 'state, R, L, T> SensorIn for RevisedDataWriter<'share, 'state, R, L>
where
    L: DataWriteLock<Target = T>,
    R: StateShare<'share, 'state, L>,
{
    type Lock = L;

    #[inline(always)]
    fn write(&self) -> L::WriteGuard<'_> {
        self.0.share_elided_ref().data.write()
    }

    #[inline(always)]
    fn read(&self) -> L::ReadGuard<'_> {
        self.0.share_elided_ref().data.read()
    }

    #[inline(always)]
    fn mark_all_unseen(&self) {
        self.0.share_elided_ref().update_version(STEP_SIZE)
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
        self.update_version(STEP_SIZE)
    }
}

impl<'share, 'state, T, L, R> SensorOut for RevisedDataObserver<R, L>
where
    L: DataReadLock<Target = T>,
    R: Deref<Target = RevisedData<L>>,
{
    type Lock = L;

    #[inline(always)]
    fn pull(&mut self) -> <Self::Lock as ReadGuardSpecifier>::ReadGuard<'_> {
        self.inner.data.read()
    }

    #[inline(always)]
    fn pull_updated(&mut self) -> <Self::Lock as ReadGuardSpecifier>::ReadGuard<'_> {
        self.mark_seen();
        self.inner.data.read()
    }

    #[inline(always)]
    fn mark_seen(&mut self) {
        self.version = self.inner.version();
    }

    #[inline(always)]
    fn mark_unseen(&mut self) {
        self.version = self.inner.version().wrapping_sub(STEP_SIZE);
    }

    #[inline(always)]
    fn has_changed(&self) -> bool {
        self.version >> 1 != self.inner.version() >> 1
    }
    #[inline(always)]
    fn is_cached(&self) -> bool {
        false
    }
}

pub struct RevisedDataObserverMapped<A, T, F> {
    a: A,
    _false_cache: OwnedFalseLock<T>,
    map: F,
}

impl<A, T, F> RevisedDataObserverMapped<A, T, F>
where
    A: SensorOut,
    F: FnMut(&<A::Lock as ReadGuardSpecifier>::Target) -> T,
{
    #[inline(always)]
    pub fn map_with(a: A, f: F) -> Self {
        Self {
            a,
            _false_cache: OwnedFalseLock(PhantomData {}),
            map: f,
        }
    }

    #[inline(always)]
    pub fn inner(&self) -> &A {
        &self.a
    }
}

impl<A, T, F> SensorOut for RevisedDataObserverMapped<A, T, F>
where
    A: SensorOut,
    F: FnMut(&<A::Lock as ReadGuardSpecifier>::Target) -> T,
{
    type Lock = OwnedFalseLock<T>;

    #[inline]
    fn pull_updated(&mut self) -> <Self::Lock as ReadGuardSpecifier>::ReadGuard<'_> {
        OwnedData((self.map)(&self.a.pull_updated()))
    }

    #[inline(always)]
    fn pull(&mut self) -> <Self::Lock as ReadGuardSpecifier>::ReadGuard<'_> {
        OwnedData((self.map)(&self.a.pull()))
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
    fn is_cached(&self) -> bool {
        false
    }
}

pub struct RevisedDataObserverMappedCached<A, T, F> {
    a: A,
    cached: FalseReadLock<T>,
    map: F,
}

impl<A, T, F> RevisedDataObserverMappedCached<A, T, F>
where
    A: SensorOut,
    F: FnMut(&<A::Lock as ReadGuardSpecifier>::Target) -> T,
{
    #[inline(always)]
    pub fn map_with(mut a: A, mut f: F) -> Self {
        let cached = FalseReadLock(f(&mut a.pull()));
        Self { a, cached, map: f }
    }

    #[inline(always)]
    pub fn inner(&self) -> &A {
        &self.a
    }
}

impl<A, T, F> SensorOut for RevisedDataObserverMappedCached<A, T, F>
where
    A: SensorOut,
    F: FnMut(&<A::Lock as ReadGuardSpecifier>::Target) -> T,
{
    type Lock = FalseReadLock<T>;

    #[inline]
    fn pull_updated(&mut self) -> <Self::Lock as ReadGuardSpecifier>::ReadGuard<'_> {
        *self.cached = (self.map)(&self.a.pull_updated());
        &self.cached
    }

    #[inline(always)]
    fn pull(&mut self) -> <Self::Lock as ReadGuardSpecifier>::ReadGuard<'_> {
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
    fn is_cached(&self) -> bool {
        true
    }
}
pub struct RevisedDataObserverFused<A, B, T, F> {
    a: A,
    b: B,
    _false_cache: OwnedFalseLock<T>,
    fuse: F,
}

impl<A, B, T, F> RevisedDataObserverFused<A, B, T, F>
where
    A: SensorOut,
    B: SensorOut,
    F: FnMut(
        &<A::Lock as ReadGuardSpecifier>::Target,
        &<B::Lock as ReadGuardSpecifier>::Target,
    ) -> T,
{
    #[inline(always)]
    pub fn fuse_with(a: A, b: B, f: F) -> Self {
        Self {
            a,
            b,
            _false_cache: OwnedFalseLock(PhantomData {}),
            fuse: f,
        }
    }

    #[inline(always)]
    pub fn inner_a(&self) -> &A {
        &self.a
    }

    #[inline(always)]
    pub fn inner_b(&self) -> &B {
        &self.b
    }
}

impl<A, B, T, F> SensorOut for RevisedDataObserverFused<A, B, T, F>
where
    A: SensorOut,
    B: SensorOut,
    F: FnMut(
        &<A::Lock as ReadGuardSpecifier>::Target,
        &<B::Lock as ReadGuardSpecifier>::Target,
    ) -> T,
{
    type Lock = OwnedFalseLock<T>;

    #[inline(always)]
    fn pull_updated(&mut self) -> <Self::Lock as ReadGuardSpecifier>::ReadGuard<'_> {
        OwnedData((self.fuse)(&self.a.pull_updated(), &self.b.pull_updated()))
    }

    #[inline(always)]
    fn pull(&mut self) -> <Self::Lock as ReadGuardSpecifier>::ReadGuard<'_> {
        OwnedData((self.fuse)(&self.a.pull(), &self.b.pull()))
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
    fn is_cached(&self) -> bool {
        self.a.is_cached() || self.b.is_cached()
    }
}

pub struct RevisedDataObserverFusedCached<A, B, T, F> {
    a: A,
    b: B,
    cache: FalseReadLock<T>,
    fuse: F,
}

impl<A, B, T, F> RevisedDataObserverFusedCached<A, B, T, F>
where
    A: SensorOut,
    B: SensorOut,
    F: FnMut(
        &<A::Lock as ReadGuardSpecifier>::Target,
        &<B::Lock as ReadGuardSpecifier>::Target,
    ) -> T,
{
    #[inline(always)]
    pub fn fuse_with(mut a: A, mut b: B, mut f: F) -> Self {
        let cache = FalseReadLock(f(&a.pull(), &b.pull()));
        Self {
            a,
            b,
            cache,
            fuse: f,
        }
    }

    #[inline(always)]
    pub fn inner_a(&self) -> &A {
        &self.a
    }

    #[inline(always)]
    pub fn inner_b(&self) -> &B {
        &self.b
    }
}

impl<A, B, T, F> SensorOut for RevisedDataObserverFusedCached<A, B, T, F>
where
    A: SensorOut,
    B: SensorOut,
    F: FnMut(
        &<A::Lock as ReadGuardSpecifier>::Target,
        &<B::Lock as ReadGuardSpecifier>::Target,
    ) -> T,
{
    type Lock = FalseReadLock<T>;

    #[inline(always)]
    fn pull_updated(&mut self) -> <Self::Lock as ReadGuardSpecifier>::ReadGuard<'_> {
        *self.cache = (self.fuse)(&self.a.pull_updated(), &self.b.pull_updated());
        &self.cache
    }

    #[inline(always)]
    fn pull(&mut self) -> <Self::Lock as ReadGuardSpecifier>::ReadGuard<'_> {
        &self.cache
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
    fn is_cached(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::{
        parking_lot::{ArcRwSensor, RwSensor},
        SensorIn, SensorOut, StateShare,
    };

    #[test]
    fn test_1() {
        let s1 = ArcRwSensor::new(-3);
        let s2 = RwSensor::new(5);

        let mut x = s1
            .spawn_observer()
            .fuse_cached(s2.spawn_observer(), |x, y| *x + *y);

        assert!(!x.has_changed());

        assert_eq!(*x.pull(), 2);
        s1.update(3);
        drop(s1);
        assert!(x.inner_a().is_closed());
        assert!(x.has_changed());
        assert_eq!(*x.pull(), 2);
        assert_eq!(*x.pull_updated(), 8);
        assert!(!x.has_changed());
        let mut x = x.map_cached(|x| x + 2);

        assert_eq!(*x.pull(), 10);
    }
}
