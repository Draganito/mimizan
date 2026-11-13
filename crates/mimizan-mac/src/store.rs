//! The App Store purchase of the Mac App Store build.
//!
//! The work is in `swift/store.swift`, which the build script compiles
//! and links: StoreKit 2 is Swift alone. These are its C functions as
//! Rust ones. Every one of them may be called from any thread; the state
//! behind them is locked on the Swift side.

use std::ffi::{c_char, c_void};
use std::sync::Once;

extern "C" {
    fn mimizan_store_start(notify: extern "C" fn(*mut c_void), context: *mut c_void);
    fn mimizan_store_state() -> u32;
    fn mimizan_store_price(buffer: *mut c_char, capacity: usize) -> usize;
    fn mimizan_store_message(buffer: *mut c_char, capacity: usize) -> usize;
    fn mimizan_store_clear_message();
    fn mimizan_store_purchase();
    fn mimizan_store_restore();
}

const OWNED: u32 = 1;
const BUSY: u32 = 2;

type Notify = Box<dyn Fn() + Send + Sync>;

extern "C" fn notified(context: *mut c_void) {
    // SAFETY: `context` is the `Notify` leaked in `start`, never freed
    // and never changed; the Swift side hands it back as it got it.
    let notify = unsafe { &*context.cast::<Notify>() };
    notify();
}

/// Ask the store what the account owns, load the product and listen for
/// purchases made elsewhere. `notify` is called, from any thread, when
/// what the functions below answer has changed. The first call counts;
/// later ones do nothing.
pub fn start(notify: impl Fn() + Send + Sync + 'static) {
    static STARTED: Once = Once::new();
    STARTED.call_once(|| {
        let context = Box::into_raw(Box::new(Box::new(notify) as Notify));
        // SAFETY: `notified` reads `context` as the `Notify` it is, and
        // the allocation lives as long as the program.
        unsafe { mimizan_store_start(notified, context.cast()) }
    });
}

/// The account owns the purchase: bought here, on the iPhone, or shared
/// by the family. Before the store has answered, the answer of the last
/// run.
pub fn owned() -> bool {
    // SAFETY: no arguments, no pointers.
    unsafe { mimizan_store_state() & OWNED != 0 }
}

/// A purchase or a restore is under way.
pub fn busy() -> bool {
    // SAFETY: no arguments, no pointers.
    unsafe { mimizan_store_state() & BUSY != 0 }
}

/// The price as the store shows it in the user's currency. Absent until
/// the store has answered.
pub fn price() -> Option<String> {
    text(mimizan_store_price)
}

/// What happened last, for the screen.
pub fn message() -> Option<String> {
    text(mimizan_store_message)
}

pub fn clear_message() {
    // SAFETY: no arguments, no pointers.
    unsafe { mimizan_store_clear_message() }
}

/// The system's purchase sheet. Returns at once; the result arrives
/// through `notify`.
pub fn purchase() {
    // SAFETY: no arguments, no pointers.
    unsafe { mimizan_store_purchase() }
}

/// Apple's "Restore Purchases". Returns at once.
pub fn restore() {
    // SAFETY: no arguments, no pointers.
    unsafe { mimizan_store_restore() }
}

fn text(read: unsafe extern "C" fn(*mut c_char, usize) -> usize) -> Option<String> {
    let mut buffer = [0u8; 512];
    // SAFETY: the Swift side writes at most `capacity` bytes into the
    // buffer and returns how many it wrote.
    let written = unsafe { read(buffer.as_mut_ptr().cast(), buffer.len()) }.min(buffer.len());
    (written > 0).then(|| String::from_utf8_lossy(&buffer[..written]).into_owned())
}
