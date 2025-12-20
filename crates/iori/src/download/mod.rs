mod sequential;
pub use sequential::SequentialDownloader;

mod parallel;
pub use parallel::{ParallelDownloader, spawn_ctrlc_handler};

mod app;
pub use app::*;
