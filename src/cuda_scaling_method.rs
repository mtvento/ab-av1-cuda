// Adds interp_algo to scale_cuda based on user preference
pub fn apply_cuda_scaling_method(method: &str) -> String {
    format!("scale_cuda=format=nv12:interp_algo={}", method)
}
