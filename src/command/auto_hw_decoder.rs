// src/auto_hw_decoder.rs

use std::process::Command;

pub fn auto_select_decoder(codec: &str) -> Option<String> {
    // Implement logic to auto-select the appropriate CUDA decoder based on the codec
    match codec {
        "h264" => Some("h264_cuvid".to_string()),
        "hevc" => Some("hevc_cuvid".to_string()),
        _ => None,
    }
}
