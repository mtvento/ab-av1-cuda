// src/command/args.rs
// ==== Patch Start: to_ffmpeg_args enhancements ====
use crate::auto_hw_decoder::auto_select_decoder;
use crate::cuda_scaling_method::apply_cuda_scaling_method;
use crate::debuglog;

impl Encode {
    pub fn to_ffmpeg_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        let scaling_method = self.cuda_scaling_method
            .as_deref()
            .unwrap_or("lanczos");
        debuglog!(self.debug_ffmpeg, "Scaling method: {}", scaling_method);

        if self.auto_hw_decoder {
            if let Some(codec) = self.detected_input_codec.as_deref() {
                if let Some(decoder) = auto_select_decoder(codec) {
                    debuglog!(self.debug_ffmpeg, "Selected decoder: {}", decoder);
                    args.extend(vec![
                        "-hwaccel".into(), "cuda".into(),
                        "-hwaccel_output_format".into(), "cuda".into(),
                        "-c:v".into(), decoder.clone().into(),
                    ]);
                    if let Some(crop) = &self.detected_crop {
                        if decoder.ends_with("_cuvid") {
                            let crop_arg = format!("{}:{}:{}:{}", crop.top, crop.bottom, crop.left, crop.right);
                            args.extend(vec!["-crop".into(), crop_arg.clone().into()]);
                            debuglog!(self.debug_ffmpeg, "CUVID crop: {}", crop_arg);
                        }
                    }
                }
            }
        }

        let mut vfilter = self.vfilter.clone();
        if vfilter.contains("scale=") {
            let before = vfilter.clone();
            vfilter = vfilter.replace("scale=", &apply_cuda_scaling_method(scaling_method));
            debuglog!(self.debug_ffmpeg, "Filter rewrite: '{}' -> '{}'", before, vfilter);
        }
        if !vfilter.is_empty() {
            args.push("-vf".into());
            args.push(vfilter.into());
        }
        args
    }
}
// ==== Patch End ====
