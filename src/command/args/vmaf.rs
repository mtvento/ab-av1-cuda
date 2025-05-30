use crate::command::args::PixelFormat;
use anyhow::Context;
use clap::Parser;
use std::{borrow::Cow, fmt::Display, sync::Arc, thread};

const DEFAULT_VMAF_FPS: f32 = 25.0;

/// Common vmaf options.
#[derive(Debug, Parser, Clone)]
pub struct Vmaf {
    /// Additional vmaf arg(s). E.g. --vmaf n_threads=8 --vmaf n_subsample=4
    ///
    /// By default `n_threads` is set to available system threads.
    ///
    /// Also see https://ffmpeg.org/ffmpeg-filters.html#libvmaf.
    #[arg(long = "vmaf", value_parser = parse_vmaf_arg)]
    pub vmaf_args: Vec<Arc<str>>,

    /// Video resolution scale to use in VMAF analysis. If set, video streams will be bicubic
    /// scaled to this during VMAF analysis. `auto` (default) automatically sets
    /// based on the model and input video resolution. `none` disables any scaling.
    /// `WxH` format may be used to specify custom scaling, e.g. `1920x1080`.
    ///
    /// auto behaviour:
    /// * 1k model (default for resolutions <= 2560x1440) if width and height
    ///   are less than 1728 & 972 respectively upscale to 1080p. Otherwise no scaling.
    /// * 4k model (default for resolutions > 2560x1440) if width and height
    ///   are less than 3456 & 1944 respectively upscale to 4k. Otherwise no scaling.
    ///
    /// The auto behaviour is based on the distorted video dimensions, equivalent
    /// to post input/reference vfilter dimensions.
    ///
    /// Scaling happens after any input/reference vfilters.
    #[arg(long, default_value_t, value_parser = parse_vmaf_scale)]
    pub vmaf_scale: VmafScale,

    /// Frame rate override used to analyse both reference & distorted videos.
    /// Maps to ffmpeg `-r` input arg.
    ///
    /// Setting to 0 disables use.
    #[arg(long, default_value_t = DEFAULT_VMAF_FPS)]
    pub vmaf_fps: f32,
}

impl Default for Vmaf {
    fn default() -> Self {
        Self {
            vmaf_args: <_>::default(),
            vmaf_scale: <_>::default(),
            vmaf_fps: DEFAULT_VMAF_FPS,
        }
    }
}

impl std::hash::Hash for Vmaf {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.vmaf_args.hash(state);
        self.vmaf_scale.hash(state);
        self.vmaf_fps.to_ne_bytes().hash(state);
    }
}

fn parse_vmaf_arg(arg: &str) -> anyhow::Result<Arc<str>> {
    Ok(arg.to_owned().into())
}

impl Vmaf {
    pub fn fps(&self) -> Option<f32> {
        Some(self.vmaf_fps).filter(|r| *r > 0.0)
    }

    /// Returns ffmpeg `filter_complex`/`lavfi` value for calculating vmaf.
    pub fn ffmpeg_lavfi(
        &self,
        distorted_res: Option<(u32, u32)>,
        pix_fmt: Option<PixelFormat>,
        ref_vfilter: Option<&str>,
    ) -> String {
        let mut args = self.vmaf_args.clone();
        if !args.iter().any(|a| a.contains("n_threads")) {
            // default n_threads to all cores
            args.push(
                format!(
                    "n_threads={}",
                    thread::available_parallelism().map_or(1, |p| p.get())
                )
                .into(),
            );
        }
        let mut lavfi = args.join(":");
        lavfi.insert_str(0, "libvmaf=shortest=true:ts_sync_mode=nearest:");

        let mut model = VmafModel::from_args(&args);
        if let (None, Some((w, h))) = (model, distorted_res) {
            if w > 2560 && h > 1440 {
                // for >2k resolutions use 4k model
                lavfi.push_str(":model=version=vmaf_4k_v0.6.1");
                model = Some(VmafModel::Vmaf4K);
            }
        }

        let ref_vf: Cow<_> = match ref_vfilter {
            None => "".into(),
            Some(vf) if vf.ends_with(',') => vf.into(),
            Some(vf) => format!("{vf},").into(),
        };
        let format = pix_fmt.map(|v| format!("format={v},")).unwrap_or_default();
        let scale = self
            .vf_scale(model.unwrap_or_default(), distorted_res)
            .map(|(w, h)| format!("scale={w}:{h}:flags=bicubic,"))
            .unwrap_or_default();

        // prefix:
        // * Add reference-vfilter if any
        // * convert both streams to common pixel format
        // * scale to vmaf width if necessary
        // * sync presentation timestamp
        let prefix = format!(
            "[0:v]{format}{scale}setpts=PTS-STARTPTS,settb=AVTB[dis];\
             [1:v]{format}{ref_vf}{scale}setpts=PTS-STARTPTS,settb=AVTB[ref];\
             [dis][ref]"
        );

        lavfi.insert_str(0, &prefix);
        lavfi
    }

