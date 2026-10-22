//! Read clipboard images and text on the TUI host. Text reads run on the clipboard worker.
//! Images are captured as inline PNG data URLs, never as borrowed paths or temporary files.

pub(crate) mod text;

use base64::Engine;
use std::path::Path;
use std::path::PathBuf;

// Leave headroom below the WebSocket server's 64 MiB message limit.
pub(crate) const MAX_CLIPBOARD_IMAGE_DATA_URL_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug, Clone)]
pub enum PasteImageError {
    ClipboardUnavailable(String),
    NoImage(String),
    EncodeFailed(String),
}

impl std::fmt::Display for PasteImageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PasteImageError::ClipboardUnavailable(msg) => write!(f, "clipboard unavailable: {msg}"),
            PasteImageError::NoImage(msg) => write!(f, "no image on clipboard: {msg}"),
            PasteImageError::EncodeFailed(msg) => write!(f, "could not encode image: {msg}"),
        }
    }
}
impl std::error::Error for PasteImageError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncodedImageFormat {
    Png,
    Jpeg,
    Other,
}

impl EncodedImageFormat {
    pub fn label(self) -> &'static str {
        match self {
            EncodedImageFormat::Png => "PNG",
            EncodedImageFormat::Jpeg => "JPEG",
            EncodedImageFormat::Other => "IMG",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PastedImageInfo {
    pub width: u32,
    pub height: u32,
    pub encoded_format: EncodedImageFormat, // Always PNG for now.
}

/// Capture image from system clipboard, encode to PNG, and return bytes + info.
#[cfg(not(target_os = "android"))]
pub fn paste_image_as_png() -> Result<(Vec<u8>, PastedImageInfo), PasteImageError> {
    let _span = tracing::debug_span!("paste_image_as_png").entered();
    tracing::debug!("attempting clipboard image read");
    let mut cb = arboard::Clipboard::new()
        .map_err(|e| PasteImageError::ClipboardUnavailable(e.to_string()))?;
    // Sometimes images on the clipboard come as files (e.g. when copy/pasting from
    // Finder), sometimes they come as image data (e.g. when pasting from Chrome).
    // Accept both, and prefer files if both are present.
    let files = cb
        .get()
        .file_list()
        .map_err(|e| PasteImageError::ClipboardUnavailable(e.to_string()));
    let dyn_img = if let Some(img) = files
        .unwrap_or_default()
        .into_iter()
        .find_map(|f| image::open(f).ok())
    {
        tracing::debug!(
            "clipboard image opened from file: {}x{}",
            img.width(),
            img.height()
        );
        img
    } else {
        let _span = tracing::debug_span!("get_image").entered();
        let img = cb
            .get_image()
            .map_err(|e| PasteImageError::NoImage(e.to_string()))?;
        let w = img.width as u32;
        let h = img.height as u32;
        tracing::debug!("clipboard image opened from image: {}x{}", w, h);

        let Some(rgba_img) = image::RgbaImage::from_raw(w, h, img.bytes.into_owned()) else {
            return Err(PasteImageError::EncodeFailed("invalid RGBA buffer".into()));
        };

        image::DynamicImage::ImageRgba8(rgba_img)
    };

    let mut png: Vec<u8> = Vec::new();
    {
        let span =
            tracing::debug_span!("encode_image", byte_length = tracing::field::Empty).entered();
        let mut cursor = std::io::Cursor::new(&mut png);
        dyn_img
            .write_to(&mut cursor, image::ImageFormat::Png)
            .map_err(|e| PasteImageError::EncodeFailed(e.to_string()))?;
        span.record("byte_length", png.len());
    }

    Ok((
        png,
        PastedImageInfo {
            width: dyn_img.width(),
            height: dyn_img.height(),
            encoded_format: EncodedImageFormat::Png,
        },
    ))
}

/// Android/Termux does not support arboard; return a clear error.
#[cfg(target_os = "android")]
pub fn paste_image_as_png() -> Result<(Vec<u8>, PastedImageInfo), PasteImageError> {
    Err(PasteImageError::ClipboardUnavailable(
        "clipboard image paste is unsupported on Android".into(),
    ))
}

/// Capture the clipboard once and own the image through submission and draft restoration.
pub fn paste_image_as_data_url() -> Result<(String, PastedImageInfo), PasteImageError> {
    // First attempt: read image from system clipboard via arboard (native paths or image data).
    match paste_image_as_png() {
        Ok((png, info)) => {
            if png.len().div_ceil(3) * 4 + "data:image/png;base64,".len()
                > MAX_CLIPBOARD_IMAGE_DATA_URL_BYTES
            {
                return Err(PasteImageError::EncodeFailed(
                    "clipboard image exceeds the 32 MiB transport limit".into(),
                ));
            }
            let encoded = base64::engine::general_purpose::STANDARD.encode(png);
            Ok((format!("data:image/png;base64,{encoded}"), info))
        }
        Err(e) => {
            #[cfg(target_os = "linux")]
            {
                try_wsl_clipboard_fallback(&e)
            }
            #[cfg(not(target_os = "linux"))]
            {
                Err(e)
            }
        }
    }
}

/// Attempt WSL fallback for clipboard image paste.
///
/// If clipboard is unavailable (common under WSL because arboard cannot access
/// the Windows clipboard), attempt a WSL fallback that calls PowerShell on the
/// Windows side to encode the clipboard image in memory and return its bytes.
#[cfg(target_os = "linux")]
fn try_wsl_clipboard_fallback(
    error: &PasteImageError,
) -> Result<(String, PastedImageInfo), PasteImageError> {
    use PasteImageError::ClipboardUnavailable;
    use PasteImageError::NoImage;

    if !is_probably_wsl() || !matches!(error, ClipboardUnavailable(_) | NoImage(_)) {
        return Err(error.clone());
    }

    tracing::debug!("attempting Windows PowerShell clipboard fallback");
    let Some(encoded) = try_read_windows_clipboard_image()? else {
        return Err(error.clone());
    };

    decode_windows_clipboard_image(&encoded)
}

#[cfg(target_os = "linux")]
fn decode_windows_clipboard_image(
    encoded: &str,
) -> Result<(String, PastedImageInfo), PasteImageError> {
    let encoded = encoded.trim();
    if encoded.len() + "data:image/png;base64,".len() > MAX_CLIPBOARD_IMAGE_DATA_URL_BYTES {
        return Err(PasteImageError::EncodeFailed(
            "clipboard image exceeds the 32 MiB transport limit".into(),
        ));
    }
    let png = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|e| PasteImageError::EncodeFailed(e.to_string()))?;
    let (width, height) =
        image::ImageReader::with_format(std::io::Cursor::new(png), image::ImageFormat::Png)
            .into_dimensions()
            .map_err(|e| PasteImageError::EncodeFailed(e.to_string()))?;
    Ok((
        format!("data:image/png;base64,{encoded}"),
        PastedImageInfo {
            width,
            height,
            encoded_format: EncodedImageFormat::Png,
        },
    ))
}

/// Try a Windows PowerShell command to return the clipboard PNG as base64.
/// Returns None if no command succeeded or no image was present.
#[cfg(target_os = "linux")]
fn try_read_windows_clipboard_image() -> Result<Option<String>, PasteImageError> {
    // Use stdout instead of a Windows temp file. Never log the image payload.
    let script = r#"[Console]::OutputEncoding = [System.Text.Encoding]::UTF8;
$img = Get-Clipboard -Format Image;
if ($null -eq $img) { exit 1 }
$stream = [System.IO.MemoryStream]::new();
try {
    $img.Save($stream, [System.Drawing.Imaging.ImageFormat]::Png);
    if ($stream.Length -gt 24MB) { exit 2 }
    [Console]::Write([Convert]::ToBase64String($stream.ToArray()));
} finally {
    $stream.Dispose();
    $img.Dispose();
}"#;

    for cmd in ["powershell.exe", "pwsh", "powershell"] {
        match std::process::Command::new(cmd)
            .args(["-NoProfile", "-Command", script])
            .output()
        {
            // Executing PowerShell command
            Ok(output) => {
                if output.status.success() {
                    // Decode as UTF-8 (forced by the script above).
                    let encoded = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    if !encoded.is_empty() {
                        tracing::debug!("{} read clipboard image", cmd);
                        return Ok(Some(encoded));
                    }
                } else if output.status.code() == Some(2) {
                    return Err(PasteImageError::EncodeFailed(
                        "clipboard image exceeds the 32 MiB transport limit".into(),
                    ));
                } else {
                    tracing::debug!("{} returned non-zero status", cmd);
                }
            }
            Err(err) => {
                tracing::debug!("{} not executable: {}", cmd, err);
            }
        }
    }
    Ok(None)
}

#[cfg(all(test, target_os = "linux"))]
#[path = "clipboard_paste_image_tests.rs"]
mod image_tests;

/// Normalize pasted text for a single-line search query.
pub(crate) fn normalize_pasted_search_query(pasted: &str) -> Option<String> {
    let normalized = pasted.split_whitespace().collect::<Vec<_>>().join(" ");
    (!normalized.is_empty()).then_some(normalized)
}

/// Normalize pasted text that may represent a filesystem path.
///
/// Supports:
/// - `file://` URLs (converted to local paths)
/// - Windows/UNC paths
/// - shell-escaped single paths (via `shlex`)
pub fn normalize_pasted_path(pasted: &str) -> Option<PathBuf> {
    let pasted = pasted.trim();
    let unquoted = pasted
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| pasted.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
        .unwrap_or(pasted);

    // file:// URL → filesystem path
    if let Ok(url) = url::Url::parse(unquoted)
        && url.scheme() == "file"
    {
        return url.to_file_path().ok();
    }

    // TODO: We'll improve the implementation/unit tests over time, as appropriate.
    // Possibly use typed-path: https://github.com/openai/codex/pull/2567/commits/3cc92b78e0a1f94e857cf4674d3a9db918ed352e
    //
    // Detect unquoted Windows paths and bypass POSIX shlex which
    // treats backslashes as escapes (e.g., C:\Users\Alice\file.png).
    // Also handles UNC paths (\\server\share\path).
    if let Some(path) = normalize_windows_path(unquoted) {
        return Some(path);
    }

    // shell-escaped single path → unescaped
    let parts: Vec<String> = shlex::Shlex::new(pasted).collect();
    if parts.len() == 1 {
        let part = parts.into_iter().next()?;
        if let Some(path) = normalize_windows_path(&part) {
            return Some(path);
        }
        return Some(PathBuf::from(part));
    }

    None
}

#[cfg(target_os = "linux")]
pub(crate) fn is_probably_wsl() -> bool {
    // Primary: Check /proc/version for "microsoft" or "WSL" (most reliable for standard WSL).
    if let Ok(version) = std::fs::read_to_string("/proc/version") {
        let version_lower = version.to_lowercase();
        if version_lower.contains("microsoft") || version_lower.contains("wsl") {
            return true;
        }
    }

    // Fallback: Check WSL environment variables. This handles edge cases like
    // custom Linux kernels installed in WSL where /proc/version may not contain
    // "microsoft" or "WSL".
    std::env::var_os("WSL_DISTRO_NAME").is_some() || std::env::var_os("WSL_INTEROP").is_some()
}

#[cfg(target_os = "linux")]
fn convert_windows_path_to_wsl(input: &str) -> Option<PathBuf> {
    if input.starts_with("\\\\") {
        return None;
    }

    let drive_letter = input.chars().next()?.to_ascii_lowercase();
    if !drive_letter.is_ascii_lowercase() {
        return None;
    }

    if input.get(1..2) != Some(":") {
        return None;
    }

    let mut result = PathBuf::from(format!("/mnt/{drive_letter}"));
    for component in input
        .get(2..)?
        .trim_start_matches(['\\', '/'])
        .split(['\\', '/'])
        .filter(|component| !component.is_empty())
    {
        result.push(component);
    }

    Some(result)
}

fn normalize_windows_path(input: &str) -> Option<PathBuf> {
    // Drive letter path: C:\ or C:/
    let drive = input
        .chars()
        .next()
        .map(|c| c.is_ascii_alphabetic())
        .unwrap_or(false)
        && input.get(1..2) == Some(":")
        && input
            .get(2..3)
            .map(|s| s == "\\" || s == "/")
            .unwrap_or(false);
    // UNC path: \\server\share
    let unc = input.starts_with("\\\\");
    if !drive && !unc {
        return None;
    }

    #[cfg(target_os = "linux")]
    {
        if is_probably_wsl()
            && let Some(converted) = convert_windows_path_to_wsl(input)
        {
            return Some(converted);
        }
    }

    Some(PathBuf::from(input))
}

/// Infer an image format for the provided path based on its extension.
pub fn pasted_image_format(path: &Path) -> EncodedImageFormat {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png") => EncodedImageFormat::Png,
        Some("jpg") | Some("jpeg") => EncodedImageFormat::Jpeg,
        _ => EncodedImageFormat::Other,
    }
}

