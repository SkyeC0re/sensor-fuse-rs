pub mod callback;
pub mod lock;
pub mod prelude;

use core::{
    ops::Deref,
    sync::atomic::{AtomicUsize, Ordering},
};
use std::{
    future::Future, marker::PhantomData, ops::DerefMut, ptr::null_mut, sync::Arc, task::Poll,
};

use crate::{
    callback::{CallbackExecute, CallbackRegister, ExecData, ExecGuard, ExecLock, WakerRegister},
    lock::{
        DataReadLock, DataWriteLock, FalseReadLock, OwnedData, OwnedFalseLock, ReadGuardSpecifier,
    },
};

/*** Revised Data ***/

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

/// Trait for sharing a (most likely heap pointer) wrapped `struct@RevisedData` with a locking strategy.
pub trait Lockshare<'a> {
    type Lock: DataWriteLock;
    type Shared: Deref<Target = RevisedData<Self::Lock>>;

    /// Share the revised data for the largest possible lifetime.
    fn share_lock(self) -> Self::Shared;

    /// There is probably a better way than this to get an elided reference
    /// to the revised data.
    fn share_elided_ref(self) -> &'a RevisedData<Self::Lock>;
}

impl<'a, L> Lockshare<'a> for &'a RevisedData<L>
where
    L: DataWriteLock,
{
    type Lock = L;
    type Shared = Self;
    #[inline(always)]
    fn share_lock(self) -> Self {
        self
    }

    #[inline(always)]
    fn share_elided_ref(self) -> Self {
        self
    }
}

impl<'a, L> Lockshare<'a> for &'a Arc<RevisedData<L>>
where
    L: DataWriteLock,
{
    type Lock = L;
    type Shared = Arc<RevisedData<L>>;

    #[inline(always)]
    fn share_lock(self) -> Self::Shared {
        self.clone()
    }

    #[inline(always)]
    fn share_elided_ref(self) -> &'a RevisedData<L> {
        self
    }
}

pub trait SensorWrite<T> {
    type Lock: DataWriteLock<Target = T>;
    type LockshareStrategy<'a>: Lockshare<'a, Lock = Self::Lock>
    where
        Self: 'a;

    /// Acquire a read lock on the underlying data.
    fn read(&self) -> <Self::Lock as ReadGuardSpecifier>::ReadGuard<'_>;
    /// Acquire a write lock on the underlying data.
    fn write(&self) -> <Self::Lock as DataWriteLock>::WriteGuard<'_>;
    /// Update the sensor value and notify observers.
    fn update(&self, sample: T);
    /// Modify the sensor value in place and notify observers.
    fn modify_with(&self, f: impl FnOnce(&mut T));
    /// Mark the current sensor value as unseen to all observers and notify them.
    fn mark_all_unseen(&self);

    /// Spawn an observer by immutably borrowing from the sensor writer.
    fn spawn_referenced_observer(&self) -> SensorObserver<&RevisedData<Self::Lock>, Self::Lock>;

    /// Spawn an observer using the appropriate cloning strategy for the sensor data.
    fn spawn_observer(
        &self,
    ) -> SensorObserver<<Self::LockshareStrategy<'_> as Lockshare>::Shared, Self::Lock>;
}

/*** Sensor Writing ***/

#[repr(transparent)]
pub struct SensorWriter<S, L>(S)
where
    for<'a> &'a S: Lockshare<'a, Lock = L>;

impl<S, L> Drop for SensorWriter<S, L>
where
    for<'a> &'a S: Lockshare<'a, Lock = L>,
{
    fn drop(&mut self) {
        let x = &self.0;
        x.share_elided_ref()
            .version
            .fetch_or(CLOSED_BIT, Ordering::Release);
    }
}

#[repr(transparent)]
pub struct SensorWriterExec<S, L, T, E>(S)
where
    L: DataWriteLock<Target = ExecData<T, E>>,
    E: CallbackExecute<T>,
    for<'a> &'a S: Lockshare<'a, Lock = ExecLock<L, T, E>>;

impl<S, L, T, E> Drop for SensorWriterExec<S, L, T, E>
where
    L: DataWriteLock<Target = ExecData<T, E>>,
    E: CallbackExecute<T>,
    for<'a> &'a S: Lockshare<'a, Lock = ExecLock<L, T, E>>,
{
    fn drop(&mut self) {
        let x = &self.0;
        x.share_elided_ref()
            .version
            .fetch_or(CLOSED_BIT, Ordering::Release);
    }
}

