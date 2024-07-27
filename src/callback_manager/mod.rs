pub mod standard;

pub trait CallbackManager {
    type Target;

    fn register<F: 'static + FnMut(&Self::Target) -> bool>(&mut self, f: F);
    fn callback(&mut self, value: &Self::Target);
}