#[cfg(test)]
mod pasted_search_query_tests {
    use super::*;

    #[test]
    fn collapses_whitespace() {
        assert_eq!(
            normalize_pasted_search_query("  alpha\n\tbeta\r\n gamma  "),
            Some(String::from("alpha beta gamma"))
        );
    }
}

#[cfg(test)]
mod pasted_paths_tests {
    use super::*;

    #[cfg(not(windows))]
    #[test]
    fn normalize_file_url() {
        let input = "file:///tmp/example.png";
        let result = normalize_pasted_path(input).expect("should parse file URL");
        assert_eq!(result, PathBuf::from("/tmp/example.png"));
    }

    #[test]
    fn normalize_file_url_windows() {
        let input = r"C:\Temp\example.png";
        let result = normalize_pasted_path(input).expect("should parse file URL");
        #[cfg(target_os = "linux")]
        let expected = if is_probably_wsl()
            && let Some(converted) = convert_windows_path_to_wsl(input)
        {
            converted
        } else {
            PathBuf::from(r"C:\Temp\example.png")
        };
        #[cfg(not(target_os = "linux"))]
        let expected = PathBuf::from(r"C:\Temp\example.png");
        assert_eq!(result, expected);
    }

    #[test]
    fn normalize_shell_escaped_single_path() {
        let input = "/home/user/My\\ File.png";
        let result = normalize_pasted_path(input).expect("should unescape shell-escaped path");
        assert_eq!(result, PathBuf::from("/home/user/My File.png"));
    }

