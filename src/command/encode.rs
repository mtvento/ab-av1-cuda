// INJECTED LOGIC: to_ffmpeg_args()
use crate::auto_hw_decoder::auto_select_decoder;
use crate::cuda_scaling_method::apply_cuda_scaling_method;
use crate::log;

use std::process::Command;

pub fn ffmpeg_has_codec(codec: &str) -> bool {
    let output = Command::new("ffmpeg")
        .arg("-codecs")
        .output()
        .expect("Failed to execute ffmpeg");
    let codecs = String::from_utf8_lossy(&output.stdout);
    codecs.contains(codec)
}

pub fn calculate_vmaf(reference: &str, distorted: &str, vmaf_path: &str) {
    Command::new(vmaf_path)
        .arg("--reference")
        .arg(reference)
        .arg("--distorted")
        .arg(distorted)
        .output()
        .expect("Failed to execute vmaf");
}

pub fn encode_video(args: &Args) {
    if !ffmpeg_has_codec(&args.codec) {
        eprintln!("FFmpeg does not support the {} codec", &args.codec);
        return;
    }
    if args.use_cuda {
        if let Some(decoder) = auto_select_decoder(&args.codec) {
            // Use CUDA decoder
        }
        if let Some(vmaf_path) = find_vmaf_cuda() {
            // Use CUDA-enabled VMAF
        }
        apply_cuda_scaling_method(&args.input, &args.output).unwrap();
    } else {
        // Use software encoding
    }
}

impl Encode {
    pub fn to_ffmpeg_args(&self) -> Vec<String> {
        let mut args = vec![];
        let scaling_method = self.cuda_scaling_method.as_deref().unwrap_or("lanczos");

        if self.auto_hw_decoder {
            if let Some(codec) = self.detected_input_codec.as_deref() {
                if let Some(decoder) = auto_select_decoder(codec) {
                    log!(self.debug_ffmpeg, "Auto-selected decoder: {}", decoder);
                    args.extend([
                        "-hwaccel", "cuda",
                        "-hwaccel_output_format", "cuda",
                        "-c:v", &decoder,
                    ]);
                    if let Some(crop) = &self.detected_crop {
                        if decoder.ends_with("_cuvid") {
                            args.extend(["-crop", &format!(
                                "{}:{}:{}:{}",
                                crop.top, crop.bottom, crop.left, crop.right
                            )]);
                            log!(self.debug_ffmpeg, "Applied CUVID crop: {:?}", crop);
                        }
                    }
                }
            }
        }

        let mut vfilter = self.vfilter.clone();
        if vfilter.contains("scale=") {
            vfilter = vfilter.replace("scale=", &apply_cuda_scaling_method(scaling_method));
        }

        if !vfilter.is_empty() {
            args.push("-vf".into());
            args.push(vfilter);
        }

        args
    }
}
