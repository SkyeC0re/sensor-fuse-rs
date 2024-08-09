pub mod callback_manager;
pub mod lock;
pub mod prelude;

use callback_manager::{CallbackExecute, CallbackRegister, ExecData, ExecGuard, ExecLock};
use core::{
    ops::Deref,
    sync::atomic::{AtomicUsize, Ordering},
};
use derived_deref::{Deref, DerefMut};
use lock::{
    DataReadLock, DataWriteLock, FalseReadLock, OwnedData, OwnedFalseLock, ReadGuardSpecifier,
};
use std::{
    cell::{Cell, OnceCell, UnsafeCell},
    future::Future,
    iter::Fuse,
    marker::PhantomData,
    mem::{self, take},
    ops::DerefMut,
    process::Output,
    sync::OnceState,
    task::Poll,
};

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

#[repr(transparent)]
pub struct SensorWriter<R, L>(R)
where
    for<'a> &'a R: Lockshare2<'a, Lock = L>;

impl<R, L> Drop for SensorWriter<R, L>
where
    for<'a> &'a R: Lockshare2<'a, Lock = L>,
{
    fn drop(&mut self) {
        let x = &self.0;
        x.share_elided_ref()
            .version
            .fetch_or(CLOSED_BIT, Ordering::Release);
    }
}

impl<R, L> Deref for SensorWriter<R, L>
where
    for<'a> &'a R: Lockshare2<'a, Lock = L>,
{
    type Target = R;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<R, L> DerefMut for SensorWriter<R, L>
where
    for<'a> &'a R: Lockshare2<'a, Lock = L>,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<R, L> SensorWriter<R, L>
where
    for<'a> &'a R: Lockshare2<'a, Lock = L>,
{
    #[inline(always)]
    pub fn spawn_referenced_observer(
        &self,
    ) -> SensorObserver<&'_ RevisedData<<&'_ R as Lockshare2>::Lock>, <&'_ R as Lockshare2>::Lock>
    {
        let inner = (&self.0).share_elided_ref();
        let version = (&self.0).share_elided_ref().version();
        SensorObserver { inner, version }
    }

    #[inline(always)]
    pub fn spawn_observer(
        &self,
    ) -> SensorObserver<<&'_ R as Lockshare2>::Shared, <&'_ R as Lockshare2>::Lock> {
        let inner = self.0.share_lock();
        SensorObserver {
            version: inner.version(),
            inner,
        }
    }
}

// pub trait Lockshare {
//     type Lock: DataWriteLock;
//     type Shared<'share>: Deref<Target = RevisedData<Self::Lock>>
//     where
//         Self: 'share;

//     /// Construct a new shareable lock from the given lock.
//     // fn new(lock: Self::Lock) -> Self;

//     /// Share the revised data for the largest possible lifetime.
//     fn share_lock(&self) -> Self::Shared<'_>;

//     /// There is probably a better way than this to get an elided reference
//     /// to the revised data.
//     fn share_elided_ref(&self) -> &RevisedData<Self::Lock>;
// }

pub trait Lockshare2<'a> {
    type Lock: DataWriteLock;
    type Shared: Deref<Target = RevisedData<Self::Lock>> + 'a;

    // /// Construct a new shareable lock from the given lock.
    // fn new(lock: Self::Lock) -> Self;

    /// Share the revised data for the largest possible lifetime.
    fn share_lock(self) -> Self::Shared;

    /// There is probably a better way than this to get an elided reference
    /// to the revised data.
    fn share_elided_ref(self) -> &'a RevisedData<Self::Lock>;
}

impl<'a, L> Lockshare2<'a> for &'a RevisedData<L>
where
    L: DataWriteLock,
{
    type Lock = L;
    type Shared = Self;
    #[inline(always)]
    fn share_lock(self) -> Self {
        self
    }

    // #[inline(always)]
    // fn new(lock: L) -> Self {
    //     RevisedData::new(lock)
    // }

    #[inline(always)]
    fn share_elided_ref(self) -> Self {
        self
    }
}

impl<'a, L> Lockshare2<'a> for &'a Arc<RevisedData<L>>
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

// // There must be a way to do this sharing without having to have a second lifetime in the trait,
// // but the author is not experienced enough.
// impl<L> Lockshare for RevisedData<L>
// where
//     L: DataWriteLock,
// {
//     type Lock = L;
//     type Shared<'share> = &'share Self
//     where
//         Self: 'share;
//     #[inline(always)]
//     fn share_lock(&self) -> &Self {
//         self
//     }

