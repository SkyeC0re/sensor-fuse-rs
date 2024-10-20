//! # Sensor-Fuse
//! An un-opinionated no-std compatible observer/callback framework for the opinionated user.
//!
//! Designed as generalization to [Tokio's watch channel](https://docs.rs/tokio/latest/tokio/sync/watch/index.html), Sensor-Fuse aims
//! to provide a modular framework for implementing observer or observer + callback patterns. From the perspective of this crate sensor data can
//! be defined as the following:
//! - The raw data.
//! - A callback executor which does some work whenever the raw data is updated.
//! - An atomic version counter.
//! - A locking strategy that allows atomic access to the raw data and callback executor.
//! - An optional heap pointer that wraps all of the above.
//!
//! Given this definition, Tokio's watch channel only allows for the generalization of the raw data type, however
//! Sensor-Fuse additionally allows for the generalization of the callback executor, the locking strategy and optional heap pointer as well.
//! In fact, Tokio's watch channel within this framework is roughly equivalent to selecting `std::sync::RwLock` as the locking strategy,
//! and the standard `VecBox` executor (the core requirement here being that the executor must be able to accept `Waker`s) with
//! an `Arc` pointer wrapping everything.
//!
//! ```
//! extern crate sensor_fuse;
//! use sensor_fuse::{prelude::*, lock::parking_lot::RwSensorWriterExec};
//!
//! let writer = RwSensorWriterExec::new(3);
//!
//! writer.register(Box::new(|x: &_| {
//!     println!("{}", x);
//!     // The standard executor uses a boolean return value to determine whether or not to keep the
//!     // function in its execution set. To keep the function forever, we just unconditionally return true.
//!     true
//! }));
//!
//! let mut observer = writer.spawn_observer();
//! assert_eq!(*observer.pull(), 3);
//!
//! // Prints "5".
//! writer.update(5);
//!
//! assert_eq!(*observer.pull(), 5);
//! ```
//!
//! ## Executors and Executables
//!
//! Within the framework, every sensor is associated with an executor which allows executables to be registered. In this context
//! an executable can be anything but fundamentally represents a piece of work that must occur each time the sensor value is updated. The framework's basic
//! non-trivial executor (`struct@StdExec`) for example allows for both the registration of `Waker`s to power asynchronous behaviour and
//! callback functions that act on references to the sensor value. Each time the sensor value is updated, `struct@StdExec` will execute all `Waker`s and
//! callbacks, dropping all the Wakers and those callbacks that produced `false` as output values.
//!

#![warn(bad_style)]
#![warn(missing_docs)]
#![warn(trivial_casts)]
#![warn(trivial_numeric_casts)]
#![warn(unused)]
#![warn(unused_extern_crates)]
#![warn(unused_import_braces)]
#![warn(unused_qualifications)]
#![warn(unused_results)]

#[cfg(feature = "alloc")]
extern crate alloc;

pub mod executor;
pub mod lock;
pub mod prelude;
pub mod sensor_core;

#[cfg(feature = "alloc")]
use alloc::sync::Arc;
use core::{
    cell::UnsafeCell,
    future::{poll_fn, Future},
    ops::Deref,
    pin::pin,
    sync::atomic::{AtomicUsize, Ordering},
    task::{Poll, Waker},
};
use derived_deref::{Deref, DerefMut};
use executor::{ExecManager, ExecRegister};
use std::marker::PhantomData;

use crate::lock::{DataReadLock, DataWriteLock, OwnedData, OwnedFalseLock, ReadGuardSpecifier};

pub type SymResult<T> = Result<T, T>;

/*** Sensor State ***/

// All credit to [Tokio's Watch Channel](https://docs.rs/tokio/latest/tokio/sync/watch/index.html). If it's not broken don't fix it.
const CLOSED_BIT: usize = 1;
const STEP_SIZE: usize = 2;

/// Data structure for storing data with an atomic version tag.
pub struct RawSensorData<L, E>
where
    L: DataWriteLock,
    E: ExecManager<L>,
{
    lock: L,
    executor: E,
    version: AtomicUsize,
    writers: AtomicUsize,
    observers: AtomicUsize,
}

