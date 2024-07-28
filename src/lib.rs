pub mod callback_manager;
pub mod lock;

use callback_manager::{CallbackManager, ExecData, ExecLock};
use core::marker::PhantomData;
use core::{
    ops::Deref,
    sync::atomic::{AtomicUsize, Ordering},
};
use lock::{
    DataReadLock, DataWriteLock, FalseReadLock, OwnedData, OwnedFalseLock, ReadGuardSpecifier,
};
use std::ops::DerefMut;

use std::sync::Arc;

// All credit to [Tokio's Watch Channel](https://docs.rs/tokio/latest/tokio/sync/watch/index.html). If its not broken don't fix it.
const CLOSED_BIT: usize = 1;
const STEP_SIZE: usize = 2;

pub struct RevisedData<T> {
    pub data: T,
    version: AtomicUsize,
}

impl<T> Deref for RevisedData<T> {
    type Target = T;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        &self.data
    }
}

impl<T> DerefMut for RevisedData<T> {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.data
    }
}

impl<T> RevisedData<T> {
    #[inline(always)]
    pub const fn new(data: T) -> Self {
        Self {
            data,
            version: AtomicUsize::new(0),
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

pub trait Lockshare<'share, 'state, L: DataWriteLock + 'state> {
    // type Lock: DataWriteLock + 'state;
    type Shared: Deref<Target = RevisedData<L>> + 'state;

    /// Construct a new shareable lock from the given lock.
    fn new(lock: L) -> Self;

    /// Share the revised data for the largest possible lifetime.
    fn share_lock(&'share self) -> Self::Shared;

    /// There is probably a better way than this to get an elided reference
    /// to the revised data.
    fn share_elided_ref(&self) -> &RevisedData<L>;
}

#[repr(transparent)]
pub struct SensorWriter<'share, 'state, R, L: DataWriteLock + 'state>(
    R,
    PhantomData<(&'share Self, &'state L)>,
)
where
    R: Lockshare<'share, 'state, L>;

impl<'share, 'state, R, L: DataWriteLock + 'state> Drop for SensorWriter<'share, 'state, R, L>
where
    R: Lockshare<'share, 'state, L>,
{
    fn drop(&mut self) {
        self.0
            .share_elided_ref()
            .version
            .fetch_or(CLOSED_BIT, Ordering::Release);
    }
}

impl<'share, 'state, R, L: DataWriteLock + 'state> Deref for SensorWriter<'share, 'state, R, L>
where
    R: Lockshare<'share, 'state, L>,
{
    type Target = R;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<'share, 'state, R, L: DataWriteLock + 'state> DerefMut for SensorWriter<'share, 'state, R, L>
where
    R: Lockshare<'share, 'state, L>,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<'share, 'state, R, L: DataWriteLock + 'state> SensorWriter<'share, 'state, R, L>
where
    R: Lockshare<'share, 'state, L>,
{
    #[inline(always)]
    pub fn spawn_referenced_observer(&self) -> SensorObserver<&RevisedData<L>, L> {
        SensorObserver {
            inner: self.0.share_elided_ref(),
            version: self.0.share_elided_ref().version(),
        }
    }

    #[inline(always)]
    pub fn spawn_observer(&'share self) -> SensorObserver<R::Shared, L> {
        let inner = self.0.share_lock();
        SensorObserver {
            version: inner.version(),
            inner,
        }
    }
}

// There must be a way to do this sharing without having to have a second lifetime in the trait,
// but the author is not experienced enough.
impl<'share, L> Lockshare<'share, 'share, L> for RevisedData<L>
where
    L: DataWriteLock + 'share,
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

impl<'state, L> Lockshare<'_, 'state, L> for Arc<RevisedData<L>>
where
    L: DataWriteLock + 'state,
{
    type Shared = Self;

    #[inline(always)]
    fn share_lock(&self) -> Self {
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
pub struct SensorObserver<R, L>
where
    R: Deref<Target = RevisedData<L>>,
{
    inner: R,
    version: usize,
}

impl<R, L> SensorObserver<R, L>
where
    R: Deref<Target = RevisedData<L>>,
{
    /// Returns true once all upstream writers have disconnected.
    #[inline(always)]
    fn is_closed(&self) -> bool {
        self.inner.version() & CLOSED_BIT == CLOSED_BIT
    }
}

pub trait SensorWrite {
    type Lock: DataWriteLock;

    fn read(&self) -> <Self::Lock as ReadGuardSpecifier>::ReadGuard<'_>;
    fn write(&self) -> <Self::Lock as DataWriteLock>::WriteGuard<'_>;
    fn update(&self, sample: <Self::Lock as ReadGuardSpecifier>::Target);
    /// Modify the sensor value in place and notify observers.
    fn modify_with(&self, f: impl FnOnce(&mut <Self::Lock as ReadGuardSpecifier>::Target));
    /// Mark the current sensor value as unseen to all observers.
    fn mark_all_unseen(&self);
}

pub trait SensorCallbackExec: SensorCallbackRegister {
    /// Update the sensor's current value, notify observers and execute all registered functions.
    fn update_exec(&self, sample: Self::Target);
    /// Modify the sensor value in place, notify observers and execute all registered functions.
    fn modify_with_exec(&self, f: impl FnOnce(&mut Self::Target));
    /// Execute all registered functions.
    fn exec(&self);
}

pub trait SensorCallbackRegister {
    type Target;

    /// Register a new function to the execution queue.
    fn register<F: 'static + FnMut(&Self::Target) -> bool>(&self, f: F);
}

impl<'share, 'state, R, L, T, E> SensorCallbackRegister
    for SensorWriter<'share, 'state, R, ExecLock<L, T, E>>
where
    L: DataWriteLock<Target = ExecData<T, E>>,
    R: Lockshare<'share, 'state, ExecLock<L, T, E>>,
    E: CallbackManager<Target = T>,
{
    type Target = T;

    fn register<F: 'static + FnMut(&T) -> bool>(&self, f: F) {
        self.share_elided_ref()
            .write()
            .inner
            .exec_manager
            .get_mut()
            .register(f);
    }
}

impl<'share, 'state, R, L, T, E> SensorCallbackRegister for SensorObserver<R, ExecLock<L, T, E>>
where
    L: DataWriteLock<Target = ExecData<T, E>>,
    R: Deref<Target = RevisedData<ExecLock<L, T, E>>>,
    E: CallbackManager<Target = T>,
{
    type Target = T;

    fn register<F: 'static + FnMut(&T) -> bool>(&self, f: F) {
        self.inner.write().inner.exec_manager.get_mut().register(f);
    }
}

pub trait SensorObserve {
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

    #[inline(always)]
    fn fuse<B, T, F>(self, other: B, f: F) -> FusedSensorObserver<Self, B, T, F>
    where
        Self: Sized,
        B: SensorObserve,
        F: FnMut(
            &<<Self as SensorObserve>::Lock as ReadGuardSpecifier>::Target,
            &<<B as SensorObserve>::Lock as ReadGuardSpecifier>::Target,
        ) -> T,
    {
        FusedSensorObserver::fuse_with(self, other, f)
    }

    #[inline(always)]
    fn fuse_cached<B, T, F>(self, other: B, f: F) -> FusedSensorObserverCached<Self, B, T, F>
    where
        Self: Sized,
        B: SensorObserve,
        F: FnMut(
            &<<Self as SensorObserve>::Lock as ReadGuardSpecifier>::Target,
            &<<B as SensorObserve>::Lock as ReadGuardSpecifier>::Target,
        ) -> T,
    {
        FusedSensorObserverCached::fuse_with(self, other, f)
    }

    #[inline(always)]
    fn map<T, F>(self, f: F) -> MappedSensorObserver<Self, T, F>
    where
        Self: Sized,
        F: FnMut(&<<Self as SensorObserve>::Lock as ReadGuardSpecifier>::Target) -> T,
    {
        MappedSensorObserver::map_with(self, f)
    }

    #[inline(always)]
    fn map_cached<T, F>(self, f: F) -> MappedSensorObserverCached<Self, T, F>
    where
        Self: Sized,
        F: FnMut(&<<Self as SensorObserve>::Lock as ReadGuardSpecifier>::Target) -> T,
    {
        MappedSensorObserverCached::map_with(self, f)
    }
}

impl<'share, 'state, R, L: DataWriteLock + 'state> SensorWrite
    for SensorWriter<'share, 'state, R, L>
where
    R: Lockshare<'share, 'state, L>,
{
    type Lock = L;

    #[inline(always)]
    fn mark_all_unseen(&self) {
        self.share_elided_ref().update_version(STEP_SIZE)
    }

    #[inline]
    fn update(&self, sample: <Self::Lock as ReadGuardSpecifier>::Target) {
        let mut guard = self.share_elided_ref().data.write();
        self.mark_all_unseen();
        *guard = sample;
    }

    #[inline]
    fn modify_with(&self, f: impl FnOnce(&mut <Self::Lock as ReadGuardSpecifier>::Target)) {
        let mut guard = self.share_elided_ref().data.write();
        self.mark_all_unseen();
        f(&mut guard);
    }

    #[inline]
    fn read(&self) -> <Self::Lock as ReadGuardSpecifier>::ReadGuard<'_> {
        self.share_elided_ref().data.read()
    }

    #[inline]
    fn write(&self) -> <Self::Lock as DataWriteLock>::WriteGuard<'_> {
        self.share_elided_ref().data.write()
    }
}

impl<'share, 'state, T, L, R> SensorObserve for SensorObserver<R, L>
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

pub struct MappedSensorObserver<
    A: SensorObserve,
    T,
    F: FnMut(&<A::Lock as ReadGuardSpecifier>::Target) -> T,
> {
    a: A,
    _false_cache: OwnedFalseLock<T>,
    map: F,
}

impl<A, T, F> MappedSensorObserver<A, T, F>
where
    A: SensorObserve,
    F: FnMut(&<A::Lock as ReadGuardSpecifier>::Target) -> T,
{
    #[inline(always)]
    pub fn map_with(a: A, f: F) -> Self {
        Self {
            a,
            _false_cache: OwnedFalseLock::new(),
            map: f,
        }
    }

    #[inline(always)]
    pub fn inner(&self) -> &A {
        &self.a
    }
}

impl<A, T, F> SensorObserve for MappedSensorObserver<A, T, F>
where
    A: SensorObserve,
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

pub struct MappedSensorObserverCached<
    A: SensorObserve,
    T,
    F: FnMut(&<A::Lock as ReadGuardSpecifier>::Target) -> T,
> {
    a: A,
    cached: FalseReadLock<T>,
    map: F,
}

impl<A, T, F> MappedSensorObserverCached<A, T, F>
where
    A: SensorObserve,
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

impl<A, T, F> SensorObserve for MappedSensorObserverCached<A, T, F>
where
    A: SensorObserve,
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
pub struct FusedSensorObserver<
    A: SensorObserve,
    B: SensorObserve,
    T,
    F: FnMut(&<A::Lock as ReadGuardSpecifier>::Target, &<B::Lock as ReadGuardSpecifier>::Target) -> T,
> {
    a: A,
    b: B,
    _false_cache: OwnedFalseLock<T>,
    fuse: F,
}

impl<A, B, T, F> FusedSensorObserver<A, B, T, F>
where
    A: SensorObserve,
    B: SensorObserve,
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
            _false_cache: OwnedFalseLock::new(),
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

impl<A, B, T, F> SensorObserve for FusedSensorObserver<A, B, T, F>
where
    A: SensorObserve,
    B: SensorObserve,
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

pub struct FusedSensorObserverCached<
    A: SensorObserve,
    B: SensorObserve,
    T,
    F: FnMut(&<A::Lock as ReadGuardSpecifier>::Target, &<B::Lock as ReadGuardSpecifier>::Target) -> T,
> {
    a: A,
    b: B,
    cache: FalseReadLock<T>,
    fuse: F,
}

impl<A, B, T, F> FusedSensorObserverCached<A, B, T, F>
where
    A: SensorObserve,
    B: SensorObserve,
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

impl<A, B, T, F> SensorObserve for FusedSensorObserverCached<A, B, T, F>
where
    A: SensorObserve,
    B: SensorObserve,
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

    use crate::{
        lock::parking_lot::{
            ArcRwSensor, ArcRwSensorWriter, RwSensor, RwSensorExec, RwSensorWriter,
            RwSensorWriterExec,
        },
        Lockshare, MappedSensorObserver, SensorCallbackExec, SensorCallbackRegister, SensorObserve,
        SensorWrite,
    };

    #[test]
    fn test_1() {
        let s1 = ArcRwSensorWriter::new(-3);
        let s2 = RwSensorWriterExec::new(5);
        let s3 = RwSensorWriter::new(8);

        let b: RwSensorExec<_> = s2.spawn_observer();
        let z: RwSensor<_> = s3.spawn_observer();

        let bb: ArcRwSensor<_> = s1.spawn_observer();

        let mut x = s1
            .spawn_observer()
            .fuse_cached(s2.spawn_observer(), |x, y| *x + *y);

        s2.register(|new_val| {
            println!("New sample {}", new_val);
            true
        });

        let d = s2.spawn_observer();

        d.register(|new_val| {
            println!("New sample OBS {}", new_val);
            true
        });

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

        s2.update_exec(7);
        s2.update_exec(10);

        assert_eq!(*x.pull(), 10);
    }
}
