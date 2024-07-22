pub mod lock;

use core::marker::PhantomData;
use core::{
    ops::Deref,
    sync::atomic::{AtomicUsize, Ordering},
};
use lock::{
    AtomicConvert, DataLockFactory, DataReadLock, DataWriteLock, FalseReadLock, OwnedData,
    OwnedFalseLock, ReadGuardSpecifier,
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

pub trait CallbackManager {
    type Target;

    fn register<F: 'static + FnMut(&Self::Target) -> bool>(&self, f: F);
    fn callback(&mut self, value: &Self::Target);
}

pub struct ExecData<T, E: CallbackManager> {
    exec_manager: E,
    data: T,
}

impl<T, E: CallbackManager> Deref for ExecData<T, E> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.data
    }
}

impl<T, E: CallbackManager> DerefMut for ExecData<T, E> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.data
    }
}

pub struct ExecLock<L, T, E>
where
    L: DataWriteLock<Target = ExecData<T, E>>,
    E: CallbackManager,
{
    inner: L,
    _types: PhantomData<(T, E)>,
}

impl<L, T, E> ReadGuardSpecifier for ExecLock<L, T, E>
where
    L: DataWriteLock<Target = ExecData<T, E>>,
    E: CallbackManager,
{
    type Target = T;

    type ReadGuard<'read> = ExecGuard<L::ReadGuard<'read>, T, E>   where
        Self::Target: 'read,
        Self: 'read;
}

impl<L, T, E> DataReadLock for ExecLock<L, T, E>
where
    L: DataWriteLock<Target = ExecData<T, E>>,
    E: CallbackManager,
{
    fn read(&self) -> Self::ReadGuard<'_> {
        ExecGuard {
            inner: self.inner.read(),
            _types: PhantomData,
        }
    }

    fn try_read(&self) -> Option<Self::ReadGuard<'_>> {
        self.inner.try_read().map(|guard| ExecGuard {
            inner: guard,
            _types: PhantomData,
        })
    }
}

impl<L, T, E> DataWriteLock for ExecLock<L, T, E>
where
    L: DataWriteLock<Target = ExecData<T, E>>,
    E: CallbackManager,
{
    type WriteGuard<'write> = ExecGuardMut<L::WriteGuard<'write>, T, E>
    where
        Self::Target: 'write,
        Self: 'write;

    fn write(&self) -> Self::WriteGuard<'_> {
        ExecGuardMut {
            inner: self.inner.write(),
            _types: PhantomData,
        }
    }

    fn try_write(&self) -> Option<Self::WriteGuard<'_>> {
        self.inner.try_write().map(|guard| ExecGuardMut {
            inner: guard,
            _types: PhantomData,
        })
    }
}

pub struct ExecGuard<G, T, E>
where
    G: Deref<Target = ExecData<T, E>>,
    E: CallbackManager,
{
    inner: G,
    _types: PhantomData<(T, E)>,
}

impl<G, T, E> Deref for ExecGuard<G, T, E>
where
    G: Deref<Target = ExecData<T, E>>,
    E: CallbackManager,
{
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.inner.data
    }
}

pub struct ExecGuardMut<G, T, E>
where
    G: Deref<Target = ExecData<T, E>>,
    E: CallbackManager,
{
    inner: G,
    _types: PhantomData<(T, E)>,
}

impl<G, T, E> Deref for ExecGuardMut<G, T, E>
where
    G: DerefMut<Target = ExecData<T, E>>,
    E: CallbackManager,
{
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.inner.data
    }
}

impl<G, T, E> DerefMut for ExecGuardMut<G, T, E>
where
    G: DerefMut<Target = ExecData<T, E>>,
    E: CallbackManager,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner.data
    }
}

impl<G, T, E> AtomicConvert for ExecGuardMut<G, T, E>
where
    G: DerefMut<Target = ExecData<T, E>>,
    E: CallbackManager,
    G: AtomicConvert,
{
    type Target = ExecGuard<<G as AtomicConvert>::Target, T, E>;

    fn convert(self) -> Self::Target {
        todo!()
    }
}

pub trait Lockshare<'share, 'state> {
    type Lock: DataWriteLock + 'state;
    type Shared: Deref<Target = RevisedData<Self::Lock>> + 'state;

    /// Construct a new shareable lock from the given lock.
    fn new(lock: Self::Lock) -> Self;