impl<T, L, E> RawSensorData<L, E>
where
    L: DataWriteLock<Target = T>,
    E: ExecManager<L>,
{
    #[inline(always)]
    const fn new_for_writer(lock: L, executor: E) -> Self {
        Self {
            lock,
            executor,
            version: AtomicUsize::new(0),
            writers: AtomicUsize::new(1),
            observers: AtomicUsize::new(0),
        }
    }
}
/// Trait for sharing a (most likely heap pointer) wrapped `struct@RawSensorData` with a locking strategy.
pub trait ShareStrategy<'a> {
    /// The data type that the sensor stores.
    type Target;
    /// The locking mechanism for the sensor's data.
    type Lock: DataWriteLock<Target = Self::Target>;
    /// The executor associated with the sensor.
    type Executor: ExecManager<Self::Lock>;
    /// The wrapping container for the sensor state.
    type Shared: Deref<Target = RawSensorData<Self::Lock, Self::Executor>>;

    /// Share the sensor state for the largest possible lifetime.
    fn share_data(self) -> Self::Shared;

    /// Create an immutable borrow to the sensor state, elided by the lifetime of
    /// wrapping container.
    fn share_elided_ref(self) -> &'a RawSensorData<Self::Lock, Self::Executor>;
}

impl<'a, L, E> ShareStrategy<'a> for &'a RawSensorData<L, E>
where
    L: DataWriteLock,
    E: ExecManager<L>,
{
    type Target = L::Target;
    type Lock = L;
    type Executor = E;
    type Shared = Self;

    #[inline(always)]
    fn share_data(self) -> Self {
        self
    }

    #[inline(always)]
    fn share_elided_ref(self) -> Self {
        self
    }
}

/// QOL trait to encapsulate sensor observer's data wrapper, locking strategy and executor type.
pub trait DerefSensorData: Deref<Target = RawSensorData<Self::Lock, Self::Executor>> {
    /// The data type that the sensor stores.
    type Target;
    /// The locking mechanism for the sensor's data.
    type Lock: DataWriteLock<Target = <Self as DerefSensorData>::Target>;
    /// The executor associated with the sensor.
    type Executor: ExecManager<Self::Lock>;
}

impl<L, E, R> DerefSensorData for R
where
    L: DataWriteLock,
    E: ExecManager<L>,
    R: Deref<Target = RawSensorData<L, E>>,
{
    type Target = L::Target;
    type Lock = L;
    type Executor = E;
}

#[cfg(feature = "alloc")]
impl<'a, L, E> ShareStrategy<'a> for &'a Arc<RawSensorData<L, E>>
where
    L: DataWriteLock,
    E: ExecManager<L>,
{
    type Target = L::Target;
    type Lock = L;
    type Executor = E;
    type Shared = Arc<RawSensorData<L, E>>;

    #[inline(always)]
    fn share_data(self) -> Self::Shared {
        self.clone()
    }

    #[inline(always)]
    fn share_elided_ref(self) -> &'a RawSensorData<L, E> {
        self
    }
}

/*** Sensor Writing ***/

/// The generalized sensor writer.
#[repr(transparent)]
pub struct SensorWriter<T, S>(S)
where
    for<'a> &'a S: ShareStrategy<'a, Target = T>;

impl<L, E> SensorWriter<L::Target, RawSensorData<L, E>>
where
    L: DataWriteLock,
    E: ExecManager<L>,
{
    /// Create a new sensor writer by wrapping the appropriate shared data.
    #[inline(always)]
    pub const fn from_parts(lock: L, executor: E) -> Self {
        Self(RawSensorData::new_for_writer(lock, executor))
    }
}

#[cfg(feature = "alloc")]
impl<L, E> SensorWriter<L::Target, Arc<RawSensorData<L, E>>>
where
    L: DataWriteLock,
    E: ExecManager<L>,
{
    /// Create a new sensor writer by wrapping the appropriate shared data.
    #[inline(always)]
    pub fn from_parts_arc(lock: L, executor: E) -> Self {
        Self(Arc::new(RawSensorData::new_for_writer(lock, executor)))
    }
}

impl<T, S> Drop for SensorWriter<T, S>
where
    for<'a> &'a S: ShareStrategy<'a, Target = T>,
{
    fn drop(&mut self) {
        let data = <&S as ShareStrategy<'_>>::share_elided_ref(&self.0);
        if data.writers.fetch_sub(1, Ordering::Relaxed) == 1 {
            let _ = data.version.fetch_or(CLOSED_BIT, Ordering::Release);
        }
    }
}

