extern crate ffmpeg_next as ffmpeg;

use ffmpeg_next::{
    codec, decoder, encoder, filter, format, frame, threading, Dictionary, Packet, Rational,
};
use flate2::read::GzDecoder;
use image::DynamicImage;
use log::debug;
use regex::Regex;
use std::time::Instant;
use tesseract_rs::{TessPageSegMode, TesseractAPI};

pub struct VideoFilter {
    _filter_graph: ffmpeg::filter::Graph,
    filter_in: filter::context::Context,
    filter_out: filter::context::Context,
}

impl VideoFilter {
    pub fn new(
        input: &format::stream::Stream,
        decoder: &decoder::Video,
        desc: String,
    ) -> Result<Self, ffmpeg::Error> {
        let mut filter_graph = ffmpeg::filter::Graph::new();
        let args = format!(
            "video_size={}x{}:pix_fmt={}:time_base={}/{}:pixel_aspect={}/{}",
            decoder.width(),
            decoder.height(),
            decoder.format().descriptor().unwrap().name(),
            input.time_base().numerator(),
            input.time_base().denominator(),
            decoder.aspect_ratio().numerator(),
            decoder.aspect_ratio().denominator()
        );
        let filter_in = filter_graph.add(&ffmpeg::filter::find("buffer").unwrap(), "in", &args)?;
        let filter_out =
            filter_graph.add(&ffmpeg::filter::find("buffersink").unwrap(), "out", "")?;

        filter_graph
            .output("in", 0)?
            .input("out", 0)?
            .parse(&desc)?;

        filter_graph.validate()?;

        Ok(Self {
            _filter_graph: filter_graph,
            filter_in,
            filter_out,
        })
    }

    pub fn apply(&mut self, frame: &frame::Video) -> Result<frame::Video, ffmpeg::Error> {
        self.filter_in.source().add(frame)?;
        let mut filtered_frame = frame::Video::empty();
        filtered_frame.set_width(frame.width());
        filtered_frame.set_height(frame.height());
        filtered_frame.set_format(frame.format());
        filtered_frame.set_pts(frame.pts());
        self.filter_out.sink().frame(&mut filtered_frame)?;
        Ok(filtered_frame)
    }
}

pub struct Transcoder {
    ost_index: usize,
    decoder: decoder::Video,
    input_time_base: Rational,
    encoder: encoder::Video,
    logging_enabled: bool,
    frame_count: usize,
    total_frames: i64,
    last_log_frame_count: usize,
    last_log_time: Instant,
    frame_re: Regex,
    failed_frames: usize,
    watermark_filter: Option<VideoFilter>,
    tesseract: Option<TesseractAPI>,
}

