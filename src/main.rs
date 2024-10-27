extern crate ffmpeg_next as ffmpeg;

use std::collections::HashMap;
use std::time::Instant;
use regex::Regex;

use ffmpeg_next::{
    codec, decoder, encoder, format, frame, log, media, picture, threading, Dictionary, Packet, Rational
};
use image::DynamicImage;
use tesseract_rs::{TesseractAPI, TessPageSegMode};
use clap::Parser;

struct Transcoder {
    ost_index: usize,
    decoder: decoder::Video,
    input_time_base: Rational,
    encoder: encoder::Video,
    logging_enabled: bool,
    frame_count: usize,
    last_log_frame_count: usize,
    starting_time: Instant,
    last_log_time: Instant,
    frame_re: Regex,
    failed_frames: usize,
    tesseract: TesseractAPI,
}

impl Transcoder {
    fn new(
        ist: &format::stream::Stream,
        octx: &mut format::context::Output,
        ost_index: usize,
        tesseract_path: &str,
        enable_logging: bool,
    ) -> Result<Self, ffmpeg::Error> {
        let global_header = octx.format().flags().contains(format::Flags::GLOBAL_HEADER);
        let decoder = codec::context::Context::from_parameters(ist.parameters())?
            .decoder()
            .video()?;

        let codec = encoder::find(codec::Id::VP8);
        let mut ost = octx.add_stream(codec)?;

        let mut encoder =
            codec::context::Context::new_with_codec(codec.ok_or(ffmpeg::Error::InvalidData)?)
                .encoder()
                .video()?;
        ost.set_parameters(&encoder);
        encoder.set_height(decoder.height());
        encoder.set_width(decoder.width());
        encoder.set_aspect_ratio(decoder.aspect_ratio());
        encoder.set_format(decoder.format());
        encoder.set_frame_rate(decoder.frame_rate());
        encoder.set_time_base(ist.time_base());
        encoder.set_bit_rate(20000);
        encoder.set_threading(threading::Config::count(0));
        encoder.set_gop(1);

        if global_header {
            encoder.set_flags(codec::Flags::GLOBAL_HEADER);
        }

        let encoder_opts = parse_opts("quality=best,cpu-used=0,crf=1,qmin=1,qmax=10,kf-min-dist=1,kf-max-dist=1".to_owned()).unwrap();
        let opened_encoder = encoder
            .open_with(encoder_opts)
            .expect("error opening encoder with supplied settings");
        ost.set_parameters(&opened_encoder);

        let tesseract = TesseractAPI::new();
        tesseract.init(tesseract_path, "eng").unwrap();
        tesseract.set_variable("tessedit_char_whitelist", "0123456789-").unwrap();
        tesseract.set_page_seg_mode(TessPageSegMode::PSM_SINGLE_LINE).unwrap();

        Ok(Self {
            ost_index,
            decoder,
            input_time_base: ist.time_base(),
            encoder: opened_encoder,
            logging_enabled: enable_logging,
            frame_count: 0,
            last_log_frame_count: 0,
            starting_time: Instant::now(),
            last_log_time: Instant::now(),
            frame_re: Regex::new(r"(?<id>[0-9]{1,3})-(?<time>[0-9]{1,13})").unwrap(),
            failed_frames: 0,
            tesseract,
        })
    }

    fn send_packet_to_decoder(&mut self, packet: &Packet) {
        self.decoder.send_packet(packet).unwrap();
    }

    fn send_eof_to_decoder(&mut self) {
        self.decoder.send_eof().unwrap();
    }