impl<T, S> Clone for SensorWriter<T, S>
where
    S: Clone,
    for<'a> &'a S: ShareStrategy<'a, Target = T>,
{
    fn clone(&self) -> Self {
        let _ = self
            .0
            .share_elided_ref()
            .writers
            .fetch_add(1, Ordering::Relaxed);
        Self(self.0.clone())
    }
}

impl<T, S> SensorWriter<T, S>
where
    for<'a> &'a S: ShareStrategy<'a, Target = T>,
{
    /// Acquire a read lock on the underlying data.
    #[inline(always)]
    pub fn read(&self) -> <<&S as ShareStrategy<'_>>::Lock as ReadGuardSpecifier>::ReadGuard<'_> {
        self.0.share_elided_ref().lock.read()
    }

    /// Acquire a write lock on the underlying data.
    #[inline(always)]
    pub fn write(&self) -> <<&S as ShareStrategy<'_>>::Lock as DataWriteLock>::WriteGuard<'_> {
        self.0.share_elided_ref().lock.write()
    }

    /// Update the sensor value, notify observers and execute all registered callbacks.
    #[inline(always)]
    pub fn update(&self, sample: T) {
        self.modify_with(|v| *v = sample)
    }

    /// Modify the sensor value in place, notify observers and execute all registered callbacks.
    #[inline]
    pub fn modify_with(&self, f: impl FnOnce(&mut T)) {
        let sensor_state = self.0.share_elided_ref();
        let mut guard = sensor_state.lock.write();

        // Atomic downgrade just occurred. No other modification can happen.
        sensor_state.executor.execute(|| {
            f(&mut guard);
            let _ = sensor_state.version.fetch_add(STEP_SIZE, Ordering::Release);
            guard
        });
    }

    /// Mark the current sensor value as unseen to all observers, notify them and execute all registered callbacks.
    #[inline(always)]
    pub fn mark_all_unseen(&self) {
        self.modify_with(|_| ())
    }

    /// Spawn an observer by immutably borrowing the sensor writer's data. By definition this observer's scope will be limited
    /// by the scope of the writer.
    #[inline(always)]
    pub fn spawn_referenced_observer(
        &self,
    ) -> SensorObserver<
        T,
        &'_ RawSensorData<<&S as ShareStrategy<'_>>::Lock, <&S as ShareStrategy<'_>>::Executor>,
    > {
        let inner = self.0.share_elided_ref();
        let _ = inner.observers.fetch_add(1, Ordering::Relaxed);
        SensorObserver {
            inner,
            version: UnsafeCell::new(inner.version.load(Ordering::Acquire)),
        }
    }

    /// Spawn an observer by leveraging the sensor writer's sharing strategy. May allow the observer
    /// to outlive the sensor writer for an appropriate sharing strategy (such as if the writer wraps its data in an `Arc`).
    #[inline(always)]
    pub fn spawn_observer(&self) -> SensorObserver<T, <&'_ S as ShareStrategy>::Shared> {
        let inner = self.0.share_data();
        let _ = inner.observers.fetch_add(1, Ordering::Relaxed);
        SensorObserver {
            version: UnsafeCell::new(inner.version.load(Ordering::Acquire)),
            inner,
        }
    }
}

/*** Sensor Observation ***/

/// General observer functionality.
pub trait SensorObserve {
    /// The underlying locking mechanism associated with this observer.
    type Lock: ReadGuardSpecifier;

    /// Returns the latest value obtainable by the sensor and marks it as seen.
    fn pull(&mut self) -> <Self::Lock as ReadGuardSpecifier>::ReadGuard<'_>;
    /// Returns the latest value obtainable by the sensor without marking it as seen.
    fn borrow(&self) -> <Self::Lock as ReadGuardSpecifier>::ReadGuard<'_>;
    /// Mark the current sensor data as seen.
    fn mark_seen(&mut self);
    /// Mark the current sensor data as unseen.
    fn mark_unseen(&mut self);
    /// Returns true if the sensor data has been marked as unseen.
    fn has_changed(&self) -> bool;
    /// Returns true if all upstream writers has been dropped and no more updates can occur.
    fn is_closed(&self) -> bool;
    /// Returns true if this observer has observed that all upstream writers has been dropped.
    /// This may return false even if all upstream writers have been dropped, but is guaranteed to
    /// be updated whenever a call to the following is made:
    /// - `function@pull`
    /// - `function@mark_seen`
    /// - `function@mark_unseen`
    /// - `function@has_changed`
    /// - `function@is_closed`
    /// - `function@wait_until_changed` is awaited.
    /// - `function@wait_for` is awaited.
    fn is_locally_closed(&self) -> bool;