    #[test]
    fn normalize_simple_quoted_path_fallback() {
        let input = "\"/home/user/My File.png\"";
        let result = normalize_pasted_path(input).expect("should trim simple quotes");
        assert_eq!(result, PathBuf::from("/home/user/My File.png"));
    }

    #[test]
    fn normalize_single_quoted_unix_path() {
        let input = "'/home/user/My File.png'";
        let result = normalize_pasted_path(input).expect("should trim single quotes via shlex");
        assert_eq!(result, PathBuf::from("/home/user/My File.png"));
    }

    #[test]
    fn normalize_multiple_tokens_returns_none() {
        // Two tokens after shell splitting → not a single path
        let input = "/home/user/a\\ b.png /home/user/c.png";
        let result = normalize_pasted_path(input);
        assert!(result.is_none());
    }

    #[test]
    fn pasted_image_format_png_jpeg_unknown() {
        assert_eq!(
            pasted_image_format(Path::new("/a/b/c.PNG")),
            EncodedImageFormat::Png
        );
        assert_eq!(
            pasted_image_format(Path::new("/a/b/c.jpg")),
            EncodedImageFormat::Jpeg
        );
        assert_eq!(
            pasted_image_format(Path::new("/a/b/c.JPEG")),
            EncodedImageFormat::Jpeg
        );
        assert_eq!(
            pasted_image_format(Path::new("/a/b/c")),
            EncodedImageFormat::Other
        );
        assert_eq!(
            pasted_image_format(Path::new("/a/b/c.webp")),
            EncodedImageFormat::Other
        );
    }

