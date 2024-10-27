# webrtcperf-vmaf-utils
A command line utility for processing real time videos for VMAF evaluation.

## Usage
Using the tool to convert a video file with an `<id>-<timestamp>` overlay into a VP8/IVF file, ensuring that frame timestamps match the recognized timestamps:
```bash
webrtcperf-vmaf-utils --process-file <video file>
```