    fn receive_and_process_decoded_frames(
        &mut self,
        octx: &mut format::context::Output,
        ost_time_base: Rational,
    ) {
        let mut frame = frame::Video::empty();

        while self.decoder.receive_frame(&mut frame).is_ok() {
            self.frame_count += 1;
            let timestamp = frame.timestamp().unwrap_or(0);
            self.log_progress(f64::from(
                Rational(timestamp as i32, 1) * self.input_time_base,
            ));

            let mut rgb_frame = frame::Video::empty();
            ffmpeg::software::scaling::context::Context::get(
                self.decoder.format(),
                self.decoder.width(),
                self.decoder.height(),
                ffmpeg::format::Pixel::RGB24,
                self.decoder.width(),
                self.decoder.height(),
                ffmpeg::software::scaling::Flags::BILINEAR,
            )
            .unwrap()
            .run(&frame, &mut rgb_frame)
            .unwrap();

            let image_data = rgb_frame.data(0);
            let image = DynamicImage::ImageRgb8(
                image::RgbImage::from_raw(
                    self.decoder.width(),
                    self.decoder.height(),
                    image_data.to_vec(),
                )
                .expect("Failed to create RgbImage from raw data"),
            );
            let image = image.crop_imm(0, 0, image.width(), 
                (image.height() as f32 / 15f32) as u32);

            self.tesseract.set_image(&image.to_rgb8(), image.width() as i32, image.height() as i32, 
                3i32, 3i32 * frame.width() as i32).unwrap();
            let output = self.tesseract.get_utf8_text().unwrap();

            if ! self.frame_re.captures(&output.trim()).map_or_else(|| {
                println!("failed to recognize: {:?}", output.trim());
                false
            }, |c| {
                let id: i32 = c["id"].parse().unwrap();
                let time: f64 = c["time"].parse().unwrap_or(0f64) / 1000f64;
                let pts_new = (time / f64::from(self.input_time_base)) as i64;
                if cfg!(debug_assertions) {
                    println!("  pts={:?} id={:?} time={:?} pts_new={:?}", frame.pts(), id, time, pts_new);
                }
                frame.set_pts(Some(pts_new));
                frame.set_kind(picture::Type::I);
                self.send_frame_to_encoder(&frame);
                self.receive_and_process_encoded_packets(octx, ost_time_base);
                true
            }) {
                self.failed_frames += 1;
            }
        }
    }

    fn send_frame_to_encoder(&mut self, frame: &frame::Video) {
        self.encoder.send_frame(frame).unwrap();
    }

    fn send_eof_to_encoder(&mut self) {
        self.encoder.send_eof().unwrap();
    }

    fn receive_and_process_encoded_packets(
        &mut self,
        octx: &mut format::context::Output,
        ost_time_base: Rational,
    ) {
        let mut encoded = Packet::empty();
        while self.encoder.receive_packet(&mut encoded).is_ok() {
            encoded.set_stream(self.ost_index);
            encoded.rescale_ts(self.input_time_base, ost_time_base);
            encoded.write_interleaved(octx).unwrap();
        }
    }

    fn log_progress(&mut self, timestamp: f64) {
        if !self.logging_enabled
            || (self.frame_count - self.last_log_frame_count < 100
                && self.last_log_time.elapsed().as_secs_f64() < 1.0)
        {
            return;
        }
        eprintln!(
            "frame: {} timestamp: {:.2} failed frames: {}",
            self.frame_count,
            timestamp,
            self.failed_frames,
        );
        self.last_log_frame_count = self.frame_count;
        self.last_log_time = Instant::now();
    }
}

fn parse_opts<'a>(s: String) -> Option<Dictionary<'a>> {
    let mut dict = Dictionary::new();
    for keyval in s.split_terminator(',') {
        let tokens: Vec<&str> = keyval.split('=').collect();
        match tokens[..] {
            [key, val] => dict.set(key, val),
            _ => return None,
        }
    }
    Some(dict)
}