    #[test]
    fn normalize_single_quoted_windows_path() {
        let input = r"'C:\\Users\\Alice\\My File.jpeg'";
        let unquoted = r"C:\\Users\\Alice\\My File.jpeg";
        let result =
            normalize_pasted_path(input).expect("should trim single quotes on windows path");
        #[cfg(target_os = "linux")]
        let expected = if is_probably_wsl()
            && let Some(converted) = convert_windows_path_to_wsl(unquoted)
        {
            converted
        } else {
            PathBuf::from(unquoted)
        };
        #[cfg(not(target_os = "linux"))]
        let expected = PathBuf::from(unquoted);
        assert_eq!(result, expected);
    }

    #[test]
    fn normalize_double_quoted_windows_path() {
        let input = r#""C:\\Users\\Alice\\My File.jpeg""#;
        let unquoted = r"C:\\Users\\Alice\\My File.jpeg";
        let result =
            normalize_pasted_path(input).expect("should trim double quotes on windows path");
        #[cfg(target_os = "linux")]
        let expected = if is_probably_wsl()
            && let Some(converted) = convert_windows_path_to_wsl(unquoted)
        {
            converted
        } else {
            PathBuf::from(unquoted)
        };
        #[cfg(not(target_os = "linux"))]
        let expected = PathBuf::from(unquoted);
        assert_eq!(result, expected);
    }