    /// Fuse this observer with another using the given fusion function into a cacheless observer.
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

    /// Map this observer into another using the given mapping function into a cacheless observer.
    #[inline(always)]
    fn map<T, F>(self, f: F) -> MappedSensorObserver<Self, T, F>
    where
        Self: Sized,
        F: FnMut(&<<Self as SensorObserve>::Lock as ReadGuardSpecifier>::Target) -> T,
    {
        MappedSensorObserver::map_with(self, f)
    }
}

#[repr(transparent)]
#[derive(Deref, DerefMut)]
struct SendSafe<T>(T);

unsafe impl<T> Send for SendSafe<T> {}

struct WaitForFut<
    'a,
    R: SensorObserveAsync + ?Sized,
    C: Send + FnMut(&<R::Lock as ReadGuardSpecifier>::Target) -> bool,
    F: Future<Output = SymResult<()>> + Unpin,
    G: Unpin + FnMut(&'a mut R) -> F,
> where
    R::Lock: 'a,
{
    observer: SendSafe<*mut R>,
    condition: C,
    wait_changed_generator: G,
    wait_changed: Option<F>,
    is_closed: bool,
    _lifetime: PhantomData<&'a mut R>,
}

impl<
        'a,
        R: SensorObserveAsync + ?Sized,
        C: Send + FnMut(&<R::Lock as ReadGuardSpecifier>::Target) -> bool,
        F: Future<Output = SymResult<()>> + Unpin,
        G: Unpin + FnMut(&'a mut R) -> F,
    > Future for WaitForFut<'a, R, C, F, G>
{
    type Output = SymResult<<R::Lock as ReadGuardSpecifier>::ReadGuard<'a>>;

    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        // Safety: Only the condition function `C` can be `Unpin`, and none of its data gets moved out.
        let s = unsafe { self.get_unchecked_mut() };

        if let Some(future) = &mut s.wait_changed {
            if let Poll::Ready(res) = pin!(future).poll(cx) {
                // Should get compiled into a value assignment.
                s.is_closed = if res.is_err() { true } else { false };
            } else {
                return Poll::Pending;
            }
        }

        loop {
            let guard = unsafe { (**s.observer).pull() };
            if (s.condition)(&guard) {
                // Should get compiled into a value assignment.
                return Poll::Ready(if s.is_closed { Err(guard) } else { Ok(guard) });
            }
            let mut future = (s.wait_changed_generator)(unsafe { &mut **s.observer });
            if let Poll::Ready(res) = pin!(&mut future).poll(cx) {
                // Should get compiled into a value assignment.
                s.is_closed = if res.is_err() { true } else { false };
                continue;
            } else {
                s.wait_changed = Some(future);
                break;
            }
        }

        return Poll::Pending;
    }
}

/// Async observer functionality.
pub trait SensorObserveAsync: SensorObserve {
    /// Asynchronously wait until the sensor value is updated. This call will **not** update the observer's version,
    /// as such an additional call to `pull` or `pull_updated` is required.
    fn wait_until_changed(&mut self) -> impl Future<Output = SymResult<()>> + Unpin + Sized;

    /// Asynchronously wait until the sensor value has been updated with a value that satisfies a condition. This call **will** update the observer's version
    /// and evaluate the condition function on values obtained by `pull_updated`.
    fn wait_for<F: Send + FnMut(&<Self::Lock as ReadGuardSpecifier>::Target) -> bool>(
        &mut self,
        condition: F,
    ) -> impl Future<Output = SymResult<<Self::Lock as ReadGuardSpecifier>::ReadGuard<'_>>> {
        return WaitForFut {
            observer: SendSafe(self),
            condition,
            is_closed: self.is_locally_closed(),
            wait_changed_generator: |r| Self::wait_until_changed(r),
            wait_changed: None,
            _lifetime: PhantomData,
        };
    }
}

/// The generalized sensor observer.
pub struct SensorObserver<T, R>
where
    R: DerefSensorData<Target = T>,
{
    inner: R,
    version: UnsafeCell<usize>,
}

