use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::Arc;

#[derive(Debug)]
pub struct Sender(Arc<AtomicBool>);

#[derive(Clone, Debug)]
pub struct Receiver(Arc<AtomicBool>);

pub fn channel() -> (Sender, Receiver) {
    let arc1 = Arc::new(AtomicBool::new(true));
    let arc2 = arc1.clone();
    (Sender(arc1), Receiver(arc2))
}

impl Drop for Sender {
    fn drop(&mut self) {
        self.0.store(false, Relaxed);
    }
}

impl Receiver {
    pub fn is_alive(&self) -> bool {
        self.0.load(Relaxed)
    }
}
