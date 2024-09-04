// #[cfg(feature = "std")]
// pub mod vec_box;

use core::{
    cell::UnsafeCell,
    ops::{Deref, DerefMut},
};
use std::marker::PhantomData;

use derived_deref::{Deref, DerefMut};

use crate::lock::{DataReadLock, DataWriteLock, ReadGuardSpecifier};

trait Sealed {}

pub trait ExecutionStrategy<T>: Sealed {
    fn execute(&self, value: &T);
}

pub trait RegistrationStrategy<T, F>: ExecutionStrategy<T> {
    fn register(&self, f: F);
}

#[repr(transparent)]
pub struct AccessStrategyImmut<T, E: ExecManager<T>>(E, PhantomData<T>);

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

impl<T> ExecManagerMut<T> for () {
    #[inline(always)]
    fn execute(&mut self, _: &T) {}
}

pub trait ExecRegisterMut<F> {
    /// Register an executable unit on the callback manager's execution set. In most cases this will usually be a function.
    fn register(&mut self, f: F);
}

impl<T, F, E> RegistrationStrategy<T, F> for AccessStrategyMut<T, E>
where
    E: ExecManagerMut<T> + ExecRegisterMut<F>,
{
    fn register(&self, f: F) {
        unsafe {
            (*self.0.get()).register(f);
        }
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

impl<T, F, E> RegistrationStrategy<T, F> for AccessStrategyImmut<T, E>
where
    E: ExecManager<T> + ExecRegister<F>,
{
    fn register(&self, f: F) {
        self.0.register(f);
    }
}
