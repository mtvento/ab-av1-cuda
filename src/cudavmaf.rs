use std::process::Command;
use std::path::Path;
use crate::vmaf_cuda_path_detection::find_vmaf_cuda;
use crate::vmaf_json_parsing::parse_vmaf_output;
use crate::log;

pub fn run_vmaf(reference: &str, distorted: &str, debug: bool) -> Option<f64> {
    let vmaf_path = find_vmaf_cuda();
    log!(debug, "Using vmaf_cuda at: {}", vmaf_path);

    let output = Command::new(&vmaf_path)
        .args(["--cuda", "--reference", reference, "--distorted", distorted, "--json"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let json = String::from_utf8_lossy(&output.stdout);
    let score = parse_vmaf_output(&json);
    log!(debug, "Parsed VMAF output: {:?}", score);
    score
}
