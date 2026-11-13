//! Kiro UI interface (egui). The same code runs in the desktop window and, compiled
//! to wasm, in the browser; [`Backend`] is the seam between the two.

mod app;
pub mod backend;
mod chat;
mod input;
mod model;
mod settings;
mod theme;
mod view;

pub use app::KiroApp;
pub use backend::{allowed_url, Backend};
