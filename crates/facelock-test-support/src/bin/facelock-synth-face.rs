//! Write the loopback tier's synthetic face sequence as raw video.
//!
//! `facelock-synth-face <dir>` writes `ir.y8` (GREY, one byte per pixel) and
//! `rgb.yuyv` (YUYV 4:2:2 with neutral chroma) — the whole loop, frames
//! back to back, at the geometry `facelock_test_support::synthetic_face`
//! fixes — plus `frame-00.pgm` for a look at what the camera will see.
//! `test/loopback/run-loopback-tier.sh` hands the two raw files to ffmpeg.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use facelock_test_support::synthetic_face::{
    FRAMES, HEIGHT, WIDTH, mono_to_yuyv, render_sequence, to_pgm,
};

fn main() -> ExitCode {
    let Some(dir) = std::env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("usage: facelock-synth-face <output-dir>");
        return ExitCode::from(2);
    };
    match write(&dir) {
        Ok(()) => {
            println!("{WIDTH}x{HEIGHT} {FRAMES} frames -> {}", dir.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("facelock-synth-face: {e}");
            ExitCode::FAILURE
        }
    }
}

fn write(dir: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    let frames = render_sequence();
    let mut ir = Vec::with_capacity(frames.len() * (WIDTH * HEIGHT) as usize);
    let mut rgb = Vec::with_capacity(ir.capacity() * 2);
    for frame in &frames {
        ir.extend_from_slice(frame);
        rgb.extend_from_slice(&mono_to_yuyv(frame));
    }
    fs::write(dir.join("ir.y8"), &ir)?;
    fs::write(dir.join("rgb.yuyv"), &rgb)?;
    fs::write(dir.join("frame-00.pgm"), to_pgm(WIDTH, HEIGHT, &frames[0]))?;
    Ok(())
}
