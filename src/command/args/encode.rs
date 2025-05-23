use anyhow::Context;
use std::process::Command;
use crate::{
    ffmpeg::FfmpegEncodeArgs,
    ffprobe::{Ffprobe, ProbeError},
    float::TerseF32,
};
use anyhow::ensure;
use clap::{Parser, ValueHint};
use std::{
    collections::HashMap,
    fmt::{self, Write},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

/// Common svt-av1/ffmpeg input encoding arguments.
#[derive(Parser, Clone)]
pub struct Encode {
    /// Encoder override. See https://ffmpeg.org/ffmpeg-all.html#toc-Video-Encoders.
    ///
    /// [possible values: libsvtav1, libx264, libx265, libvpx-vp9, ...]
    #[arg(value_enum, short, long, default_value = "libsvtav1")]
    pub encoder: Encoder,

    /// Input video file.
    #[arg(short, long, value_hint = ValueHint::FilePath)]
    pub input: PathBuf,

    /// Ffmpeg video filter applied to the input before encoding.
    /// E.g. --vfilter "scale=1280:-1,fps=24".
    ///
    /// See https://ffmpeg.org/ffmpeg-filters.html#Video-Filters
    ///
    /// For VMAF calculations this is also applied to the reference video meaning VMAF
    /// scores represent the quality of input stream *after* applying filters compared
    /// to the encoded result.
    /// This allows filters like cropping to work with VMAF, as it would be the
    /// cropped stream that is VMAF compared to a cropped-then-encoded stream. Such filters
    /// would not otherwise generally be comparable.
    ///
    /// A consequence is the VMAF score will not reflect any quality lost
    /// by the vfilter itself, only the encode.
    /// To override the VMAF vfilter set --reference-vfilter.
    #[arg(long)]
    pub vfilter: Option<String>,

    /// Pixel format. libsvtav1, libaom-av1 & librav1e default to yuv420p10le.
    #[arg(value_enum, long)]
    pub pix_format: Option<PixelFormat>,

    /// Encoder preset (0-13).
    /// Higher presets means faster encodes, but with a quality tradeoff.
    ///
    /// For some ffmpeg encoders a word may be used, e.g. "fast".
    /// libaom-av1 preset is mapped to equivalent -cpu-used argument.
    ///
    /// [svt-av1 default: 8]
    #[arg(long, allow_hyphen_values = true)]
    pub preset: Option<Arc<str>>,

    /// Interval between keyframes. Can be specified as a number of frames, or a duration.
    /// E.g. "300" or "10s". Defaults to 10s if the input duration is over 3m.
    ///
    /// Longer intervals can give better compression but make seeking more coarse.
    /// Durations will be converted to frames using the input fps.
    ///
    /// Works on svt-av1 & most ffmpeg encoders set with --encoder.
    #[arg(long)]
    pub keyint: Option<KeyInterval>,

    /// Svt-av1 scene change detection, inserts keyframes at scene changes.
    /// Defaults on if using default keyint & the input duration is over 3m. Otherwise off.
    #[arg(long)]
    pub scd: Option<bool>,

    /// Additional svt-av1 arg(s). E.g. --svt mbr=2000 --svt film-grain=8
    ///
    /// See https://gitlab.com/AOMediaCodec/SVT-AV1/-/blob/master/Docs/svt-av1_encoder_user_guide.md#options
    #[arg(long = "svt", value_parser = parse_svt_arg)]
    pub svt_args: Vec<Arc<str>>,

    /// Additional ffmpeg encoder arg(s). E.g. `--enc x265-params=lossless=1`
    /// These are added as ffmpeg output file options.
    ///
    /// The first '=' symbol will be used to infer that this is an option with a value.
    /// Passed to ffmpeg like "x265-params=lossless=1" -> ['-x265-params', 'lossless=1']
    #[arg(long = "enc", allow_hyphen_values = true, value_parser = parse_enc_arg)]
    pub enc_args: Vec<String>,

    /// Additional ffmpeg input encoder arg(s). E.g. `--enc-input r=1`
    /// These are added as ffmpeg input file options.
    ///
    /// See --enc docs.
    ///
    /// *_vaapi (e.g. h264_vaapi) encoder default:
    /// `--enc-input hwaccel=vaapi --enc-input hwaccel_output_format=vaapi`.
    ///
    /// *_vulkan encoder default: `--enc-input hwaccel=vulkan --enc-input hwaccel_output_format=vulkan`.
    #[arg(long = "enc-input", allow_hyphen_values = true, value_parser = parse_enc_arg)]
    pub enc_input_args: Vec<String>,
     /// CUDA decoder to use (e.g. h264_cuvid, hevc_cuvid)
     #[arg(long)]
     pub cuda_decoder: Option<String>,

     /// CUDA-accelerated video filters (e.g. crop_cuda=1920:1080:0:0)
     #[arg(long)]
     pub cuda_filters: Vec<String>,
     /// CUDA scaling method [bilinear/lanczos/bicubic] (default: lanczos)
     #[arg(long, default_value = "lanczos")]
     pub cuda_scaling_method: String,

     /// Number of CUDA surfaces (default: 16 for 4GB GPUs)
     #[arg(long, default_value_t = 16)]
     pub cuda_surfaces: usize,

    /// Path to VMAF executable
    #[arg(long, default_value = "vmaf")]
    pub vmaf_path: PathBuf,

    /// Use CUDA-accelerated VMAF calculation
    #[arg(long)]
    pub vmaf_cuda: bool,

    /// VMAF model path
    #[arg(long, default_value = "vmaf_v0.6.1.json")]
    pub vmaf_model: PathBuf,

    /// VMAF CUDA surfaces (default: 16)
    #[arg(long, default_value_t = 16)]
    pub vmaf_surfaces: usize,
}

fn parse_svt_arg(arg: &str) -> anyhow::Result<Arc<str>> {
    let arg = arg.trim_start_matches('-').to_owned();

    for deny in ["crf", "preset", "keyint", "scd", "input-depth"] {
        ensure!(!arg.starts_with(deny), "'{deny}' cannot be used here");
    }

    Ok(arg.into())
}

fn parse_enc_arg(arg: &str) -> anyhow::Result<String> {
    let mut arg = arg.to_owned();
    if !arg.starts_with('-') {
        arg.insert(0, '-');
    }

    ensure!(
        !arg.starts_with("-svtav1-params"),
        "'svtav1-params' cannot be set here, use `--svt`"
    );

    Ok(arg)
}

fn detect_crop(&self) -> anyhow::Result<String> {
    Command::new("ffmpeg")
        .args(["-hwaccel", "cuda", "-i", &self.input, ...])
        .output()?;
    // Parse crop from output
}

#[test]
fn test_cuda_pipeline() {
    let enc = Encode { cuda_decoder: Some("h264_cuvid".into()), ... };
    let args = enc.to_ffmpeg_args(...).unwrap();
    assert!(args.vfilter.contains("hwupload_cuda"));
}

impl Encode {
    pub fn to_encoder_args(
        &self,
        crf: f32,
        probe: &Ffprobe,
    ) -> anyhow::Result<FfmpegEncodeArgs<'_>> {
        self.to_ffmpeg_args(crf, probe)
    }

    pub fn encode_hint(&self, crf: f32) -> String {
        let Self {
            encoder,
            input,
            vfilter,
            preset,
            pix_format,
            keyint,
            scd,
            svt_args,
            enc_args,
            enc_input_args,
        } = self;

        let input = shell_escape::escape(input.display().to_string().into());

        let mut hint = "ab-av1 encode".to_owned();

        let vcodec = encoder.as_str();
        if vcodec != "libsvtav1" {
            write!(hint, " -e {vcodec}").unwrap();
        }
        write!(hint, " -i {input} --crf {}", TerseF32(crf)).unwrap();

        if let Some(preset) = preset {
            write!(hint, " --preset {preset}").unwrap();
        }
        if let Some(keyint) = keyint {
            write!(hint, " --keyint {keyint}").unwrap();
        }
        if let Some(scd) = scd {
            write!(hint, " --scd {scd}").unwrap();
        }
        if let Some(pix_fmt) = pix_format {
            write!(hint, " --pix-format {pix_fmt}").unwrap();
        }
        if let Some(filter) = vfilter {
            write!(hint, " --vfilter {filter:?}").unwrap();
        }
        for arg in svt_args {
            write!(hint, " --svt {arg}").unwrap();
        }
        for arg in enc_input_args {
            let arg = arg.trim_start_matches('-');
            write!(hint, " --enc-input {arg}").unwrap();
        }
        for arg in enc_args {
            let arg = arg.trim_start_matches('-');
            write!(hint, " --enc {arg}").unwrap();
        }

        hint
    }

    // Add this method to handle auto-crop detection
    fn detect_cuda_crop(&self) -> anyhow::Result<String> {
        let output = Command::new("ffmpeg")
            .args([
                "-hwaccel", "cuda",
                "-i", self.input.to_str().unwrap(),
                "-vf", "cropdetect=24:16:0",
                "-f", "null", "-"
            ])
            .output()
            .context("CUDA crop detection failed")?;

        let stderr = String::from_utf8_lossy(&output.stderr);
        stderr.lines()
            .rev()
            .find(|l| l.contains("crop="))
            .and_then(|l| l.split_whitespace().find(|s| s.starts_with("crop=")))
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow::anyhow!("No crop detected"))
    }


    fn to_ffmpeg_args(&self, crf: f32, probe: &Ffprobe) -> anyhow::Result<FfmpegEncodeArgs<'_>> {
        // Add this block
        if let Some(decoder) = &self.cuda_decoder {
            let available = get_cuvid_decoders()?;
            if !available.contains(decoder) {
                anyhow::bail!(
                    "CUDA decoder {} not available. Supported: {}",
                    decoder,
                    available.join(", ")
                );
            }
        }

        // Add auto-crop detection
        let mut filters = self.cuda_filters.clone();
        if filters.iter().any(|f| f == "autocrop") {
            let crop = self.detect_cuda_crop()?;
            filters.push(crop);

        let vcodec = &self.encoder.0;
        let svtav1 = vcodec.as_ref() == "libsvtav1";
        ensure!(
            svtav1 || self.svt_args.is_empty(),
            "--svt may only be used with svt-av1"
        );

        // Validate CUDA configuration
        if self.cuda_decoder.is_some() {
            let available_decoders = get_cuvid_decoders()?;
            if !available_decoders.contains(&self.cuda_decoder.as_ref().unwrap().as_str()) {
                anyhow::bail!(
                    "CUDA decoder {} not available. Supported: {}",
                    self.cuda_decoder.as_ref().unwrap(),
                    available_decoders.join(", ")
                );
            }
            ensure!(
                self.cuda_surfaces >= 8 && self.cuda_surfaces <= 32,
                "CUDA surfaces must be between 8-32 for Pascal GPUs (got {})", 
                self.cuda_surfaces
            );
        }

        let preset = match &self.preset {
            Some(n) => Some(n.clone()),
            None if svtav1 => Some("8".into()),
            None => None,
        };

        let keyint = self.keyint(probe)?;

        let mut svtav1_params = vec![];
        if svtav1 {
            let scd = match (self.scd, self.keyint, keyint) {
                (Some(true), ..) | (_, None, Some(_)) => 1,
                _ => 0,
            };
            svtav1_params.push(format!("scd={scd}"));
            // add all --svt args
            svtav1_params.extend(self.svt_args.iter().map(|a| a.to_string()));
        }

            // Build CUDA-specific arguments
            let mut cuda_input_args = vec![];
            let mut cuda_filters = String::new();
            if let Some(decoder) = &self.cuda_decoder {
                cuda_input_args.extend([
                    "-hwaccel".into(),
                    "cuda".into(),
                    "-hwaccel_output_format".into(),
                    "cuda".into(),
                    "-extra_hw_frames".into(),
                    self.cuda_surfaces.to_string().into(),
                    "-c:v".into(),
                    decoder.clone().into(),
                ]);

                // Convert standard filters to CUDA variants
                if !self.cuda_filters.is_empty() {
                    cuda_filters = self.cuda_filters.join(",")
                        .replace("crop=", "hwupload_cuda,crop=")
                        .replace("scale=", "scale_cuda=format=nv12:");
                    cuda_filters = format!("hwdownload,format=nv12,{},hwupload_cuda", cuda_filters);
                }

                // Add format conversion and memory transfer
                if !cuda_filters.is_empty() {
                    cuda_filters = format!(
                        "hwdownload,format=nv12,{},hwupload_cuda",
                        cuda_filters
                    );
                }
            }

        let mut args: Vec<Arc<String>> = self
            .enc_args
            .iter()
            .flat_map(|arg| {
                if let Some((opt, val)) = arg.split_once('=') {
                    if opt == "svtav1-params" {
                        svtav1_params.push(arg.clone());
                        vec![].into_iter()
                    } else {
                        vec![opt.to_owned().into(), val.to_owned().into()].into_iter()
                    }
                } else {
                    vec![arg.clone().into()].into_iter()
                }
            })
            .collect();

        if !svtav1_params.is_empty() {
            args.push("-svtav1-params".to_owned().into());
            args.push(svtav1_params.join(":").into());
        }

        // Set keyint/-g for all vcodecs
        if let Some(keyint) = keyint {
            if !args.iter().any(|a| &**a == "-g") {
                args.push("-g".to_owned().into());
                args.push(keyint.to_string().into());
            }
        }

        for (name, val) in self.encoder.default_ffmpeg_args() {
            if !args.iter().any(|arg| &**arg == name) {
                args.push(name.to_string().into());
                args.push(val.to_string().into());
            }
        }

        let pix_fmt = self.pix_format.or_else(|| match &**vcodec {
            "libsvtav1" | "libaom-av1" | "librav1e" => Some(PixelFormat::Yuv420p10le),
            _ if self.cuda_decoder.is_some() => Some(PixelFormat::Nv12),
            _ => None,
        });

        // Merge CUDA filters with existing filters
        let mut vfilter = self.vfilter.clone().unwrap_or_default();
        if !cuda_filters.is_empty() {
            if !vfilter.is_empty() {
                vfilter = format!("{},{}", cuda_filters, vfilter);
            } else {
                vfilter = cuda_filters;
            }
        }

        let mut input_args: Vec<Arc<String>> = self
            .enc_input_args
            .iter()
            .flat_map(|arg| {
                if let Some((opt, val)) = arg.split_once('=') {
                    vec![opt.to_owned().into(), val.to_owned().into()].into_iter()
                } else {
                    vec![arg.clone().into()].into_iter()
                }
            })
             .chain(cuda_input_args)
            .collect();

        for (name, val) in self.encoder.default_ffmpeg_input_args() {
            if !input_args.iter().any(|arg| &**arg == name) {
                input_args.push(name.to_string().into());
                input_args.push(val.to_string().into());
            }
        }

        // ban usage of the bits we already set via other args & logic
        let input_reserved = HashMap::from([
            ("-i", ""),
            ("-y", ""),
            ("-n", ""),
            ("-pix_fmt", " use --pix-format"),
            ("-crf", ""),
            ("-preset", " use --preset"),
            ("-vf", " use --vfilter"),
            ("-filter:v", " use --vfilter"),
        ]);
        for arg in &input_args {
            if let Some(hint) = input_reserved.get(arg.as_str()) {
                anyhow::bail!("Encoder argument `{arg}` not allowed{hint}");
            }
        }
        let output_reserved = {
            let mut r = input_reserved;
            r.extend([
                ("-c:a", " use --acodec"),
                ("-codec:a", " use --acodec"),
                ("-acodec", " use --acodec"),
                ("-c:v", " use --encoder"),
                ("-c:v:0", " use --encoder"),
                ("-codec:v", " use --encoder"),
                ("-codec:v:0", " use --encoder"),
                ("-vcodec", " use --encoder"),
            ]);
            r
        };
        for arg in &args {
            if let Some(hint) = output_reserved.get(arg.as_str()) {
                anyhow::bail!("Encoder argument `{arg}` not allowed{hint}");
            }
        }

        Ok(FfmpegEncodeArgs {
            input: &self.input,
            vcodec: Arc::clone(vcodec),
            pix_fmt,
            vfilter: self.vfilter.as_deref(),
            crf,
            preset,
            output_args: args,
            input_args,
            video_only: false,
        })
    }

    fn keyint(&self, probe: &Ffprobe) -> anyhow::Result<Option<i32>> {
        const KEYINT_DEFAULT_INPUT_MIN: Duration = Duration::from_secs(60 * 3);
        const KEYINT_DEFAULT: Duration = Duration::from_secs(10);

        let filter_fps = self.vfilter.as_deref().and_then(try_parse_fps_vfilter);
        Ok(
            match (self.keyint, &probe.duration, &probe.fps, filter_fps) {
                // use the filter-fps if used, otherwise the input fps
                (Some(ki), .., Some(fps)) => Some(ki.keyint_number(Ok(fps))?),
                (Some(ki), _, fps, None) => Some(ki.keyint_number(fps.clone())?),
                (None, Ok(duration), _, Some(fps)) if *duration >= KEYINT_DEFAULT_INPUT_MIN => {
                    Some(KeyInterval::Duration(KEYINT_DEFAULT).keyint_number(Ok(fps))?)
                }
                (None, Ok(duration), Ok(fps), None) if *duration >= KEYINT_DEFAULT_INPUT_MIN => {
                    Some(KeyInterval::Duration(KEYINT_DEFAULT).keyint_number(Ok(*fps))?)
                }
                _ => None,
            },
        )
    }
}