//     #[inline(always)]
//     fn share_elided_ref(&self) -> &Self {
//         &self
//     }
// }

// impl<L> Lockshare for Arc<RevisedData<L>>
// where
//     L: DataWriteLock,
// {
//     type Lock = L;
//     type Shared<'share> = Self
//     where
//         Self: 'share;

//     #[inline(always)]
//     fn share_lock(&self) -> Self {
//         self.clone()
//     }

//     #[inline(always)]
//     fn share_elided_ref(&self) -> &RevisedData<L> {
//         self
//     }
// }

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
    pub fn is_closed(&self) -> bool {
        self.inner.version() & CLOSED_BIT == CLOSED_BIT
    }
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

pub trait SensorCallbackExec<T> {
    /// Update the sensor's current value, notify observers and execute all registered functions.
    fn update_exec(&self, sample: T);
    /// Modify the sensor value in place, notify observers and execute all registered functions.
    fn modify_with_exec(&self, f: impl FnOnce(&mut T));
    /// Execute all registered functions.
    fn exec(&self);
}

// pub trait SensorCallbackRegister<T> {
//     /// Register a new function to the execution queue.
//     fn register<F: 'static + FnMut(&T) -> bool>(&self, f: F);

// }

impl<'a, R, L, T, E> SensorWriter<R, <&'a R as Lockshare2<'a>>::Lock>
where
    L: DataWriteLock<Target = ExecData<T, E>>,
    // R: Lockshare2<'a, Lock = ExecLock<L, T, E>>,
    E: CallbackRegister<'a, T>,
    for<'b> &'b R: Lockshare2<'b, Lock = ExecLock<L, T, E>>,
{
    fn register<F: 'a + FnMut(&T) -> bool>(&self, f: F) {
        self.deref()
            .share_elided_ref()
            .write()
            .inner
            .exec_manager
            .get_mut()
            .register(f);
    }
}

// fn elide<'a: 'b, 'b>(&)

pub trait StaticFuse<'a, I: 'a, O>
where
    Self: Deref<Target = I>,
{
    type Fused: DerefMut<Target = O>;

    unsafe fn static_fuse<F: FnOnce(&'a I) -> O>(self, f: F) -> Self::Fused;
}

#[repr(transparent)]
pub struct FusedRef<T>(T);

impl<T> Deref for FusedRef<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> DerefMut for FusedRef<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<'a, I, O> StaticFuse<'a, I, O> for &'a I {
    type Fused = FusedRef<O>;

    unsafe fn static_fuse<F: FnOnce(&'a I) -> O>(self, f: F) -> Self::Fused {
        FusedRef(f(self))
    }
}

pub struct FusedHeapPtr<T, P>(T, P);

impl<T, P> Deref for FusedHeapPtr<T, P> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T, P> DerefMut for FusedHeapPtr<T, P> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<'a, I: 'a, O> StaticFuse<'a, I, O> for Arc<I> {
    type Fused = FusedHeapPtr<O, Self>;

    unsafe fn static_fuse<F: FnOnce(&'a I) -> O>(self, f: F) -> Self::Fused {
        let shared_ref = &*(&*self as *const I);
        FusedHeapPtr(f(shared_ref), self)
    }
}

#[repr(transparent)]
pub struct WaitUntilChangedFuture<'a, 'req, R, L, T, E>(
    Option<&'a mut SensorObserver<R, ExecLock<L, T, E>>>,
    PhantomData<&'req (L, T, E)>,
)
where
    L: DataWriteLock<Target = ExecData<T, E>>,
    R: Deref<Target = RevisedData<ExecLock<L, T, E>>>,
    E: CallbackRegister<'req, T>;
// &'a R: Lockshare2<'a, Lock = ExecLock<L, T, E>>;

impl<'a, 'req, R, L, T, E> Future for WaitUntilChangedFuture<'a, 'req, R, L, T, E>
where
    L: DataWriteLock<Target = ExecData<T, E>>,
    R: Deref<Target = RevisedData<ExecLock<L, T, E>>>,
    E: CallbackRegister<'req, T>,
    // &'a R: Lockshare2<'a, Lock = ExecLock<L, T, E>>,
    Self: Unpin,
{
    type Output = ExecGuard<L::ReadGuard<'a>, T, E>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Self::Output> {
        if let Some(value) = self.0.take() {
            println!("POLLED");
            // return Poll::Ready(value.pull_updated());
            if value.has_changed() {
                println!("THIS");
                Poll::Ready(value.pull_updated())
            } else {
                print!("POLLED: NOT READY");
                value
                    .inner
                    .deref()
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
            println!("THIS?");
            Poll::Pending
        }
    }
}

