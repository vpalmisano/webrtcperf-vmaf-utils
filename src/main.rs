use clap::Parser;
use env_logger;
use webrtcperf_vmaf_utils::{process_video, watermark_video};

/// Utility for processing real time videos for VMAF evaluation
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// When set, the video will be prepared adding a timestamp overlay
    #[arg(short, long, default_value_t = String::new())]
    watermark: String,

    /// The id to write on the watermark
    #[arg(long, default_value_t = String::new())]
    watermark_id: String,

    /// When set, the video will be processed recognizing the timestamp overlay and setting the frames pts accordingly
    #[arg(short, long, default_value_t = String::new())]
    process: String,
}
fn main() {
    env_logger::init();
    let args = Args::parse();

    let (sender, receiver) = crossbeam_channel::unbounded();

    ctrlc::set_handler(move || {
        sender.send("stop").expect("Error sending signal");
    })
    .expect("Error setting Ctrl-C handler");

    if !args.watermark.is_empty() {
        println!("watermark video: {}", args.watermark);
        if let Err(e) = watermark_video(&args.watermark, &args.watermark_id, receiver) {
            eprintln!("Error watermarking video: {}", e);
        }
    } else if !args.process.is_empty() {
        println!("process video: {}", args.process);
        if let Err(e) = process_video(&args.process, receiver) {
            eprintln!("Error processing video: {}", e);
        }
    } else {
        eprintln!("No action specified");
        std::process::exit(1);
    }
}