/// Video codec for encoding.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Encoder(Arc<str>);

impl Encoder {
    /// vcodec name that would work if you used it as the -e argument.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns default crf-increment.
    ///
    /// Generally 0.1 if codec supports decimal crf.
    pub fn default_crf_increment(&self) -> f32 {
        match self.as_str() {
            "libx264" | "libx265" => 0.1,
            _ => 1.0,
        }
    }

    pub fn default_min_crf(&self) -> f32 {
        match self.as_str() {
            "mpeg2video" => 2.0,
            _ => 10.0,
        }
    }

    pub fn default_max_crf(&self) -> f32 {
        match self.as_str() {
            "librav1e" | "av1_vaapi" => 255.0,
            "libx264" | "libx265" => 46.0,
            "mpeg2video" => 30.0,
            // Works well for svt-av1
            _ => 55.0,
        }
    }

    pub fn default_image_ext(&self) -> &'static str {
        match self.as_str() {
            // ffmpeg doesn't currently have good heif support,
            // these raw formats allow crf-search to work
            "libx264" => "264",
            "libx265" => "265",
            // otherwise assume av1
            _ => "avif",
        }
    }

    /// Additional encoder specific ffmpeg arg defaults.
    fn default_ffmpeg_args(&self) -> &[(&'static str, &'static str)] {
        match self.as_str() {
            // add `-b:v 0` for aom & vp9 to use "constant quality" mode
            "libaom-av1" | "libvpx-vp9" => &[("-b:v", "0")],
            // enable lookahead mode for qsv encoders
            "av1_qsv" | "hevc_qsv" | "h264_qsv" => &[
                ("-look_ahead", "1"),
                ("-extbrc", "1"),
                ("-look_ahead_depth", "40"),
            ],
            _ => &[],
        }
    }

    /// Additional encoder specific ffmpeg input arg defaults.
    fn default_ffmpeg_input_args(&self) -> &[(&'static str, &'static str)] {
        match self.as_str() {
            e if e.ends_with("_vaapi") => {
                &[("-hwaccel", "vaapi"), ("-hwaccel_output_format", "vaapi")]
            }
            e if e.ends_with("_vulkan") => {
                &[("-hwaccel", "vulkan"), ("-hwaccel_output_format", "vulkan")]
            }
            e if e.ends_with("_cuvid") => &[
            ("-hwaccel", "cuda"),
            ("-hwaccel_output_format", "cuda")
        ],
        _ => &[]
        }
    }
}