    #[test]
    fn normalize_unquoted_windows_path_with_spaces() {
        let input = r"C:\\Users\\Alice\\My Pictures\\example image.png";
        let result = normalize_pasted_path(input).expect("should accept unquoted windows path");
        #[cfg(target_os = "linux")]
        let expected = if is_probably_wsl()
            && let Some(converted) = convert_windows_path_to_wsl(input)
        {
            converted
        } else {
            PathBuf::from(r"C:\\Users\\Alice\\My Pictures\\example image.png")
        };
        #[cfg(not(target_os = "linux"))]
        let expected = PathBuf::from(r"C:\\Users\\Alice\\My Pictures\\example image.png");
        assert_eq!(result, expected);
    }

    #[test]
    fn normalize_unc_windows_path() {
        let input = r"\\\\server\\share\\folder\\file.jpg";
        let result = normalize_pasted_path(input).expect("should accept UNC windows path");
        assert_eq!(
            result,
            PathBuf::from(r"\\\\server\\share\\folder\\file.jpg")
        );
    }

    #[test]
    fn pasted_image_format_with_windows_style_paths() {
        assert_eq!(
            pasted_image_format(Path::new(r"C:\\a\\b\\c.PNG")),
            EncodedImageFormat::Png
        );
        assert_eq!(
            pasted_image_format(Path::new(r"C:\\a\\b\\c.jpeg")),
            EncodedImageFormat::Jpeg
        );
        assert_eq!(
            pasted_image_format(Path::new(r"C:\\a\\b\\noext")),
            EncodedImageFormat::Other
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn normalize_windows_path_in_wsl() {
        // This test only runs on actual WSL systems
        if !is_probably_wsl() {
            // Skip test if not on WSL
            return;
        }
        let input = r"C:\\Users\\Alice\\Pictures\\example image.png";
        let result = normalize_pasted_path(input).expect("should convert windows path on wsl");
        assert_eq!(
            result,
            PathBuf::from("/mnt/c/Users/Alice/Pictures/example image.png")
        );
    }
}
