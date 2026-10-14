//! The lock that lets one guest thread at a time run guest code (ADR-0018):
//! in lockstep, and with `ARMOR_GIL` (ADR-0023).
//!
//! A ticket lock: threads are served in the order they asked, so a thread
//! that ends its block of instructions and asks again waits behind the
//! others. `std::sync::Mutex` makes no such promise, and with it a guest
//! thread spinning on a lock could keep the GIL while the thread holding
//! that lock never runs.

use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard};

pub struct Gil {
    next_ticket: AtomicU64,
    serving: Mutex<u64>,
    turn: Condvar,
}

/// Holding the GIL; released when dropped.
pub struct GilGuard<'a> {
    gil: &'a Gil,
}

impl Gil {
    pub const fn new() -> Gil {
        Gil {
            next_ticket: AtomicU64::new(0),
            serving: Mutex::new(0),
            turn: Condvar::new(),
        }
    }

    pub fn lock(&self) -> GilGuard<'_> {
        let ticket = self.next_ticket.fetch_add(1, Ordering::Relaxed);
        let mut serving = self.serving.lock().expect("GIL poisoned");
        while *serving != ticket {
            serving = self.turn.wait(serving).expect("GIL poisoned");
        }
        GilGuard { gil: self }
    }
}

thread_local! {
    /// The GIL's turn counter, held across a fork by the forking thread.
    static FORKING: RefCell<Option<MutexGuard<'static, u64>>> = const { RefCell::new(None) };
}

/// Around a fork of a guest with several threads (armor_linux::process):
/// the forking thread takes the GIL, then holds its turn counter so that
/// no other thread is inside `lock` or a guard's drop at the fork. The
/// child, with only that thread, skips the tickets of the threads it does
/// not have.
impl armor_linux::process::ForkHooks for Gil {
    fn prepare(&'static self) {
        std::mem::forget(self.lock());
        let turn = self.serving.lock().expect("GIL poisoned");
        FORKING.with(|f| *f.borrow_mut() = Some(turn));
    }

    fn parent(&'static self) {
        if let Some(mut turn) = FORKING.with(|f| f.borrow_mut().take()) {
            *turn += 1;
            self.turn.notify_all();
        }
    }

    fn child(&'static self) {
        if let Some(mut turn) = FORKING.with(|f| f.borrow_mut().take()) {
            *turn = self.next_ticket.load(Ordering::Relaxed);
        }
    }
}

impl Drop for GilGuard<'_> {
    fn drop(&mut self) {
        let mut serving = self.gil.serving.lock().expect("GIL poisoned");
        *serving += 1;
        self.gil.turn.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn threads_are_served_in_turn() {
        let gil = Arc::new(Gil::new());
        let order = Arc::new(Mutex::new(Vec::new()));
        let guard = gil.lock();
        let handles: Vec<_> = (0..4)
            .map(|i| {
                let (mine, order) = (Arc::clone(&gil), Arc::clone(&order));
                let handle = std::thread::spawn(move || {
                    let _turn = mine.lock();
                    order.lock().unwrap().push(i);
                });
                // Let thread i take its ticket before thread i + 1.
                while gil.next_ticket.load(Ordering::Relaxed) < i + 2 {
                    std::thread::yield_now();
                }
                handle
            })
            .collect();
        drop(guard);
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(*order.lock().unwrap(), vec![0, 1, 2, 3]);
    }
}