impl std::str::FromStr for Encoder {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> anyhow::Result<Self> {
        Ok(match s {
            // Support "svt-av1" alias for back compat
            "svt-av1" => Self("libsvtav1".into()),
            vcodec => Self(vcodec.into()),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyInterval {
    Frames(i32),
    Duration(Duration),
}

impl KeyInterval {
    pub fn keyint_number(&self, fps: Result<f64, ProbeError>) -> Result<i32, ProbeError> {
        Ok(match self {
            Self::Frames(keyint) => *keyint,
            Self::Duration(duration) => (duration.as_secs_f64() * fps?).round() as i32,
        })
    }
}

impl fmt::Display for KeyInterval {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Frames(frames) => write!(f, "{frames}"),
            Self::Duration(d) => write!(f, "{}", humantime::format_duration(*d)),
        }
    }
}

/// Parse as integer frames or a duration.
impl std::str::FromStr for KeyInterval {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> anyhow::Result<Self> {
        let frame_err = match s.parse::<i32>() {
            Ok(f) => return Ok(Self::Frames(f)),
            Err(err) => err,
        };
        match humantime::parse_duration(s) {
            Ok(d) => Ok(Self::Duration(d)),
            Err(e) => Err(anyhow::anyhow!("frames: {frame_err}, duration: {e}")),
        }
    }
}

/// Ordered by ascending quality.
#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[clap(rename_all = "lower")]
pub enum PixelFormat {
    Yuv420p,
    Yuv420p10le,
    Yuv422p10le,
    Yuv444p10le,
}

impl PixelFormat {
    /// Returns the max quality pixel format, or None if both are None.
    pub fn opt_max(a: Option<Self>, b: Option<Self>) -> Option<Self> {
        match (a, b) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        }
    }
}

#[test]
fn pixel_format_order() {
    use PixelFormat::*;
    assert!(Yuv420p < Yuv420p10le);
    assert!(Yuv420p10le < Yuv422p10le);
    assert!(Yuv422p10le < Yuv444p10le);
}

impl PixelFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Yuv420p10le => "yuv420p10le",
            Self::Yuv422p10le => "yuv422p10le",
            Self::Yuv444p10le => "yuv444p10le",
            Self::Yuv420p => "yuv420p",
        }
    }
}