impl<T, R> Drop for SensorObserver<T, R>
where
    R: DerefSensorData<Target = T>,
{
    fn drop(&mut self) {
        let _ = self.inner.observers.fetch_sub(1, Ordering::Relaxed);
    }
}

impl<T, R> Clone for SensorObserver<T, R>
where
    R: DerefSensorData<Target = T> + Clone,
{
    fn clone(&self) -> Self {
        let _ = self.inner.observers.fetch_add(1, Ordering::Relaxed);
        Self {
            inner: self.inner.clone(),
            version: UnsafeCell::new(unsafe { *self.version.get() }),
        }
    }
}

impl<T, R> SensorObserve for SensorObserver<T, R>
where
    R: DerefSensorData<Target = T>,
{
    type Lock = R::Lock;

    #[inline(always)]
    fn borrow(&self) -> <Self::Lock as ReadGuardSpecifier>::ReadGuard<'_> {
        self.inner.lock.read()
    }

    #[inline(always)]
    fn pull(&mut self) -> <Self::Lock as ReadGuardSpecifier>::ReadGuard<'_> {
        let guard = self.inner.lock.read();
        *self.version.get_mut() = self.inner.version.load(Ordering::Acquire);
        guard
    }

    #[inline(always)]
    fn mark_seen(&mut self) {
        *self.version.get_mut() = self.inner.version.load(Ordering::Acquire);
    }

    #[inline(always)]
    fn mark_unseen(&mut self) {
        *self.version.get_mut() = self
            .inner
            .version
            .load(Ordering::Acquire)
            .wrapping_sub(STEP_SIZE);
    }

    #[inline(always)]
    fn has_changed(&self) -> bool {
        let latest_version = self.inner.version.load(Ordering::Acquire);
        let version = unsafe { &mut *self.version.get() };
        *version |= latest_version & CLOSED_BIT;
        *version != latest_version
    }

    #[inline(always)]
    fn is_closed(&self) -> bool {
        unsafe {
            *self.version.get() |= self.inner.version.load(Ordering::Acquire) & CLOSED_BIT;
        }
        self.is_locally_closed()
    }

    #[inline(always)]
    fn is_locally_closed(&self) -> bool {
        unsafe { *self.version.get() & CLOSED_BIT == CLOSED_BIT }
    }
}

impl<T, R> SensorObserveAsync for SensorObserver<T, R>
where
    for<'a> R::Executor: ExecRegister<&'a Waker>,
    R: DerefSensorData<Target = T>,
{
    fn wait_until_changed(&mut self) -> impl Future<Output = SymResult<()>> + Unpin + Sized {
        poll_fn(move |cx| {
            let mut has_changed = self.has_changed();
            let mut closed = self.is_locally_closed();
            if has_changed || closed {
                // Should get compiled out into value assignment
                return Poll::Ready(if closed { Err(()) } else { Ok(()) });
            }

            if !self.register_if(|| {
                has_changed = self.has_changed();
                closed = self.is_locally_closed();
                if has_changed || closed {
                    return None;
                }
                Some(cx.waker())
            }) {
                // Should get compiled out into value assignment
                return Poll::Ready(if closed { Err(()) } else { Ok(()) });
            }

            Poll::Pending
        })
    }
}

/*** Mapped and Fused Observers ***/

/// A mapped sensor observer.
pub struct MappedSensorObserver<
    A: SensorObserve,
    T,
    F: FnMut(&<A::Lock as ReadGuardSpecifier>::Target) -> T,
> {
    /// The original pre-mapped observer.
    pub inner: A,
    map: UnsafeCell<F>,
}

impl<A, T, F> MappedSensorObserver<A, T, F>
where
    A: SensorObserve,
    F: FnMut(&<A::Lock as ReadGuardSpecifier>::Target) -> T,
{
    /// Create a new mapped observer given another observer and an appropriate mapping function.
    #[inline(always)]
    pub const fn map_with(a: A, f: F) -> Self {
        Self {
            inner: a,
            map: UnsafeCell::new(f),
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
    fn pull(&mut self) -> <Self::Lock as ReadGuardSpecifier>::ReadGuard<'_> {
        OwnedData((*self.map.get_mut())(&self.inner.pull()))
    }

    #[inline(always)]
    fn borrow(&self) -> <Self::Lock as ReadGuardSpecifier>::ReadGuard<'_> {
        unsafe { OwnedData((*self.map.get())(&self.inner.borrow())) }
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
    fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }

    #[inline(always)]
    fn is_locally_closed(&self) -> bool {
        self.inner.is_locally_closed()
    }
}

