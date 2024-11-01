# webrtcperf-vmaf-utils
A command line utility for processing real time videos for VMAF evaluation.

## Installation
```bash
cargo install --git https://github.com/vpalmisano/webrtcperf-vmaf-utils
```

## Usage

### Apply a video watermark
Using the tool to apply a timestmap watermark to a video file. It will generate
a new video file with `.w.ivf` extension.
```bash
webrtcperf-vmaf-utils --watermark VIDEO_FILE --watermark-id ID
```
### Process a video file with a watermark overlay
Using the tool to convert a video file with an `<id>-<timestamp>` overlay into a VP8/IVF file, 
ensuring that frame timestamps match the recognized timestamps.
It will generate a new video file with `.r.ivf` extension.
```bash
webrtcperf-vmaf-utils --process VIDEO_FILE
```