impl fmt::Display for PixelFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl TryFrom<&str> for PixelFormat {
    type Error = ();

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "yuv420p10le" => Ok(Self::Yuv420p10le),
            "yuv422p10le" => Ok(Self::Yuv422p10le),
            "yuv444p10le" => Ok(Self::Yuv444p10le),
            "yuv420p" => Ok(Self::Yuv420p),
            _ => Err(()),
        }
    }
}

fn try_parse_fps_vfilter(vfilter: &str) -> Option<f64> {
    let fps_filter = vfilter
        .split(',')
        .find_map(|vf| vf.trim().strip_prefix("fps="))?
        .trim();

    match fps_filter {
        "ntsc" => Some(30000.0 / 1001.0),
        "pal" => Some(25.0),
        "film" => Some(24.0),
        "ntsc_film" => Some(24000.0 / 1001.0),
        _ => crate::ffprobe::parse_frame_rate(fps_filter),
    }
}

#[test]
fn test_try_parse_fps_vfilter() {
    let fps = try_parse_fps_vfilter("scale=1280:-1, fps=24, transpose=1").unwrap();
    assert!((fps - 24.0).abs() < f64::EPSILON, "{fps:?}");

    let fps = try_parse_fps_vfilter("scale=1280:-1, fps=ntsc, transpose=1").unwrap();
    assert!((fps - 30000.0 / 1001.0).abs() < f64::EPSILON, "{fps:?}");
}

