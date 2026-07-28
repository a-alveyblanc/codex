//! Sandboxed TeX-to-PNG rendering and bounded on-disk caching for display math.

use std::ffi::OsStr;
use std::fs;
use std::io::Cursor;
use std::io::Write as _;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use image::DynamicImage;
use image::GrayImage;
use image::ImageFormat;
use image::Rgba;
use image::RgbaImage;
use image::imageops::FilterType;
use sha2::Digest;
use sha2::Sha256;
use tokio::process::Command;
use tokio::sync::Semaphore;

use crate::display_math::MathStyle;
use crate::display_math::RenderedMath;

const CACHE_VERSION: &str = "v4-grayscale-aa";
const RENDER_DPI: u16 = 1080;
const OUTPUT_RASTER_SCALE: u32 = 2;
const LOGICAL_PADDING_PX: u32 = 2;
const MAX_RAW_PNG_BYTES: u64 = 8 * 1024 * 1024;
const MAX_FINAL_PNG_BYTES: usize = 4 * 1024 * 1024;
const MAX_PIXELS: u64 = 16 * 1024 * 1024;
const MAX_CACHE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CACHE_FILES: usize = 512;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
static RENDER_PERMITS: Semaphore = Semaphore::const_new(2);

pub(super) async fn render_formula(
    formula: &str,
    style: MathStyle,
    cache_dir: &Path,
    foreground: (u8, u8, u8),
    background: (u8, u8, u8),
) -> Result<RenderedMath> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (formula, style, cache_dir, foreground, background);
        bail!("display-math rendering currently requires Linux and bubblewrap");
    }

    #[cfg(target_os = "linux")]
    {
        let _permit = RENDER_PERMITS.acquire().await?;
        fs::create_dir_all(cache_dir)
            .with_context(|| format!("create display-math cache {}", cache_dir.display()))?;
        let cache_key = cache_key(formula, style, foreground, background);
        let cache_path = cache_dir.join(format!("{}.png", hex_digest(cache_key)));
        if let Some(rendered) = read_cached(&cache_path, cache_key)? {
            return Ok(rendered);
        }

        let rendered = render_uncached(formula, style, foreground, background, cache_key).await?;
        let mut temporary_cache =
            tempfile::NamedTempFile::new_in(cache_dir).context("create temporary cache file")?;
        temporary_cache
            .write_all(rendered.png.as_ref())
            .context("write temporary display-math cache file")?;
        temporary_cache
            .persist(&cache_path)
            .map_err(|err| err.error)
            .with_context(|| format!("install {}", cache_path.display()))?;
        prune_cache(cache_dir);
        Ok(rendered)
    }
}

#[cfg(target_os = "linux")]
async fn render_uncached(
    formula: &str,
    style: MathStyle,
    foreground: (u8, u8, u8),
    background: (u8, u8, u8),
    fingerprint: [u8; 32],
) -> Result<RenderedMath> {
    let bwrap = find_tool("bwrap")?;
    let latex = find_sandboxed_tool("latex")?;
    let dvipng = find_sandboxed_tool("dvipng")?;
    let prlimit = find_sandboxed_tool("prlimit")?;
    let work = tempfile::tempdir().context("create display-math render directory")?;
    let tex_path = work.path().join("input.tex");
    fs::write(&tex_path, tex_document(formula, style))
        .with_context(|| format!("write {}", tex_path.display()))?;
    let render_dpi = RENDER_DPI.to_string();

    run_in_bwrap(
        &bwrap,
        &prlimit,
        &latex,
        [
            OsStr::new("-interaction=nonstopmode"),
            OsStr::new("-halt-on-error"),
            OsStr::new("-no-shell-escape"),
            OsStr::new("input.tex"),
        ],
        work.path(),
    )
    .await
    .context("latex failed")?;

    let dvi_path = work.path().join("input.dvi");
    let dvi_size = fs::metadata(&dvi_path)
        .with_context(|| format!("inspect {}", dvi_path.display()))?
        .len();
    if dvi_size > MAX_RAW_PNG_BYTES {
        bail!("latex output exceeded size limit");
    }
    run_in_bwrap(
        &bwrap,
        &prlimit,
        &dvipng,
        [
            OsStr::new("-T"),
            OsStr::new("tight"),
            OsStr::new("-D"),
            OsStr::new(&render_dpi),
            OsStr::new("-bg"),
            OsStr::new("Transparent"),
            OsStr::new("-fg"),
            OsStr::new("rgb 1.0 1.0 1.0"),
            OsStr::new("-o"),
            OsStr::new("output.png"),
            OsStr::new("input.dvi"),
        ],
        work.path(),
    )
    .await
    .context("dvipng failed")?;

    let raw_path = work.path().join("output.png");
    let raw_size = fs::metadata(&raw_path)
        .with_context(|| format!("inspect {}", raw_path.display()))?
        .len();
    if raw_size > MAX_RAW_PNG_BYTES {
        bail!("dvipng output exceeded size limit");
    }
    let raw = fs::read(&raw_path).with_context(|| format!("read {}", raw_path.display()))?;
    postprocess_png(&raw, foreground, background, fingerprint)
}

