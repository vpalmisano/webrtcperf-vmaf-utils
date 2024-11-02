extern crate ffmpeg_next as ffmpeg;
mod transcoder;

use crate::transcoder::Transcoder;

use crossbeam_channel::Receiver;
use ffmpeg::Dictionary;
use ffmpeg::{format, media, Rational};
use log::debug;
use regex::Regex;
use std::collections::HashMap;
use transcoder::Mode;

pub fn watermark_video(
    input_file: &str,
    watermark_id: &str,
    receiver: Receiver<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    ffmpeg_encoder(input_file, Mode::Watermark, Some(watermark_id), receiver)
}

pub fn process_video(
    input_file: &str,
    receiver: Receiver<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    ffmpeg_encoder(input_file, Mode::Process, None, receiver)
}

fn ffmpeg_encoder(
    input_file: &str,
    mode: Mode,
    watermark_id: Option<&str>,
    receiver: Receiver<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let with_watermark = matches!(mode, Mode::Watermark);
    let replacement = if with_watermark { "$1.ivf" } else { "$1.r.ivf" };
    let output_file = Regex::new(r"(^.+)\.\w+$")
        .unwrap()
        .replace(input_file, replacement)
        .to_string();
    debug!(
        "ffmpeg_encoder: {} -> {} mode: {:?}",
        input_file, output_file, mode
    );
    /* if std::path::Path::new(&output_file).exists() {
        return Err(format!("output file {} already exists", output_file).into());
    } */

    ffmpeg::init()?;
    if cfg!(debug_assertions) {
        ffmpeg::log::set_level(ffmpeg::log::Level::Verbose);
    } else {
        ffmpeg::log::set_level(ffmpeg::log::Level::Info);
    }

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
                &mode,
                watermark_id,
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

        match receiver.try_recv() {
            Ok("stop") => {
                debug!("ffmpeg_encoder stop received");
                break;
            }
            _ => {}
        }
    }

    debug!("ffmpeg_encoder flushing");

    // Flush encoders and decoders.
    for (ost_index, transcoder) in transcoders.iter_mut() {
        let ost_time_base = ost_time_bases[*ost_index];
        transcoder.send_eof_to_decoder();
        transcoder.receive_and_process_decoded_frames(&mut octx, ost_time_base);
        transcoder.send_eof_to_encoder();
        transcoder.receive_and_process_encoded_packets(&mut octx, ost_time_base);
    }

    octx.write_trailer()?;

    if matches!(mode, Mode::Process) {
        if let Some(transcoder) = transcoders.values().next() {
            let id = transcoder.recognized_id();
            debug!(
                "ffmpeg_encoder done id: {} failed: {}",
                id.unwrap_or(&"none".to_string()),
                transcoder.failed_frames()
            );
            if let Some(id) = id {
                let new_output_file = Regex::new(r"(\..+)$")
                    .unwrap()
                    .replace(&input_file, format!(".{}.ivf", id))
                    .to_string();
                std::fs::rename(&output_file, &new_output_file)?;
                debug!("Output file renamed to: {}", new_output_file);
            }
        }
    }

    Ok(())
}
