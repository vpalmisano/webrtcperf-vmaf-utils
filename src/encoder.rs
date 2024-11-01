extern crate ffmpeg_next as ffmpeg;
mod transcoder;

use crate::transcoder::Transcoder;

use ffmpeg::Dictionary;
use ffmpeg::{format, media, Rational};
use regex::Regex;
use std::collections::HashMap;
use log::debug;

pub fn watermark_video(input_file: &str) -> Result<(), Box<dyn std::error::Error>> {
    ffmpeg_encoder(input_file, true, false)
}

pub fn process_video(input_file: &str) -> Result<(), Box<dyn std::error::Error>> {
    ffmpeg_encoder(input_file, false, true)
}

fn ffmpeg_encoder(
    input_file: &str,
    with_watermark: bool,
    with_recognition: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    debug!(
        "ffmpeg_encoder: {} with_watermark: {} with_recognition: {}",
        input_file, with_watermark, with_recognition
    );
    let replacement = if with_watermark { ".w.ivf" } else { ".r.ivf" };
    let output_file = Regex::new(r"(\..+)$")
        .unwrap()
        .replace(input_file, replacement)
        .to_string();
    if std::path::Path::new(&output_file).exists() {
        return Err(format!("output file {} already exists", output_file).into());
    }

    ffmpeg::init()?;
    ffmpeg::log::set_level(ffmpeg::log::Level::Info);

    let mut ictx = format::input(input_file)?;
    let mut octx = format::output(&output_file)?;

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
        if ist_medium != media::Type::Video {
            stream_mapping[ist_index] = -1;
            continue;
        }
        stream_mapping[ist_index] = ost_index;
        ist_time_bases[ist_index] = ist.time_base();
        // Initialize transcoder for video stream.
        transcoders.insert(
            ist_index,
            Transcoder::new(
                &ist,
                &mut octx,
                ost_index as _,
                Some(ist_index) == best_video_stream_index,
                with_watermark,
                with_recognition,
            )?,
        );
        ost_index += 1;
    }

    octx.set_metadata(ictx.metadata().to_owned());
    let mut movflags_opts = Dictionary::new();
    movflags_opts.set("movflags", "faststart");
    octx.write_header_with(movflags_opts)?;

    for (ost_index, _) in octx.streams().enumerate() {
        ost_time_bases[ost_index] = octx.stream(ost_index as _).unwrap().time_base();
    }

    for (stream, packet) in ictx.packets() {
        let ist_index = stream.index();
        let ost_index = stream_mapping[ist_index];
        if ost_index < 0 {
            continue;
        }
        let ost_time_base = ost_time_bases[ost_index as usize];
        let transcoder = transcoders.get_mut(&ist_index).unwrap();
        transcoder.send_packet_to_decoder(&packet);
        transcoder.receive_and_process_decoded_frames(&mut octx, ost_time_base);
    }

    // Flush encoders and decoders.
    for (ost_index, transcoder) in transcoders.iter_mut() {
        let ost_time_base = ost_time_bases[*ost_index];
        transcoder.send_eof_to_decoder();
        transcoder.receive_and_process_decoded_frames(&mut octx, ost_time_base);
        transcoder.send_eof_to_encoder();
        transcoder.receive_and_process_encoded_packets(&mut octx, ost_time_base);
    }

    octx.write_trailer()?;

    if let Some(transcoder) = transcoders.values().next() {
        debug!("ffmpeg_encoder done (failed: {})", transcoder.failed_frames());
    }

    Ok(())
}
