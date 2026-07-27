//! Moss engine library.
//!
//! The core (agent loop, LLM client, tools, state, memory) is UI-free and
//! usable both from the `moss` CLI binary and as an embedded cdylib
//! (`libmoss.so`) loaded into the Moss Terminal (kitty) process via `ffi`.
//!
//! Interactive/terminal-only modules are gated behind the `cli` feature so
//! the embedded build carries no raw-mode, audio, or process-exit hazards.

pub mod agent;
pub mod alarm;
pub mod clipboard;
pub mod config;
pub mod default_kb;
pub mod default_models;
pub mod envfile;
pub mod i18n;
pub mod llm;
pub mod logging;
pub mod memory;
pub mod models_cache;
pub mod paths;
pub mod prompts;
pub mod question;
pub mod render;
pub mod state;
pub mod token_counter;
pub mod token_estimate;
pub mod tools;

pub mod ffi;

#[cfg(feature = "cli")]
pub mod cli;
#[cfg(feature = "cli")]
pub mod config_tui;
#[cfg(feature = "cli")]
pub mod question_tui;
#[cfg(feature = "cli")]
pub mod shell;