impl<S, L: DataWriteLock> SensorWrite<L::Target> for SensorWriter<S, L>
where
    for<'a> &'a S: Lockshare<'a, Lock = L>,
{
    type Lock = L;
    type LockshareStrategy<'a> = &'a S
    where
        Self: 'a;

    #[inline(always)]
    fn mark_all_unseen(&self) {
        self.0.share_elided_ref().update_version(STEP_SIZE)
    }

    #[inline]
    fn update(&self, sample: L::Target) {
        let mut guard = self.0.share_elided_ref().data.write();
        self.mark_all_unseen();
        *guard = sample;
    }

    #[inline]
    fn modify_with(&self, f: impl FnOnce(&mut L::Target)) {
        let mut guard = self.0.share_elided_ref().data.write();
        self.mark_all_unseen();
        f(&mut guard);
    }

    #[inline]
    fn read(&self) -> L::ReadGuard<'_> {
        self.0.share_elided_ref().data.read()
    }

    #[inline]
    fn write(&self) -> L::WriteGuard<'_> {
        self.0.share_elided_ref().data.write()
    }

    #[allow(refining_impl_trait)]
    #[inline(always)]
    fn spawn_referenced_observer(&self) -> SensorObserver<&'_ RevisedData<L>, L> {
        let inner = self.0.share_elided_ref();
        SensorObserver {
            inner,
            version: inner.version(),
        }
    }

    #[allow(refining_impl_trait)]
    #[inline(always)]
    fn spawn_observer(&self) -> SensorObserver<<&'_ S as Lockshare>::Shared, L> {
        let inner = self.0.share_lock();
        SensorObserver {
            version: inner.version(),
            inner,
        }
    }
}

impl<S, L, T, E> SensorWrite<T> for SensorWriterExec<S, L, T, E>
where
    for<'a> &'a S: Lockshare<'a, Lock = ExecLock<L, T, E>>,
    L: DataWriteLock<Target = ExecData<T, E>>,
    E: CallbackExecute<T>,
{
    type Lock = ExecLock<L, T, E>;
    type LockshareStrategy<'a> = &'a S
    where
        Self: 'a;

    #[inline(always)]
    fn read(&self) -> <Self::Lock as ReadGuardSpecifier>::ReadGuard<'_> {
        self.0.share_elided_ref().data.read()
    }

    #[inline(always)]
    fn write(&self) -> <Self::Lock as DataWriteLock>::WriteGuard<'_> {
        self.0.share_elided_ref().data.write()
    }

    /// Update the sensor value, notify observers and execute all registered callbacks.
    #[inline]
    fn update(&self, sample: T) {
        let revised_data = self.0.share_elided_ref();
        let mut guard = revised_data.data.inner.write();
        guard.data = sample;
        revised_data.update_version(STEP_SIZE);
        let guard = L::atomic_downgrade(guard);

        // Atomic downgrade just occured. No other modication can happen.
        unsafe { (*guard.exec_manager.get()).callback(&guard.data) };
    }

    /// Modify the sensor value in place, notify observers and execute all registered callbacks.
    #[inline]
    fn modify_with(&self, f: impl FnOnce(&mut T)) {
        let revised_data = self.0.share_elided_ref();
        let mut guard = revised_data.data.inner.write();
        f(&mut guard.data);
        revised_data.update_version(STEP_SIZE);
        let guard = L::atomic_downgrade(guard);

        // Atomic downgrade just occured. No other modification can happen.
        unsafe {
            (*guard.exec_manager.get()).callback(&guard.data);
        };
    }

    /// Mark the current sensor value as unseen to all observers, notify them and execute all registered callbacks.
    #[inline]
    fn mark_all_unseen(&self) {
        let revised_data = self.0.share_elided_ref();
        let guard = L::atomic_downgrade(revised_data.data.inner.write());
        revised_data.update_version(STEP_SIZE);

        // Atomic downgrade just occured. No other modification can happen.
        unsafe { (*guard.exec_manager.get()).callback(&guard.data) };
    }

    #[allow(refining_impl_trait)]
    #[inline(always)]
    fn spawn_referenced_observer(
        &self,
    ) -> SensorObserver<&'_ RevisedData<ExecLock<L, T, E>>, ExecLock<L, T, E>> {
        let inner = self.0.share_elided_ref();
        SensorObserver {
            inner,
            version: inner.version(),
        }
    }

    #[allow(refining_impl_trait)]
    #[inline(always)]
    fn spawn_observer(&self) -> SensorObserver<<&'_ S as Lockshare>::Shared, ExecLock<L, T, E>> {
        let inner = self.0.share_lock();
        SensorObserver {
            version: inner.version(),
            inner,
        }
    }
}

/*** Sensor Observation ***/

