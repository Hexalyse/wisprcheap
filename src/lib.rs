//! wisprcheap: push-to-talk dictation. Hold a hotkey, speak, and the cleaned-up text is pasted
//! where you type. Windows and Linux (X11, and Wayland through the input devices), with a tray icon.

pub mod logging;

pub mod app;
pub mod audio;
pub mod cli;
pub mod clipboard;
pub mod command;
pub mod config;
pub mod dictionary;
pub mod env_edit;
pub mod history;
pub mod hotkey;
pub mod icons;
pub mod instance;
pub mod keyboard;
pub mod lang;
pub mod llm;
pub mod output;
pub mod paths;
pub mod polish;
pub mod pricing;
pub mod recorder;
pub mod sounds;
pub mod state;
pub mod sync;
pub mod transcribe;
pub mod tray;
pub mod yaml_edit;
