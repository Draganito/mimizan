//! The one purchase of the Mac App Store build.
//!
//! That build gives the darkroom and the JPEG at full size away and sells
//! the TIFF and the negative for another program, once, through the App
//! Store: the purchase the iPhone sells, valid on both. It is the `store`
//! feature, which only the Mac App Store workflow sets. Every other build,
//! the Mac download on GitHub among them, has this type with nothing
//! behind it: nothing is locked and nothing is asked.

/// The export at `path` is part of the purchase: the negative always, the
/// print when it is not a JPEG. The path decides, as it decides the format
/// the worker writes.
pub fn is_paid(linear: bool, path: &std::path::Path) -> bool {
    linear || !mimizan_core::print::is_jpeg_path(path)
}

#[cfg(not(feature = "store"))]
pub struct Unlock;

#[cfg(not(feature = "store"))]
impl Unlock {
    pub fn new(_ctx: &egui::Context) -> Self {
        Self
    }

    /// The TIFF and the negative wait for the purchase. Never, here.
    pub fn locked(&self) -> bool {
        false
    }

    pub fn ask(&mut self) {}

    pub fn open(&self) -> bool {
        false
    }

    pub fn show(&mut self, _ctx: &egui::Context) {}
}

#[cfg(feature = "store")]
pub use with_store::Unlock;

#[cfg(feature = "store")]
mod with_store {
    use egui::RichText;

    /// The App Store, through StoreKit 2.
    #[cfg(target_os = "macos")]
    use mimizan_mac::store as backend;

    /// No App Store on this system. The feature still compiles here, so
    /// the locked window and its question are tested and photographed
    /// where the program is written. `MIMIZAN_STORE_STUB=owned` is a
    /// bought copy; any other value is the price the button shows.
    #[cfg(not(target_os = "macos"))]
    mod backend {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::OnceLock;

        static ASKED: AtomicBool = AtomicBool::new(false);

        fn stub() -> Option<&'static str> {
            static STUB: OnceLock<Option<String>> = OnceLock::new();
            STUB.get_or_init(|| std::env::var("MIMIZAN_STORE_STUB").ok().filter(|v| !v.is_empty())).as_deref()
        }

        pub fn start(_notify: impl Fn() + Send + Sync + 'static) {}

        pub fn owned() -> bool {
            stub() == Some("owned")
        }

        pub fn busy() -> bool {
            false
        }

        pub fn price() -> Option<String> {
            stub().filter(|v| *v != "owned").map(str::to_string)
        }

        pub fn message() -> Option<String> {
            ASKED.load(Ordering::Relaxed).then(|| "The App Store is on the Mac alone.".to_string())
        }

        pub fn clear_message() {
            ASKED.store(false, Ordering::Relaxed);
        }

        pub fn purchase() {
            ASKED.store(true, Ordering::Relaxed);
        }

        pub fn restore() {
            ASKED.store(true, Ordering::Relaxed);
        }
    }

    /// What the purchase is, in the words of the iPhone's question.
    const OFFER: &str = "The darkroom and the JPEG at full size are free. One purchase, once, unlocks the \
                         TIFF, at 16 and at 8 bit, and the negative for another program: the linear TIFF \
                         and the linear DNG. Shared with your family and with the iPhone.";
    const OWNED: &str = "The TIFF and the negative are unlocked.";

    pub struct Unlock {
        /// The question is on screen.
        asked: bool,
    }

    impl Unlock {
        /// Asks the store what the account owns. `ctx` is painted again
        /// when the answer, the price or a purchase arrives.
        pub fn new(ctx: &egui::Context) -> Self {
            let ctx = ctx.clone();
            backend::start(move || ctx.request_repaint());
            Self { asked: false }
        }

        /// The TIFF and the negative wait for the purchase.
        pub fn locked(&self) -> bool {
            !backend::owned()
        }

        /// Put the question on screen.
        pub fn ask(&mut self) {
            self.asked = true;
        }

        /// The question is on screen and has the keyboard.
        pub fn open(&self) -> bool {
            self.asked
        }

        /// The question: what the purchase is, the price as the store
        /// names it, and Apple's Restore.
        pub fn show(&mut self, ctx: &egui::Context) {
            if !self.asked {
                return;
            }
            let owned = backend::owned();
            let busy = backend::busy();
            let mut close = false;
            egui::Modal::new(egui::Id::new("unlock")).show(ctx, |ui| {
                ui.set_width(380.0);
                ui.label(RichText::new("TIFF and Negative").strong());
                ui.add_space(6.0);
                ui.label(if owned { OWNED } else { OFFER });
                if let Some(message) = backend::message() {
                    ui.add_space(6.0);
                    ui.label(RichText::new(message).weak());
                }
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if !owned {
                        let unlock = match backend::price() {
                            Some(price) => format!("Unlock · {price}"),
                            None => "Unlock".to_string(),
                        };
                        if ui.add_enabled(!busy, egui::Button::new(RichText::new(unlock).strong())).clicked()
                        {
                            backend::purchase();
                        }
                        if ui
                            .add_enabled(!busy, egui::Button::new("Restore"))
                            .on_hover_text("Bought before, on this Mac, on the iPhone or by the family.")
                            .clicked()
                        {
                            backend::restore();
                        }
                    }
                    if ui.button("Close").clicked() {
                        close = true;
                    }
                    if busy {
                        ui.spinner();
                    }
                });
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    close = true;
                }
            });
            if close {
                self.asked = false;
                backend::clear_message();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn the_jpeg_is_free_and_the_rest_is_the_purchase() {
        for free in ["a.jpg", "a.JPG", "a.jpeg", "b/c.Jpeg"] {
            assert!(!is_paid(false, Path::new(free)), "{free}");
        }
        for paid in ["a.tif", "a.tiff", "a.TIF", "a.dng", "a", "a.jpg.tif", "a.png"] {
            assert!(is_paid(false, Path::new(paid)), "{paid}");
        }
        // The negative is the purchase under any name.
        for negative in ["a.tif", "a.dng", "a.jpg"] {
            assert!(is_paid(true, Path::new(negative)), "{negative}");
        }
    }

    #[cfg(not(feature = "store"))]
    #[test]
    fn a_build_without_the_store_locks_nothing() {
        let ctx = egui::Context::default();
        let mut unlock = Unlock::new(&ctx);
        assert!(!unlock.locked());
        unlock.ask();
        assert!(!unlock.open(), "nothing to ask without a store");
    }

    #[cfg(all(feature = "store", not(target_os = "macos")))]
    #[test]
    fn the_store_build_is_locked_until_the_store_says_owned() {
        let ctx = egui::Context::default();
        let mut unlock = Unlock::new(&ctx);
        if std::env::var_os("MIMIZAN_STORE_STUB").is_none() {
            assert!(unlock.locked());
        }
        assert!(!unlock.open());
        unlock.ask();
        assert!(unlock.open());
    }
}
