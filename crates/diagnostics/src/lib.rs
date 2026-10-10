//! Bounded, content-free local diagnostics shared by desktop and Android.

mod event;
mod reader;
mod recorder;
mod transport_trace;

pub use event::*;
pub use reader::*;
pub use recorder::*;
pub use transport_trace::*;

#[cfg(test)]
mod tests;
