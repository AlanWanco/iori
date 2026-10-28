#!/bin/sh
#![allow(unused_attributes)] /*
                             OUT=/tmp/tmp && rustc "$0" -o ${OUT} && exec ${OUT} $@ || exit $? #*/

use std::fs;
use std::io::Result;
use std::process::Command;

fn run_build(script: &str, branch: &str) -> Result<()> {
    let status = Command::new(script).arg(branch).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "{script} failed with {status}"
        )))
    }
}

fn main() -> Result<()> {
    let target = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "x86_64-apple-darwin".to_string());
    let branch = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "release/7.1".to_string());

    if fs::metadata("tmp").is_ok() {
        return Ok(());
    }

    #[cfg(target_os = "linux")]
    {
        if target == "x86_64-pc-windows-gnu" {
            run_build("./build/windows_ffmpeg_cross.rs", &branch)?;
        } else {
            run_build("./build/linux_ffmpeg.rs", &branch)?;
        }
    }

    // Cross compile on macOS
    #[cfg(target_os = "macos")]
    {
        if target == "x86_64-pc-windows-gnu" {
            run_build("./build/windows_ffmpeg_cross.rs", &branch)?;
        } else if target == "x86_64-apple-darwin" {
            run_build("./build/macos_ffmpeg_cross.rs", &branch)?;
        } else {
            run_build("./build/linux_ffmpeg.rs", &branch)?;
        }
    }

    Ok(())
}
