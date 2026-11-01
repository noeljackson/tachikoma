//! Authenticated, domain-independent approval control plane.
pub mod api;
pub mod store;
pub use api::Api;
pub use store::Store;