impl<'a, 'req, R, L, T, E> SensorObserver<R, ExecLock<L, T, E>>
where
    L: DataWriteLock<Target = ExecData<T, E>>,
    R: Deref<Target = RevisedData<ExecLock<L, T, E>>>,
    E: CallbackRegister<'req, T>,
    // &'a R: Lockshare2<'a, Lock = ExecLock<L, T, E>>,
    // Self: 'a,
{
    fn wait_until_changed(&mut self) -> WaitUntilChangedFuture<'_, 'req, R, L, T, E> {
        WaitUntilChangedFuture(Some(self), PhantomData)
    }
}

impl<'a, R, L, T, E> SensorObserver<R, ExecLock<L, T, E>>
where
    L: DataWriteLock<Target = ExecData<T, E>>,
    R: Deref<Target = RevisedData<ExecLock<L, T, E>>>,
    E: CallbackRegister<'a, T>,
{
    fn register<F: 'a + FnMut(&T) -> bool>(&self, f: F) {
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

impl<R, L: DataWriteLock> SensorWrite for SensorWriter<R, L>
where
    for<'a> &'a R: Lockshare2<'a, Lock = L>,
{
    type Lock = L;

    #[inline(always)]
    fn mark_all_unseen(&self) {
        self.share_elided_ref().update_version(STEP_SIZE)
    }

    #[inline]
    fn update(&self, sample: L::Target) {
        let mut guard = self.share_elided_ref().data.write();
        self.mark_all_unseen();
        *guard = sample;
    }

    #[inline]
    fn modify_with(&self, f: impl FnOnce(&mut L::Target)) {
        let mut guard = self.share_elided_ref().data.write();
        self.mark_all_unseen();
        f(&mut guard);
    }

    #[inline]
    fn read(&self) -> L::ReadGuard<'_> {
        self.share_elided_ref().data.read()
    }

    #[inline]
    fn write(&self) -> L::WriteGuard<'_> {
        self.share_elided_ref().data.write()
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

    use std::{
        future,
        thread::{self, sleep},
        time::Duration,
    };

    use futures::executor::block_on;

    use crate::{
        lock::parking_lot::{
            ArcRwSensor, ArcRwSensorWriter, RwSensor, RwSensorExec, RwSensorWriter,
            RwSensorWriterExec,
        },
        SensorCallbackExec, SensorObserve, SensorWrite,
    };

    static X: RwSensorWriterExec<i32> = RwSensorWriterExec::new(5);

    #[test]
    fn test_1() {
        let s1 = ArcRwSensorWriter::new(-3);
        let s2 = RwSensorWriterExec::new(5);
        let s3 = RwSensorWriter::new(8);

        let b: RwSensorExec<_> = s2.spawn_observer();
        let z: RwSensor<_> = s3.spawn_observer();

        let handle = thread::spawn(move || {
            let xref = &X;
            let mut zz = xref.spawn_referenced_observer();
            let x = block_on(async { zz.wait_until_changed().await });
            println!("{}", *x)
        });

        X.update_exec(8);

        // println!("HANDLE JOINED");
        // let bb: ArcRwSensor<_> = s1.spawn_observer();
        // let bbc = bb.clone();

        // let mut x = s1
        //     .spawn_observer()
        //     .fuse_cached(s2.spawn_observer(), |x, y| *x + *y);

        // s2.register(|new_val| {
        //     println!("New sample {}", new_val);
        //     true
        // });

        // let d = s2.spawn_observer();

        // d.register(|new_val| {
        //     println!("New sample OBS {}", new_val);
        //     true
        // });

        // assert!(!x.has_changed());

        // assert_eq!(*x.pull(), 2);
        // s1.update(3);
        // drop(s1);
        // assert!(x.inner_a().is_closed());
        // assert!(x.has_changed());
        // assert_eq!(*x.pull(), 2);
        // assert_eq!(*x.pull_updated(), 8);
        // assert!(!x.has_changed());
        // let mut x = x.map_cached(|x| x + 2);

        // s2.update_exec(7);
        // s2.update_exec(10);

        // assert_eq!(*x.pull(), 10);

        sleep(Duration::from_secs(1));
        X.update_exec(9);
        sleep(Duration::from_secs(1));
        X.update_exec(11);
        X.update_exec(11);
        X.update_exec(11);
        X.update_exec(11);
        X.update_exec(11);
        X.update_exec(11);
        X.update_exec(11);
        // handle.join().unwrap();
        println!("HELLOPP");
    }
}