impl Transcoder {
    pub fn new(
        ist: &format::stream::Stream,
        octx: &mut format::context::Output,
        ost_index: usize,
        enable_logging: bool,
        with_watermark: bool,
        with_recognition: bool,
    ) -> Result<Self, ffmpeg::Error> {
        debug!(
            "Transcoder with_watermark: {} with_recognition: {}",
            with_watermark, with_recognition
        );

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

        let encoder_opts = parse_opts(
            "quality=best,cpu-used=0,crf=1,qmin=1,qmax=10,kf-min-dist=1,kf-max-dist=1".to_owned(),
        )
        .unwrap();
        let opened_encoder = encoder
            .open_with(encoder_opts)
            .expect("error opening encoder with supplied settings");
        ost.set_parameters(&opened_encoder);

        let watermark_filter = if with_watermark {
            let text_height = (decoder.height() as f32 / 15.0).round() as i32;
            let font_size = (decoder.height() as f32 / 18.0).round() as i32;
            let id = "1";
            let watermark_filter = VideoFilter::new(ist, &decoder, format!("\
drawbox=x=0:y=0:w=iw:h={text_height}:color=black:t=fill,\
drawtext=fontfile=/usr/share/fonts/truetype/noto/NotoMono-Regular.ttf:text='{id}-%{{eif\\:t*1000\\:u}}'\
:fontcolor=white:fontsize={font_size}:x=(w-text_w)/2:y=({text_height}-text_h)/2", 
                    text_height = text_height, id = id, font_size = font_size))
                .expect("Failed to create watermark filter");
            Some(watermark_filter)
        } else {
            None
        };

        let tesseract = if with_recognition {
            println!("Initializing Tesseract");
            // Initialize Tesseract
            let home_dir = std::env::var("HOME").expect("Failed to get home directory");
            let tesseract_dir = format!("{}/.webrtcperf/cache", home_dir);
            std::fs::create_dir_all(&tesseract_dir)
                .expect("Failed to create Tesseract data directory");
            let tesseract_path = format!("{}/eng.traineddata", tesseract_dir);
            if !std::path::Path::new(&tesseract_path).exists() {
                // Download the file from the URL
                let response = reqwest::blocking::get(
                    "https://cdn.jsdelivr.net/npm/@tesseract.js-data/eng/4.0.0/eng.traineddata.gz",
                )
                .expect("Failed to download Tesseract data file");
                let mut decoder = GzDecoder::new(response);
                let mut file = std::fs::File::create(&tesseract_path)
                    .expect("Failed to create Tesseract data file");
                std::io::copy(&mut decoder, &mut file)
                    .expect("Failed to write Tesseract data file");
            }
            let tesseract = TesseractAPI::new();
            tesseract.init(tesseract_dir, "eng").unwrap();
            tesseract
                .set_variable("tessedit_char_whitelist", "0123456789-")
                .unwrap();
            tesseract
                .set_page_seg_mode(TessPageSegMode::PSM_SINGLE_LINE)
                .unwrap();
            Some(tesseract)
        } else {
            None
        };

        Ok(Self {
            ost_index,
            decoder,
            input_time_base: ist.time_base(),
            encoder: opened_encoder,
            logging_enabled: enable_logging,
            frame_count: 0,
            total_frames: ist.frames(),
            last_log_frame_count: 0,
            last_log_time: Instant::now(),
            frame_re: Regex::new(r"(?<id>[0-9]{1,3})-(?<time>[0-9]{1,13})").unwrap(),
            failed_frames: 0,
            watermark_filter,
            tesseract,
        })
    }

    pub fn send_packet_to_decoder(&mut self, packet: &Packet) {
        self.decoder.send_packet(packet).unwrap();
    }

    pub fn send_eof_to_decoder(&mut self) {
        self.decoder.send_eof().unwrap();
    }

    pub fn receive_and_process_decoded_frames(
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

            match self.tesseract {
                Some(ref mut tesseract) => {
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
                    let image =
                        image.crop_imm(0, 0, image.width(), (image.height() as f32 / 15f32) as u32);

                    tesseract
                        .set_image(
                            &image.to_rgb8(),
                            image.width() as i32,
                            image.height() as i32,
                            3i32,
                            3i32 * frame.width() as i32,
                        )
                        .unwrap();
                    let output = tesseract.get_utf8_text().unwrap();

                    if !self.frame_re.captures(output.trim()).map_or_else(
                        || {
                            eprintln!("failed to recognize text: \"{:?}\"", output.trim());
                            false
                        },
                        |c| {
                            let id: i32 = c["id"].parse().unwrap();
                            let time: f64 = c["time"].parse().unwrap_or(0f64) / 1000f64;
                            let pts_new = (time / f64::from(self.input_time_base)) as i64;
                            if cfg!(debug_assertions) {
                                println!(
                                    "  pts={:?} id={:?} time={:?} pts_new={:?}",
                                    frame.pts(),
                                    id,
                                    time,
                                    pts_new
                                );
                            }
                            frame.set_pts(Some(pts_new));
                            self.send_frame_to_encoder(&frame);
                            self.receive_and_process_encoded_packets(octx, ost_time_base);
                            true
                        },
                    ) {
                        self.failed_frames += 1;
                    }
                }
                None => {
                    frame = match &mut self.watermark_filter {
                        Some(filter) => filter.apply(&frame).unwrap(),
                        None => frame,
                    };

                    self.send_frame_to_encoder(&frame);
                    self.receive_and_process_encoded_packets(octx, ost_time_base);
                }
            }
        }
    }

    fn send_frame_to_encoder(&mut self, frame: &frame::Video) {
        self.encoder.send_frame(frame).unwrap();
    }

    pub fn send_eof_to_encoder(&mut self) {
        self.encoder.send_eof().unwrap();
    }

    pub fn receive_and_process_encoded_packets(
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
            "[{}/{}] {:.2}s (failed: {})",
            self.frame_count, self.total_frames, timestamp, self.failed_frames,
        );
        self.last_log_frame_count = self.frame_count;
        self.last_log_time = Instant::now();
    }

    pub fn failed_frames(&self) -> usize {
        self.failed_frames
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
