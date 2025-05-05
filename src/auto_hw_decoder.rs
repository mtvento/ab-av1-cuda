// Auto-selects NVDEC decoder based on codec
pub fn auto_select_decoder(codec: &str) -> Option<&'static str> {
    match codec {
        "h264" => Some("h264_cuvid"),
        "hevc" => Some("hevc_cuvid"),
        "vp9" => Some("vp9_cuvid"),
        _ => None,
    }
}
