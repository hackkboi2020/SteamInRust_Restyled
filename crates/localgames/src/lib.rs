//! # Local games
//!
//! Everything about games that don't come from Steam: a library you add
//! programs to (or import from a folder, itch.io or Minecraft), art and
//! descriptions fetched from the internet, and LAN discovery.
//!
//! The modules `id`, `exe`, `text` and `lan` use only the standard library on
//! purpose: they hold the logic that must be exactly right and are easy to
//! test in isolation.

pub mod exe;
pub mod fetch;
pub mod id;
pub mod lan;
pub mod library;
pub mod scan;
pub mod text;

pub use library::{Art, LocalGame, Meta, Source, Store};