/// Utility for processing real time videos for VMAF evaluation.
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// The video file to process.
    #[arg(short, long)]
    process_file: String,

    /// The tesseract data path (optional).
    #[arg(short, long, default_value_t = String::from("/usr/share/tesseract-ocr/5/tessdata"))]
    tesseract_path: String,
}
fn main() {
    let args = Args::parse();
    let output_file = Regex::new(r"(\..+)$").unwrap().replace(&args.process_file, ".ivf").to_string();
    println!("processing: {} -> {}", args.process_file, output_file);

    // Initialize ffmpeg.
    if let Err(e) = ffmpeg::init() {
        eprintln!("Failed to initialize ffmpeg: {}", e);
        return;
    }
    log::set_level(log::Level::Info);

    let mut ictx = match format::input(&args.process_file) {
        Ok(context) => context,
        Err(e) => {
            eprintln!("Failed to open input file: {}", e);
            return;
        }
    };
    let mut octx = match format::output(&output_file) {
        Ok(context) => context,
        Err(e) => {
            eprintln!("Failed to open output file: {}", e);
            return;
        }
    };

    //format::context::input::dump(&ictx, 0, Some(&args.process_file));

    let best_video_stream_index = ictx
        .streams()
        .best(media::Type::Video)
        .map(|stream| stream.index());
    let mut stream_mapping: Vec<isize> = vec![0; ictx.nb_streams() as _];
    let mut ist_time_bases = vec![Rational(0, 0); ictx.nb_streams() as _];
    let mut ost_time_bases = vec![Rational(0, 0); ictx.nb_streams() as _];
    let mut transcoders = HashMap::new();
    let mut ost_index = 0;
    for (ist_index, ist) in ictx.streams().enumerate() {
        let ist_medium = ist.parameters().medium();
        if ist_medium != media::Type::Audio
            && ist_medium != media::Type::Video
            && ist_medium != media::Type::Subtitle
        {
            stream_mapping[ist_index] = -1;
            continue;
        }
        stream_mapping[ist_index] = ost_index;
        ist_time_bases[ist_index] = ist.time_base();
        if ist_medium == media::Type::Video {
            // Initialize transcoder for video stream.
            transcoders.insert(
                ist_index,
                Transcoder::new(
                    &ist,
                    &mut octx,
                    ost_index as _,
                    &args.tesseract_path,
                    Some(ist_index) == best_video_stream_index,
                )
                .unwrap(),
            );
        } else {
            // Set up for stream copy for non-video stream.
            let mut ost = octx.add_stream(encoder::find(codec::Id::None)).unwrap();
            ost.set_parameters(ist.parameters());
            // We need to set codec_tag to 0 lest we run into incompatible codec tag
            // issues when muxing into a different container format. Unfortunately
            // there's no high level API to do this (yet).
            unsafe {
                (*ost.parameters().as_mut_ptr()).codec_tag = 0;
            }
        }
        ost_index += 1;
    }

    octx.set_metadata(ictx.metadata().to_owned());
    // format::context::output::dump(&octx, 0, Some(&output_file));
    let mut movflags_opts = Dictionary::new();
    movflags_opts.set("movflags", "faststart");
    octx.write_header_with(movflags_opts).unwrap();

    for (ost_index, _) in octx.streams().enumerate() {
        ost_time_bases[ost_index] = octx.stream(ost_index as _).unwrap().time_base();
    }

    for (stream, mut packet) in ictx.packets() {
        let ist_index = stream.index();
        let ost_index = stream_mapping[ist_index];
        if ost_index < 0 {
            continue;
        }
        let ost_time_base = ost_time_bases[ost_index as usize];
        match transcoders.get_mut(&ist_index) {
            Some(transcoder) => {
                transcoder.send_packet_to_decoder(&packet);
                transcoder.receive_and_process_decoded_frames(&mut octx, ost_time_base);
            }
            None => {
                // Do stream copy on non-video streams.
                packet.rescale_ts(ist_time_bases[ist_index], ost_time_base);
                packet.set_position(-1);
                packet.set_stream(ost_index as _);
                packet.write_interleaved(&mut octx).unwrap();
            }
        }
    }

    // Flush encoders and decoders.
    for (ost_index, transcoder) in transcoders.iter_mut() {
        let ost_time_base = ost_time_bases[*ost_index];
        transcoder.send_eof_to_decoder();
        transcoder.receive_and_process_decoded_frames(&mut octx, ost_time_base);
        transcoder.send_eof_to_encoder();
        transcoder.receive_and_process_encoded_packets(&mut octx, ost_time_base);
    }

    octx.write_trailer().unwrap();
}
