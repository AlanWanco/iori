mod sequencial;
pub use sequencial::SequencialDownloader;

mod parallel;
pub use parallel::{ParallelDownloader, spawn_ctrlc_handler};

mod app;
pub use app::*;