    fn vf_scale(&self, model: VmafModel, distorted_res: Option<(u32, u32)>) -> Option<(i32, i32)> {
        match (self.vmaf_scale, distorted_res) {
            (VmafScale::Auto, Some((w, h))) => match model {
                // upscale small resolutions to 1k for use with the 1k model
                VmafModel::Vmaf1K if w < 1728 && h < 972 => {
                    Some(minimally_scale((w, h), (1920, 1080)))
                }
                // upscale small resolutions to 4k for use with the 4k model
                VmafModel::Vmaf4K if w < 3456 && h < 1944 => {
                    Some(minimally_scale((w, h), (3840, 2160)))
                }
                _ => None,
            },
            (VmafScale::Custom { width, height }, Some((w, h))) => {
                Some(minimally_scale((w, h), (width, height)))
            }
            (VmafScale::Custom { width, height }, None) => Some((width as _, height as _)),
            _ => None,
        }
    }
}

/// Return the smallest ffmpeg vf `(w, h)` scale values so that at least one of the
/// `target_w` or `target_h` bounds are met.
fn minimally_scale((from_w, from_h): (u32, u32), (target_w, target_h): (u32, u32)) -> (i32, i32) {
    let w_factor = from_w as f64 / target_w as f64;
    let h_factor = from_h as f64 / target_h as f64;
    if h_factor > w_factor {
        (-1, target_h as _) // scale vertically
    } else {
        (target_w as _, -1) // scale horizontally
    }
}

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VmafScale {
    None,
    #[default]
    Auto,
    Custom {
        width: u32,
        height: u32,
    },
}

fn parse_vmaf_scale(vs: &str) -> anyhow::Result<VmafScale> {
    const ERR: &str = "vmaf-scale must be 'none', 'auto' or WxH format e.g. '1920x1080'";
    match vs {
        "none" => Ok(VmafScale::None),
        "auto" => Ok(VmafScale::Auto),
        _ => {
            let (w, h) = vs.split_once('x').context(ERR)?;
            let (width, height) = (w.parse().context(ERR)?, h.parse().context(ERR)?);
            Ok(VmafScale::Custom { width, height })
        }
    }
}