#[cfg(target_os = "linux")]
async fn run_in_bwrap<I, S>(
    bwrap: &Path,
    prlimit: &Path,
    tool: &Path,
    args: I,
    workdir: &Path,
) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let tool_path = tool
        .parent()
        .map(|directory| format!("{}:/usr/bin", directory.display()))
        .unwrap_or_else(|| "/usr/bin".to_string());
    let mut command = Command::new(bwrap);
    command.args([
        "--die-with-parent",
        "--new-session",
        "--unshare-all",
        "--clearenv",
        "--ro-bind",
        "/usr",
        "/usr",
    ]);
    add_library_mount(&mut command, Path::new("/lib"), "usr/lib");
    add_library_mount(&mut command, Path::new("/lib64"), "usr/lib");
    command
        .args([
            "--proc", "/proc", "--dev", "/dev", "--tmpfs", "/tmp", "--dir", "/work", "--bind",
        ])
        .arg(workdir)
        .args([
            "/work", "--chdir", "/work", "--setenv", "HOME", "/tmp", "--setenv", "PATH",
        ])
        .arg(tool_path)
        .args(["--setenv", "LANG", "C.UTF-8"])
        .arg(prlimit)
        .args([
            "--as=536870912",
            "--cpu=5",
            "--fsize=8388608",
            "--nproc=32",
            "--",
        ])
        .arg(tool)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = command.spawn().context("start bubblewrap")?;
    match tokio::time::timeout(COMMAND_TIMEOUT, child.wait()).await {
        Ok(Ok(status)) if status.success() => Ok(()),
        Ok(Ok(status)) => bail!("sandboxed process exited with {status}"),
        Ok(Err(err)) => Err(err).context("wait for sandboxed process"),
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            bail!("sandboxed process timed out");
        }
    }
}

#[cfg(target_os = "linux")]
fn add_library_mount(command: &mut Command, path: &Path, symlink_target: &str) {
    if let Ok(target) = fs::read_link(path) {
        command.arg("--symlink").arg(target).arg(path);
    } else if path.is_dir() {
        command.arg("--ro-bind").arg(path).arg(path);
    } else {
        command.arg("--symlink").arg(symlink_target).arg(path);
    }
}

fn postprocess_png(
    raw: &[u8],
    foreground: (u8, u8, u8),
    _background: (u8, u8, u8),
    fingerprint: [u8; 32],
) -> Result<RenderedMath> {
    let decoded = image::load_from_memory_with_format(raw, ImageFormat::Png)
        .context("decode dvipng output")?
        .to_rgba8();
    let raw_pixels = u64::from(decoded.width()) * u64::from(decoded.height());
    if raw_pixels == 0 || raw_pixels > MAX_PIXELS {
        bail!("dvipng dimensions exceeded limit");
    }

    let target_width = decoded.width().div_ceil(3).max(1);
    let target_height = decoded.height().div_ceil(3).max(1);
    let alpha = GrayImage::from_fn(decoded.width(), decoded.height(), |x, y| {
        image::Luma([decoded.get_pixel(x, y).0[3]])
    });
    let coverage =
        image::imageops::resize(&alpha, target_width, target_height, FilterType::Lanczos3);
    let padding = LOGICAL_PADDING_PX * OUTPUT_RASTER_SCALE;
    let mut output = RgbaImage::new(
        target_width.saturating_add(padding * 2),
        target_height.saturating_add(padding * 2),
    );
    for y in 0..target_height {
        for x in 0..target_width {
            let alpha = coverage.get_pixel(x, y).0[0];
            let pixel = if alpha == 0 {
                Rgba([0, 0, 0, 0])
            } else {
                Rgba([foreground.0, foreground.1, foreground.2, alpha])
            };
            output.put_pixel(x + padding, y + padding, pixel);
        }
    }

    let mut png = Vec::new();
    DynamicImage::ImageRgba8(output)
        .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
        .context("encode postprocessed display math")?;
    if png.len() > MAX_FINAL_PNG_BYTES {
        bail!("postprocessed PNG exceeded size limit");
    }
    let image = image::load_from_memory_with_format(&png, ImageFormat::Png)
        .context("verify postprocessed display math")?;
    Ok(RenderedMath {
        png: Arc::from(png),
        // Keep a 2x raster for Kitty/Ghostty to downsample, but report baseline
        // dimensions to terminal layout so higher density does not make the
        // equation occupy twice as many rows and columns.
        width_px: image.width().div_ceil(OUTPUT_RASTER_SCALE),
        height_px: image.height().div_ceil(OUTPUT_RASTER_SCALE),
        fingerprint,
    })
}