#[test]
fn frame_interval_from_str() {
    use std::str::FromStr;
    let from_300 = KeyInterval::from_str("300").unwrap();
    assert_eq!(from_300, KeyInterval::Frames(300));
}

#[test]
fn duration_interval_from_str() {
    use std::{str::FromStr, time::Duration};
    let from_10s = KeyInterval::from_str("10s").unwrap();
    assert_eq!(from_10s, KeyInterval::Duration(Duration::from_secs(10)));
}

/// Should use keyint & scd defaults for >3m inputs.
#[test]
fn svtav1_to_ffmpeg_args_default_over_3m() {
    let enc = Encode {
        encoder: Encoder("libsvtav1".into()),
        input: "vid.mp4".into(),
        vfilter: Some("scale=320:-1,fps=film".into()),
        preset: None,
        pix_format: None,
        keyint: None,
        scd: None,
        svt_args: vec!["film-grain=30".into()],
        enc_args: <_>::default(),
        enc_input_args: <_>::default(),
    };

    let probe = Ffprobe {
        duration: Ok(Duration::from_secs(300)),
        has_audio: true,
        max_audio_channels: None,
        fps: Ok(30.0),
        resolution: Some((1280, 720)),
        is_image: false,
        pix_fmt: None,
    };

    let FfmpegEncodeArgs {
        input,
        vcodec,
        vfilter,
        pix_fmt,
        crf,
        preset,
        output_args,
        input_args,
        video_only,
    } = enc.to_ffmpeg_args(32.0, &probe).expect("to_ffmpeg_args");

    assert_eq!(&*vcodec, "libsvtav1");
    assert_eq!(input, enc.input);
    assert_eq!(vfilter, Some("scale=320:-1,fps=film"));
    assert_eq!(crf, 32.0);
    assert_eq!(preset, Some("8".into()));
    assert_eq!(pix_fmt, Some(PixelFormat::Yuv420p10le));
    assert!(!video_only);

    assert!(
        output_args
            .windows(2)
            .any(|w| w[0].as_str() == "-g" && w[1].as_str() == "240"),
        "expected -g in {output_args:?}"
    );
    let svtargs_idx = output_args
        .iter()
        .position(|a| a.as_str() == "-svtav1-params")
        .expect("missing -svtav1-params");
    let svtargs = output_args
        .get(svtargs_idx + 1)
        .expect("missing -svtav1-params value")
        .as_str();
    assert_eq!(svtargs, "scd=1:film-grain=30");
    assert!(input_args.is_empty());
}