impl<A, T, F> SensorObserveAsync for MappedSensorObserver<A, T, F>
where
    A: SensorObserve + SensorObserveAsync,
    F: FnMut(&<A::Lock as ReadGuardSpecifier>::Target) -> T,
{
    fn wait_until_changed(&mut self) -> impl Future<Output = SymResult<()>> + Unpin {
        self.inner.wait_until_changed()
    }
}

/// A fused sensor observer.
pub struct FusedSensorObserver<
    A: SensorObserve,
    B: SensorObserve,
    T,
    F: FnMut(&<A::Lock as ReadGuardSpecifier>::Target, &<B::Lock as ReadGuardSpecifier>::Target) -> T,
> {
    /// The first original pre-mapped observer.
    pub a: A,
    /// The second original pre-mapped observer.
    pub b: B,
    fuse: UnsafeCell<F>,
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
    /// Create a new cacheless fused observer given two other independent observers and an appropriate fusing function.
    #[inline(always)]
    pub fn fuse_with(a: A, b: B, f: F) -> Self {
        Self {
            a,
            b,
            fuse: UnsafeCell::new(f),
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
    fn pull(&mut self) -> <Self::Lock as ReadGuardSpecifier>::ReadGuard<'_> {
        unsafe { OwnedData((*self.fuse.get())(&self.a.pull(), &self.b.pull())) }
    }

    #[inline(always)]
    fn borrow(&self) -> <Self::Lock as ReadGuardSpecifier>::ReadGuard<'_> {
        unsafe { OwnedData((*self.fuse.get())(&self.a.borrow(), &self.b.borrow())) }
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
    fn is_closed(&self) -> bool {
        self.a.is_closed() && self.b.is_closed()
    }

    #[inline(always)]
    fn is_locally_closed(&self) -> bool {
        self.a.is_locally_closed() && self.b.is_locally_closed()
    }
}

impl<A, B, T, F> SensorObserveAsync for FusedSensorObserver<A, B, T, F>
where
    A: SensorObserve + SensorObserveAsync,
    B: SensorObserve + SensorObserveAsync,
    F: FnMut(
        &<A::Lock as ReadGuardSpecifier>::Target,
        &<B::Lock as ReadGuardSpecifier>::Target,
    ) -> T,
{
    fn wait_until_changed(&mut self) -> impl Future<Output = SymResult<()>> + Unpin {
        let mut a = self.a.wait_until_changed();
        let mut b = self.b.wait_until_changed();
        poll_fn(move |cx| {
            if let Poll::Ready(res) = pin!(&mut a).poll(cx) {
                return Poll::Ready(res);
            }
            if let Poll::Ready(res) = pin!(&mut b).poll(cx) {
                return Poll::Ready(res);
            }

            return Poll::Pending;
        })
    }
}

/*** Executables and Async ***/

/// Allows registration of an executable.
pub trait RegisterFunction<F> {
    /// Unconditionally register an executable.
    #[inline(always)]
    fn register(&self, f: F) {
        let _ = self.register_if(|| Some(f));
    }

    /// Conditionally register a function and returns whether or not it was registered.
    fn register_if<C: FnOnce() -> Option<F>>(&self, condition: C) -> bool;
}

impl<T, S, F> RegisterFunction<F> for SensorWriter<T, S>
where
    for<'a> &'a S: ShareStrategy<'a, Target = T>,
    for<'a> <&'a S as ShareStrategy<'a>>::Executor: ExecRegister<F>,
{
    #[inline]
    fn register_if<C: FnOnce() -> Option<F>>(&self, condition: C) -> bool {
        let inner_data = self.0.share_elided_ref();
        inner_data.executor.register(condition)
    }
}

impl<T, R, F> RegisterFunction<F> for SensorObserver<T, R>
where
    R: DerefSensorData<Target = T>,
    R::Executor: ExecRegister<F>,
{
    #[inline]
    fn register_if<C: FnOnce() -> Option<F>>(&self, condition: C) -> bool {
        let inner_data = self.inner.share_elided_ref();
        inner_data.executor.register(condition)
    }
}