fn read_cached(path: &Path, fingerprint: [u8; 32]) -> Result<Option<RenderedMath>> {
    let Ok(metadata) = fs::metadata(path) else {
        return Ok(None);
    };
    if metadata.len() > MAX_FINAL_PNG_BYTES as u64 {
        let _ = fs::remove_file(path);
        return Ok(None);
    }
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let Ok(image) = image::load_from_memory_with_format(&bytes, ImageFormat::Png) else {
        let _ = fs::remove_file(path);
        return Ok(None);
    };
    if u64::from(image.width()) * u64::from(image.height()) > MAX_PIXELS {
        let _ = fs::remove_file(path);
        return Ok(None);
    }
    Ok(Some(RenderedMath {
        png: Arc::from(bytes),
        width_px: image.width().div_ceil(OUTPUT_RASTER_SCALE),
        height_px: image.height().div_ceil(OUTPUT_RASTER_SCALE),
        fingerprint,
    }))
}

fn cache_key(
    formula: &str,
    style: MathStyle,
    foreground: (u8, u8, u8),
    background: (u8, u8, u8),
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(CACHE_VERSION);
    hasher.update(RENDER_DPI.to_le_bytes());
    hasher.update(OUTPUT_RASTER_SCALE.to_le_bytes());
    hasher.update(match style {
        MathStyle::Display => b"display".as_slice(),
        MathStyle::Inline => b"inline".as_slice(),
    });
    hasher.update([
        foreground.0,
        foreground.1,
        foreground.2,
        background.0,
        background.1,
        background.2,
    ]);
    hasher.update(formula.as_bytes());
    hasher.finalize().into()
}

fn tex_document(formula: &str, style: MathStyle) -> String {
    let command = match style {
        MathStyle::Display => "\\displaystyle",
        MathStyle::Inline => "\\textstyle",
    };
    format!(
        "\\documentclass{{article}}\n\
         \\usepackage{{amsmath}}\n\
         \\usepackage{{amssymb}}\n\
         \\pagestyle{{empty}}\n\
         \\begin{{document}}\n\
         \\thispagestyle{{empty}}\n\
         \\({command} {formula}\\)\n\
         \\end{{document}}\n"
    )
}

fn find_tool(name: &str) -> Result<PathBuf> {
    let path = std::env::var_os("PATH").context("PATH is not set")?;
    std::env::split_paths(&path)
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
        .with_context(|| format!("{name} is not installed"))
}

fn find_sandboxed_tool(name: &str) -> Result<PathBuf> {
    let path = find_tool(name)?;
    let resolved = path.canonicalize()?;
    if !resolved.starts_with("/usr") || !path.starts_with("/usr") {
        bail!("{name} must be installed under /usr for sandboxed rendering");
    }
    Ok(path)
}

fn hex_digest(digest: [u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn prune_cache(cache_dir: &Path) {
    let Ok(entries) = fs::read_dir(cache_dir) else {
        return;
    };
    let mut files = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let metadata = entry.metadata().ok()?;
            metadata
                .is_file()
                .then(|| (metadata.modified().ok(), metadata.len(), entry.path()))
        })
        .collect::<Vec<_>>();
    files.sort_by_key(|(modified, _, _)| *modified);
    let mut total = files.iter().map(|(_, size, _)| size).sum::<u64>();
    while files.len() > MAX_CACHE_FILES || total > MAX_CACHE_BYTES {
        let (_, size, path) = files.remove(0);
        if fs::remove_file(path).is_ok() {
            total = total.saturating_sub(size);
        }
    }
}

#[cfg(test)]
#[path = "display_math_renderer_tests.rs"]
mod tests;
