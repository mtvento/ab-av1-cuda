// INJECTED LOGIC: to_ffmpeg_args()
use crate::auto_hw_decoder::auto_select_decoder;
use crate::cuda_scaling_method::apply_cuda_scaling_method;
use crate::log;

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
