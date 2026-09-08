pub mod application;
pub mod debug;
pub mod domain;
pub mod error;
#[cfg(feature = "native")]
pub mod infrastructure;

pub use application::service::ArcRelayService;
pub use error::{Error, ErrorKind, Result};

// Headless database fixtures for transport integration tests. This feature never
// links native clipboard, input, OCR, or window-management backends.
#[cfg(all(feature = "test-support", not(feature = "native")))]
pub mod infrastructure {
    #[allow(dead_code)]
    mod clipboard_store;
    pub mod clipboard_test_support;
}