pub trait SensorObserve {
    type Lock: ReadGuardSpecifier;

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
    /// Returns true if the sensor data has been marked as unseen.
    fn has_changed(&self) -> bool;
    /// Returns true if `pull` may produce stale results.
    fn is_cached(&self) -> bool;
    /// Returns true if all upstream writers has been dropped and no more updates can occur.
    fn is_closed(&self) -> bool;

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

pub struct SensorObserver<R, L>
where
    R: Deref<Target = RevisedData<L>>,
{
    inner: R,
    version: usize,
}

impl<R, L> Clone for SensorObserver<R, L>
where
    R: Deref<Target = RevisedData<L>> + Clone,
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            version: self.version,
        }
    }
}

impl<T, L, R> SensorObserve for SensorObserver<R, L>
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

    #[inline(always)]
    fn is_closed(&self) -> bool {
        self.inner.version() & CLOSED_BIT == CLOSED_BIT
    }
}

/*** Mapped and Fused Observers ***/

pub struct MappedSensorObserver<
    A: SensorObserve,
    T,
    F: FnMut(&<A::Lock as ReadGuardSpecifier>::Target) -> T,
> {
    pub inner: A,
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
            inner: a,
            _false_cache: OwnedFalseLock::new(),
            map: f,
        }
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
        OwnedData((self.map)(&self.inner.pull_updated()))
    }

    #[inline(always)]
    fn pull(&mut self) -> <Self::Lock as ReadGuardSpecifier>::ReadGuard<'_> {
        OwnedData((self.map)(&self.inner.pull()))
    }

    #[inline(always)]
    fn mark_seen(&mut self) {
        self.inner.mark_seen();
    }

    #[inline(always)]
    fn mark_unseen(&mut self) {
        self.inner.mark_unseen();
    }

    #[inline(always)]
    fn has_changed(&self) -> bool {
        self.inner.has_changed()
    }

    #[inline(always)]
    fn is_cached(&self) -> bool {
        false
    }

    #[inline(always)]
    fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }
}

pub struct MappedSensorObserverCached<
    A: SensorObserve,
    T,
    F: FnMut(&<A::Lock as ReadGuardSpecifier>::Target) -> T,
> {
    pub inner: A,
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
        Self {
            inner: a,
            cached,
            map: f,
        }
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
        *self.cached = (self.map)(&self.inner.pull_updated());
        &self.cached
    }

    #[inline(always)]
    fn pull(&mut self) -> <Self::Lock as ReadGuardSpecifier>::ReadGuard<'_> {
        &self.cached
    }

    #[inline(always)]
    fn mark_seen(&mut self) {
        self.inner.mark_seen();
    }

    #[inline(always)]
    fn mark_unseen(&mut self) {
        self.inner.mark_unseen();
    }

    #[inline(always)]
    fn has_changed(&self) -> bool {
        self.inner.has_changed()
    }

    #[inline(always)]
    fn is_cached(&self) -> bool {
        true
    }

    #[inline(always)]
    fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }
}
pub struct FusedSensorObserver<
    A: SensorObserve,
    B: SensorObserve,
    T,
    F: FnMut(&<A::Lock as ReadGuardSpecifier>::Target, &<B::Lock as ReadGuardSpecifier>::Target) -> T,
> {
    pub a: A,
    pub b: B,
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

    #[inline(always)]
    fn is_closed(&self) -> bool {
        self.a.is_closed() && self.b.is_closed()
    }
}

pub struct FusedSensorObserverCached<
    A: SensorObserve,
    B: SensorObserve,
    T,
    F: FnMut(&<A::Lock as ReadGuardSpecifier>::Target, &<B::Lock as ReadGuardSpecifier>::Target) -> T,
> {
    pub a: A,
    pub b: B,
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

    #[inline(always)]
    fn is_closed(&self) -> bool {
        self.a.is_closed() && self.b.is_closed()
    }
}

/*** Callbacks and Async ***/

pub trait RegisterFunction<T, F>
where
    F: FnMut(&T) -> bool,
{
    fn register(&self, f: F);
}

impl<S, L, T, E, F> RegisterFunction<T, F> for SensorWriterExec<S, L, T, E>
where
    L: DataWriteLock<Target = ExecData<T, E>>,
    E: CallbackRegister<T, F> + CallbackExecute<T>,
    F: FnMut(&T) -> bool,
    for<'b> &'b S: Lockshare<'b, Lock = ExecLock<L, T, E>>,
{
    fn register(&self, f: F) {
        self.0
            .share_elided_ref()
            .write()
            .inner
            .exec_manager
            .get_mut()
            .register(f);
    }
}