    /// Share the revised data for the largest possible lifetime.
    fn share_lock(&'share self) -> Self::Shared;

    /// There is probably a better way than this to get an elided reference
    /// to the revised data.
    fn share_elided_ref(&self) -> &RevisedData<Self::Lock>;
}

pub struct SensorWriter<'share, 'state, R>(R, PhantomData<(&'share Self, &'state ())>)
where
    R: Lockshare<'share, 'state>;

impl<'share, 'state, R> Drop for SensorWriter<'share, 'state, R>
where
    R: Lockshare<'share, 'state>,
{
    fn drop(&mut self) {
        self.0
            .share_elided_ref()
            .version
            .fetch_or(CLOSED_BIT, Ordering::Release);
    }
}

impl<'share, 'state, R> Deref for SensorWriter<'share, 'state, R>
where
    R: Lockshare<'share, 'state>,
{
    type Target = R;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<'share, 'state, R> DerefMut for SensorWriter<'share, 'state, R>
where
    R: Lockshare<'share, 'state>,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<'share, 'state, R> SensorWriter<'share, 'state, R>
where
    R: Lockshare<'share, 'state>,
{
    // #[inline(always)]
    // pub fn new_from<LF: DataLockFactory<Lock = R::Lock>>(
    //     init: <<R::Lock as LockRef>::Lock as ReadGuardSpecifier>::Target,
    // ) -> Self {
    //     Self(R::new(LF::new(init)), PhantomData)
    // }

    #[inline(always)]
    pub fn spawn_referenced_observer(&self) -> SensorObserver<&RevisedData<R::Lock>> {
        SensorObserver {
            inner: self.0.share_elided_ref(),
            version: self.0.share_elided_ref().version(),
        }
    }

    #[inline(always)]
    fn spawn_observer(&'share self) -> SensorObserver<R::Shared> {
        let inner = self.0.share_lock();
        SensorObserver {
            version: inner.version(),
            inner,
        }
    }
}

// impl<'share, 'state, R, L: 'state> SensorWriter<'share, 'state, R>
// where
//     L: DataWriteLock + DataLockFactory<Lock = L>,
//     R: RevisedDataLockshare<'share, 'state, Lock = L>,
// {
//     #[inline(always)]
//     pub fn new(init: <R::Lock as ReadGuardSpecifier>::Target) -> Self {
//         Self::new_from::<R::Lock>(init)
//     }
// }

// pub struct SensorWriterExec<'share, 'state, R>
// where
//     R: RevisedDataLockshare<'share, 'state>,
// {
//     inner: SensorWriter<'share, 'state, R>,
//     exec: Vec<Box<dyn FnMut(&<R::Lock as ReadGuardSpecifier>::Target) -> bool>>,
// }

// impl<'share, 'state, R> SensorWriterExec<'share, 'state, R>
// where
//     R: RevisedDataLockshare<'share, 'state>,
// {
//     #[inline(always)]
//     pub fn register<F: 'static + FnMut(&<R::Lock as ReadGuardSpecifier>::Target) -> bool>(
//         &mut self,
//         f: F,
//     ) {
//         self.exec.push(Box::new(f));
//     }

//     /// Execute all functions with the given value and remove those that returned false.
//     /// Although this function uses an immutable borrow, IT SHOULD ONLY EVER BE CALLED WHEN IT IS SAFE TO
//     /// TO BORROW `exec` MUTABLY.
//     #[inline(always)]
//     unsafe fn execute_inner(&self, val: &<R::Lock as ReadGuardSpecifier>::Target) {
//         let mut i = 0;
//         let mut len = self.exec.len();
//         let ptr_slice = self.exec.as_ptr().cast_mut();
//         while i < len {
//             let func = ptr_slice.add(i);
//             if (*func)(val) {
//                 i += 1;
//             } else {
//                 len -= 1;
//                 std::ptr::swap(func, ptr_slice.add(len));
//             }
//         }
//         (*std::ptr::from_ref(&self.exec).cast_mut()).truncate(len);
//     }
// }

// impl<'share, 'state, R> Deref for SensorWriterExec<'share, 'state, R>
// where
//     R: Lockshare<'share, 'state>,
// {
//     type Target = SensorWriter<'share, 'state, R>;

//     #[inline(always)]
//     fn deref(&self) -> &Self::Target {
//         &self.inner
//     }
// }

// There must be a way to do this sharing without having to have a second lifetime in the trait,
// but the author is not experienced enough.
impl<'share, L: 'share> Lockshare<'share, 'share> for RevisedData<L>
where
    L: DataWriteLock,
{
    type Lock = L;
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

impl<'state, L: 'state> Lockshare<'_, 'state> for Arc<RevisedData<L>>
where
    L: DataWriteLock,
{
    type Lock = L;
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
pub struct SensorObserver<R> {
    inner: R,
    version: usize,
}

impl<R, L> SensorObserver<R>
where
    R: Deref<Target = RevisedData<L>>,
{
    #[inline(always)]
    fn is_closed(&self) -> bool {
        self.inner.version() & CLOSED_BIT == CLOSED_BIT
    }
}

trait SensorWrite {
    type Target;

    fn update(&self, sample: Self::Target);
    /// Modify the sensor value in place and notify observers.
    fn modify_with(&self, f: impl FnOnce(&mut Self::Target));
    /// Mark the current sensor value as unseen to all observers.
    fn mark_all_unseen(&self);
}

trait CallbackSignal {}

trait SensorCallbackWrite {
    type Target;
    /// Update the sensor's current value, notify observers and execute all registered functions.
    fn update_exec(&self, sample: Self::Target);
    /// Modify the sensor value in place, notify observers and execute all registered functions.
    fn modify_with_exec(&self, f: impl FnOnce(&mut Self::Target));

    fn exec(&self);
    // TODO
    fn register(&self);
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

    fn map<T, F>(self, f: F) -> MappedSensorObserver<Self, T, F>
    where
        Self: Sized,
        F: FnMut(&<<Self as SensorObserve>::Lock as ReadGuardSpecifier>::Target) -> T,
    {
        MappedSensorObserver::map_with(self, f)
    }

    fn map_cached<T, F>(self, f: F) -> MappedSensorObserverCached<Self, T, F>
    where
        Self: Sized,
        F: FnMut(&<<Self as SensorObserve>::Lock as ReadGuardSpecifier>::Target) -> T,
    {
        MappedSensorObserverCached::map_with(self, f)
    }
}

impl<'share, 'state, R> SensorWrite for SensorWriter<'share, 'state, R>
where
    R: Lockshare<'share, 'state>,
    R::Lock: DataWriteLock,
{
    type Target = <R::Lock as ReadGuardSpecifier>::Target;

    #[inline(always)]
    fn mark_all_unseen(&self) {
        self.share_elided_ref().update_version(STEP_SIZE)
    }

    #[inline]
    fn update(&self, sample: Self::Target) {
        let mut guard = self.share_elided_ref().data.write();
        self.mark_all_unseen();
        *guard = sample;
    }

    #[inline]
    fn modify_with(&self, f: impl FnOnce(&mut Self::Target)) {
        let mut guard = self.share_elided_ref().data.write();
        self.mark_all_unseen();
        f(guard);
    }
}

impl<'share, 'state, R, L, T, E> SensorCallbackWrite for SensorWriter<'share, 'state, R>
where
    R: Lockshare<'share, 'state, Lock = ExecLock<L, T, E>>,
    L: DataWriteLock<Target = ExecData<T, E>>,
    L::WriteGuard: AtomicConvert<'write, Target = L::ReadGuard<'write>>,
    E: CallbackManager,
{
    type Target = T;
    fn update_exec(&self, sample: Self::Target) {
        let mut guard = self.share_elided_ref().data.write();
        *guard = sample;
        self.mark_all_unseen();
        let guard = guard.convert();

        // We have a mutable reference to the sensor
        unsafe { (*core::ptr::from_ref(&guard.inner.exec_manager).cast_mut()).execute() };
        // guard.inner.exec_manager.execute();
    }

    fn modify_with_exec(&self, f: impl FnOnce(&mut <Self::Lock as ReadGuardSpecifier>::Target)) {
        todo!()
    }

    fn exec(&self) {
        todo!()
    }

    fn register(&self) {
        todo!()
    }
}

// impl<T, L, S> SensorIn for S
// where
//     L: DataWriteLock<Target = T>,
//     S: Deref<Target = RevisedData<L>> + ?Sized,
// {
//     type Lock = L;

//     #[inline(always)]
//     fn write(&self) -> L::WriteGuard<'_> {
//         self.data.write()
//     }

//     #[inline(always)]
//     fn read(&self) -> L::ReadGuard<'_> {
//         self.data.read()
//     }

//     #[inline(always)]
//     fn mark_all_unseen(&self) {
//         self.update_version(STEP_SIZE)
//     }
// }

impl<'share, 'state, T, L, R> SensorObserve for SensorObserver<R>
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
        lock::parking_lot::{ArcRwSensor, ArcRwSensorWriter, RwSensorWriter},
        Lockshare, MappedSensorObserver, SensorObserve, SensorWrite,
    };

    // #[test]
    // fn test_1() {
    //     let s1 = ArcRwSensorWriter::new(-3);
    //     let s2 = RwSensorWriterExec::new(5);

    //     let bb: ArcRwSensor<i32> = s1.spawn_observer();

    //     let mut x = s1
    //         .spawn_observer()
    //         .fuse_cached(s2.spawn_observer(), |x, y| *x + *y);

    //     assert!(!x.has_changed());

    //     assert_eq!(*x.pull(), 2);
    //     s1.update(3);
    //     drop(s1);
    //     assert!(x.inner_a().is_closed());
    //     assert!(x.has_changed());
    //     assert_eq!(*x.pull(), 2);
    //     assert_eq!(*x.pull_updated(), 8);
    //     assert!(!x.has_changed());
    //     let mut x = x.map_cached(|x| x + 2);

    //     assert_eq!(*x.pull(), 10);
    // }
}
