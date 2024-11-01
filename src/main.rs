mod encoder;
mod transcoder;

use clap::Parser;
use encoder::{process_video, watermark_video};

/// Utility for processing real time videos for VMAF evaluation
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// When set, the video will be prepared adding a timestamp overlay
    #[arg(short, long, default_value_t = String::new())]
    watermark: String,

    /// When set, the video will be processed recognizing the timestamp overlay and setting the frames pts accordingly
    #[arg(short, long, default_value_t = String::new())]
    process: String,
}
fn main() {
    let args = Args::parse();

    if !args.watermark.is_empty() {
        println!("watermark video: {}", args.watermark);
        if let Err(e) = watermark_video(&args.watermark) {
            eprintln!("Error watermarking video: {}", e);
        }
    }
    if !args.process.is_empty() {
        println!("process video: {}", args.process);
        if let Err(e) = process_video(&args.process) {
            eprintln!("Error processing video: {}", e);
        }
    }
}