impl Display for VmafScale {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::None => "none".fmt(f),
            Self::Auto => "auto".fmt(f),
            Self::Custom { width, height } => write!(f, "{width}x{height}"),
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
enum VmafModel {
    /// Default 1080p model.
    #[default]
    Vmaf1K,
    /// 4k model.
    Vmaf4K,
    /// Some other user specified model.
    Custom,
}

impl VmafModel {
    fn from_args(args: &[Arc<str>]) -> Option<Self> {
        let mut using_custom_model: Vec<_> = args.iter().filter(|v| v.contains("model")).collect();

        match using_custom_model.len() {
            0 => None,
            1 => Some(match using_custom_model.remove(0) {
                v if v.ends_with("version=vmaf_v0.6.1") => Self::Vmaf1K,
                v if v.ends_with("version=vmaf_4k_v0.6.1") => Self::Vmaf4K,
                _ => Self::Custom,
            }),
            _ => Some(Self::Custom),
        }
    }
}

#[test]
fn vmaf_lavfi() {
    let vmaf = Vmaf {
        vmaf_args: vec!["n_threads=5".into(), "n_subsample=4".into()],
        ..<_>::default()
    };
    assert_eq!(
        vmaf.ffmpeg_lavfi(
            None,
            Some(PixelFormat::Yuv420p),
            Some("scale=1280:-1,fps=24")
        ),
        "[0:v]format=yuv420p,setpts=PTS-STARTPTS,settb=AVTB[dis];\
         [1:v]format=yuv420p,scale=1280:-1,fps=24,setpts=PTS-STARTPTS,settb=AVTB[ref];\
         [dis][ref]libvmaf=shortest=true:ts_sync_mode=nearest:n_threads=5:n_subsample=4"
    );
}

#[test]
fn vmaf_lavfi_default() {
    let vmaf = Vmaf::default();
    let expected = format!(
        "[0:v]setpts=PTS-STARTPTS,settb=AVTB[dis];\
         [1:v]setpts=PTS-STARTPTS,settb=AVTB[ref];\
         [dis][ref]libvmaf=shortest=true:ts_sync_mode=nearest:n_threads={}",
        thread::available_parallelism().map_or(1, |p| p.get())
    );
    assert_eq!(vmaf.ffmpeg_lavfi(None, None, None), expected);
}

#[test]
fn vmaf_lavfi_default_pix_fmt() {
    let vmaf = Vmaf::default();
    let expected = format!(
        "[0:v]format=yuv420p10le,setpts=PTS-STARTPTS,settb=AVTB[dis];\
         [1:v]format=yuv420p10le,setpts=PTS-STARTPTS,settb=AVTB[ref];\
         [dis][ref]libvmaf=shortest=true:ts_sync_mode=nearest:n_threads={}",
        thread::available_parallelism().map_or(1, |p| p.get())
    );
    assert_eq!(
        vmaf.ffmpeg_lavfi(None, Some(PixelFormat::Yuv420p10le), None),
        expected
    );
}

#[test]
fn vmaf_lavfi_include_n_threads() {
    let vmaf = Vmaf {
        vmaf_args: vec!["log_path=output.xml".into()],
        ..<_>::default()
    };
    let expected = format!(
        "[0:v]format=yuv420p,setpts=PTS-STARTPTS,settb=AVTB[dis];\
         [1:v]format=yuv420p,setpts=PTS-STARTPTS,settb=AVTB[ref];\
         [dis][ref]libvmaf=shortest=true:ts_sync_mode=nearest:log_path=output.xml:n_threads={}",
        thread::available_parallelism().map_or(1, |p| p.get())
    );
    assert_eq!(
        vmaf.ffmpeg_lavfi(None, Some(PixelFormat::Yuv420p), None),
        expected
    );
}

/// Low resolution videos should be upscaled to 1080p
#[test]
fn vmaf_lavfi_small_width() {
    let vmaf = Vmaf {
        vmaf_args: vec!["n_threads=5".into(), "n_subsample=4".into()],
        ..<_>::default()
    };
    assert_eq!(
        vmaf.ffmpeg_lavfi(Some((1280, 720)), Some(PixelFormat::Yuv420p), None),
        "[0:v]format=yuv420p,scale=1920:-1:flags=bicubic,setpts=PTS-STARTPTS,settb=AVTB[dis];\
         [1:v]format=yuv420p,scale=1920:-1:flags=bicubic,setpts=PTS-STARTPTS,settb=AVTB[ref];\
         [dis][ref]libvmaf=shortest=true:ts_sync_mode=nearest:n_threads=5:n_subsample=4"
    );
}

/// 4k videos should use 4k model
#[test]
fn vmaf_lavfi_4k() {
    let vmaf = Vmaf {
        vmaf_args: vec!["n_threads=5".into(), "n_subsample=4".into()],
        ..<_>::default()
    };
    assert_eq!(
        vmaf.ffmpeg_lavfi(Some((3840, 2160)), Some(PixelFormat::Yuv420p), None),
        "[0:v]format=yuv420p,setpts=PTS-STARTPTS,settb=AVTB[dis];\
         [1:v]format=yuv420p,setpts=PTS-STARTPTS,settb=AVTB[ref];\
         [dis][ref]libvmaf=shortest=true:ts_sync_mode=nearest:n_threads=5:n_subsample=4:model=version=vmaf_4k_v0.6.1"
    );
}

/// >2k videos should be upscaled to 4k & use 4k model
#[test]
fn vmaf_lavfi_3k_upscale_to_4k() {
    let vmaf = Vmaf {
        vmaf_args: vec!["n_threads=5".into()],
        ..<_>::default()
    };
    assert_eq!(
        vmaf.ffmpeg_lavfi(Some((3008, 1692)), Some(PixelFormat::Yuv420p), None),
        "[0:v]format=yuv420p,scale=3840:-1:flags=bicubic,setpts=PTS-STARTPTS,settb=AVTB[dis];\
         [1:v]format=yuv420p,scale=3840:-1:flags=bicubic,setpts=PTS-STARTPTS,settb=AVTB[ref];\
         [dis][ref]libvmaf=shortest=true:ts_sync_mode=nearest:n_threads=5:model=version=vmaf_4k_v0.6.1"
    );
}

/// If user has overridden the model, don't default a vmaf width
#[test]
fn vmaf_lavfi_small_width_custom_model() {
    let vmaf = Vmaf {
        vmaf_args: vec![
            "model=version=foo".into(),
            "n_threads=5".into(),
            "n_subsample=4".into(),
        ],
        ..<_>::default()
    };
    assert_eq!(
        vmaf.ffmpeg_lavfi(Some((1280, 720)), Some(PixelFormat::Yuv420p), None),
        "[0:v]format=yuv420p,setpts=PTS-STARTPTS,settb=AVTB[dis];\
         [1:v]format=yuv420p,setpts=PTS-STARTPTS,settb=AVTB[ref];\
         [dis][ref]libvmaf=shortest=true:ts_sync_mode=nearest:model=version=foo:n_threads=5:n_subsample=4"
    );
}

#[test]
fn vmaf_lavfi_custom_model_and_width() {
    let vmaf = Vmaf {
        vmaf_args: vec![
            "model=version=foo".into(),
            "n_threads=5".into(),
            "n_subsample=4".into(),
        ],
        // if specified just do it
        vmaf_scale: VmafScale::Custom {
            width: 123,
            height: 720,
        },
        ..<_>::default()
    };
    assert_eq!(
        vmaf.ffmpeg_lavfi(Some((1280, 720)), Some(PixelFormat::Yuv420p), None),
        "[0:v]format=yuv420p,scale=123:-1:flags=bicubic,setpts=PTS-STARTPTS,settb=AVTB[dis];\
         [1:v]format=yuv420p,scale=123:-1:flags=bicubic,setpts=PTS-STARTPTS,settb=AVTB[ref];\
         [dis][ref]libvmaf=shortest=true:ts_sync_mode=nearest:model=version=foo:n_threads=5:n_subsample=4"
    );
}

#[test]
fn vmaf_lavfi_1080p() {
    let vmaf = Vmaf {
        vmaf_args: vec!["n_threads=5".into(), "n_subsample=4".into()],
        ..<_>::default()
    };
    assert_eq!(
        vmaf.ffmpeg_lavfi(Some((1920, 1080)), Some(PixelFormat::Yuv420p), None),
        "[0:v]format=yuv420p,setpts=PTS-STARTPTS,settb=AVTB[dis];\
         [1:v]format=yuv420p,setpts=PTS-STARTPTS,settb=AVTB[ref];\
         [dis][ref]libvmaf=shortest=true:ts_sync_mode=nearest:n_threads=5:n_subsample=4"
    );
}