#[test]
fn svtav1_to_ffmpeg_args_default_under_3m() {
    let enc = Encode {
        encoder: Encoder("libsvtav1".into()),
        input: "vid.mp4".into(),
        vfilter: None,
        preset: Some("7".into()),
        pix_format: Some(PixelFormat::Yuv420p),
        keyint: None,
        scd: None,
        svt_args: vec![],
        enc_args: <_>::default(),
        enc_input_args: <_>::default(),
    };

    let probe = Ffprobe {
        duration: Ok(Duration::from_secs(179)),
        has_audio: true,
        max_audio_channels: None,
        fps: Ok(24.0),
        resolution: Some((1280, 720)),
        is_image: false,
        pix_fmt: None,
    };

    let FfmpegEncodeArgs {
        input,
        vcodec,
        vfilter,
        pix_fmt,
        crf,
        preset,
        output_args,
        input_args,
        video_only,
    } = enc.to_ffmpeg_args(32.0, &probe).expect("to_ffmpeg_args");

    assert_eq!(&*vcodec, "libsvtav1");
    assert_eq!(input, enc.input);
    assert_eq!(vfilter, None);
    assert_eq!(crf, 32.0);
    assert_eq!(preset, Some("7".into()));
    assert_eq!(pix_fmt, Some(PixelFormat::Yuv420p));
    assert!(!video_only);

    assert!(
        !output_args.iter().any(|a| a.as_str() == "-g"),
        "unexpected -g in {output_args:?}"
    );
    let svtargs_idx = output_args
        .iter()
        .position(|a| a.as_str() == "-svtav1-params")
        .expect("missing -svtav1-params");
    let svtargs = output_args
        .get(svtargs_idx + 1)
        .expect("missing -svtav1-params value")
        .as_str();
    assert_eq!(svtargs, "scd=0");
    assert!(input_args.is_empty());
}

fn get_cuvid_decoders() -> anyhow::Result<Vec<String>> {
    let output = Command::new("ffmpeg")
        .args(["-hide_banner", "-decoders"])
        .output()
        .context("FFailed to execute ffmpeg for decoder list")?;

    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|l| l.contains("_cuvid"))
        .filter_map(|l| l.split_whitespace().nth(1)) // More robust than split(' ')
        .map(String::from)
        .collect())
}
