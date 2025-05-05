// Parses vmaf_cuda JSON output for VMAF score
use serde_json::Value;
pub fn parse_vmaf_output(json_str: &str) -> Option<f64> {
    let parsed: Value = serde_json::from_str(json_str).ok()?;
    parsed["VMAF_score"].as_f64()
}