#[repr(transparent)]
pub struct WaitUntilChangedFuture<'a, R, L, T, E>(Option<&'a SensorObserver<R, ExecLock<L, T, E>>>)
where
    L: DataWriteLock<Target = ExecData<T, E>>,
    R: Deref<Target = RevisedData<ExecLock<L, T, E>>>,
    E: WakerRegister + CallbackExecute<T>;

impl<'a, R, L, T, E> Future for WaitUntilChangedFuture<'a, R, L, T, E>
where
    L: DataWriteLock<Target = ExecData<T, E>>,
    R: Deref<Target = RevisedData<ExecLock<L, T, E>>>,
    E: WakerRegister + CallbackExecute<T>,
    Self: Unpin,
{
    type Output = ();

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Self::Output> {
        if let Some(value) = self.0.take() {
            if value.has_changed() {
                Poll::Ready(())
            } else {
                value
                    .inner
                    .share_elided_ref()
                    .write()
                    .inner
                    .exec_manager
                    .get_mut()
                    .register_waker(cx.waker());
                self.0 = Some(value);
                Poll::Pending
            }
        } else {
            #[cfg(debug_assertions)]
            panic!("Poll called after future returned ready.");
            #[cfg(not(debug_assertions))]
            Poll::Pending
        }
    }
}

pub struct WaitForFuture<'a, R: 'a, L: 'a, T: 'a, E: 'a, F>(
    *mut SensorObserver<R, ExecLock<L, T, E>>,
    F,
    PhantomData<&'a ()>,
)
where
    L: DataWriteLock<Target = ExecData<T, E>>,
    R: Deref<Target = RevisedData<ExecLock<L, T, E>>>,
    E: WakerRegister + CallbackExecute<T>;

impl<'a, R: 'a, L: 'a, T: 'a, E: 'a, F> Future for WaitForFuture<'a, R, L, T, E, F>
where
    L: DataWriteLock<Target = ExecData<T, E>>,
    R: Deref<Target = RevisedData<ExecLock<L, T, E>>>,
    E: WakerRegister + CallbackExecute<T>,
    F: Send + FnMut(&T) -> bool,
    Self: Unpin,
{
    type Output = ExecGuard<L::ReadGuard<'a>, T, E>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Self::Output> {
        // Safe code does not seem to want to compile without a guard reaquisition. Possibly inexperience,
        // but also possibly a quirk of `Option`'s `None` variant still storing type information.
        if !self.0.is_null() {
            let mut observer = unsafe { &mut *self.0 };
            let changed = observer.has_changed();
            let guard = observer.pull_updated();
            if changed && (self.1)(&guard) {
                self.0 = null_mut();
                return Poll::Ready(guard);
            } else {
                drop(guard);
                observer = unsafe { &mut *self.0 };
                observer
                    .inner
                    .share_elided_ref()
                    .write()
                    .inner
                    .exec_manager
                    .get_mut()
                    .register_waker(cx.waker());
                return Poll::Pending;
            }
        }

        #[cfg(debug_assertions)]
        panic!("Poll called after future returned ready.");
        #[cfg(not(debug_assertions))]
        Poll::Pending
    }
}

impl<R, L, T, E> SensorObserver<R, ExecLock<L, T, E>>
where
    L: DataWriteLock<Target = ExecData<T, E>>,
    R: Deref<Target = RevisedData<ExecLock<L, T, E>>>,
    E: WakerRegister + CallbackExecute<T>,
{
    /// Asyncronously wait until the sensor value is updated. This call will **not** update the observer's version,
    /// as such an additional call to `pull` or `pull_updated` is required.
    pub fn wait_until_changed(&self) -> WaitUntilChangedFuture<'_, R, L, T, E> {
        WaitUntilChangedFuture(Some(self))
    }
    /// Asyncronously wait until the sensor value has been updated with a value that satisfies a condition. This call **will** update the observer's version
    /// and evaluate the condition function on values obtained by `pull_updated`.
    pub fn wait_for<F: Send + FnMut(&T) -> bool>(
        &mut self,
        f: F,
    ) -> WaitForFuture<'_, R, L, T, E, F> {
        WaitForFuture(self, f, PhantomData)
    }
}
impl<R, L, T, E, F: FnMut(&T) -> bool> RegisterFunction<T, F>
    for SensorObserver<R, ExecLock<L, T, E>>
where
    L: DataWriteLock<Target = ExecData<T, E>>,
    E: CallbackRegister<T, F> + CallbackExecute<T>,
    R: Deref<Target = RevisedData<ExecLock<L, T, E>>>,
{
    fn register(&self, f: F) {
        self.inner.write().inner.exec_manager.get_mut().register(f);
    }
}
