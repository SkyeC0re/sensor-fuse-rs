#[cfg(feature = "alloc")]
pub mod standard;

use core::{cell::UnsafeCell, marker::PhantomData};

use crate::lock::DataWriteLock;

trait Sealed {}

pub trait ExecutionStrategy<T>: Sealed {
    fn execute(&self, value: &T);
}

pub trait RegistrationStrategy<F>: Sealed {
    fn register<L: DataWriteLock>(data: &(L, Self), f: F);
}

#[repr(transparent)]
pub struct AccessStrategyImmut<T, E: ExecManager<T>>(E, PhantomData<T>);

impl<T, E> AccessStrategyImmut<T, E>
where
    E: ExecManager<T>,
{
    #[inline(always)]
    pub const fn new(exec_manager: E) -> Self {
        Self(exec_manager, PhantomData)
    }
}

impl<T, E> Sealed for AccessStrategyImmut<T, E> where E: ExecManager<T> {}
impl<T, E> ExecutionStrategy<T> for AccessStrategyImmut<T, E>
where
    E: ExecManager<T>,
{
    fn execute(&self, value: &T) {
        self.0.execute(value)
    }
}

#[repr(transparent)]
pub struct AccessStrategyMut<T, E: ExecManagerMut<T>>(UnsafeCell<E>, PhantomData<T>);

unsafe impl<T, E> Sync for AccessStrategyMut<T, E> where E: ExecManagerMut<T> {}

impl<T, E> AccessStrategyMut<T, E>
where
    E: ExecManagerMut<T>,
{
    #[inline(always)]
    pub const fn new(exec_manager: E) -> Self {
        Self(UnsafeCell::new(exec_manager), PhantomData)
    }
}

impl<T, E> Sealed for AccessStrategyMut<T, E> where E: ExecManagerMut<T> {}
impl<T, E> ExecutionStrategy<T> for AccessStrategyMut<T, E>
where
    E: ExecManagerMut<T>,
{
    fn execute(&self, value: &T) {
        unsafe {
            (*self.0.get()).execute(value);
        }
    }
}

pub trait ExecManagerMut<T> {
    /// Execute all executables registered on the executor manager.
    fn execute(&mut self, value: &T);
}

impl<T> ExecManager<T> for () {
    #[inline(always)]
    fn execute(&self, _: &T) {}
}

pub trait ExecRegisterMut<F> {
    /// Register an executable unit on the callback manager's execution set. In most cases this will usually be a function.
    fn register(&mut self, f: F);
}

impl<T, F, E> RegistrationStrategy<F> for AccessStrategyMut<T, E>
where
    E: ExecManagerMut<T> + ExecRegisterMut<F>,
{
    #[inline]
    fn register<L: DataWriteLock>(data: &(L, Self), f: F) {
        // Piggyback off of the data's semaphore.
        let guard = L::atomic_downgrade(data.0.write());
        unsafe {
            (*data.1 .0.get()).register(f);
        }
        drop(guard);
    }
}

pub trait ExecManager<T> {
    /// Execute all executables registered on the executor manager.
    fn execute(&self, value: &T);
}

pub trait ExecRegister<F> {
    /// Register an executable unit on the callback manager's execution set. In most cases this will usually be a function.
    fn register(&self, f: F);
}

impl<T, F, E> RegistrationStrategy<F> for AccessStrategyImmut<T, E>
where
    E: ExecManager<T> + ExecRegister<F>,
{
    #[inline(always)]
    fn register<L: DataWriteLock>(data: &(L, Self), f: F) {
        data.1 .0.register(f);
    }
}
