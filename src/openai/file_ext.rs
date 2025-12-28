//! File extension validation and classification for OpenAI operations.
//!
//! This module provides functions to determine:
//! - Which file extensions are allowed for upload to OpenAI
//! - Which extensions indicate binary content (unsuitable for code search)
//! - Which extensions indicate source code (suitable for CodeQuery indexing)

use std::borrow::Cow;
use std::path::Path;

/// Checks if a file extension is allowed for direct upload to OpenAI.
///
/// OpenAI's Files API only accepts specific file formats. The check is case-insensitive.
///
/// # Allowed Extensions
///
/// Source code: `c`, `cpp`, `css`, `go`, `html`, `java`, `js`, `json`, `php`, `py`, `rb`, `ts`
/// Documents: `csv`, `doc`, `docx`, `md`, `pdf`, `pptx`, `tex`, `txt`, `xlsx`, `xml`
/// Images: `gif`, `jpeg`, `jpg`, `png`, `webp`
/// Archives: `tar`, `zip`
/// Other: `pkl`
pub fn is_allowed_upload_ext(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "c" | "cpp"
            | "css"
            | "csv"
            | "doc"
            | "docx"
            | "gif"
            | "go"
            | "html"
            | "java"
            | "jpeg"
            | "jpg"
            | "js"
            | "json"
            | "md"
            | "pdf"
            | "php"
            | "pkl"
            | "png"
            | "pptx"
            | "py"
            | "rb"
            | "tar"
            | "tex"
            | "ts"
            | "txt"
            | "webp"
            | "xlsx"
            | "xml"
            | "zip"
    )
}

/// Checks if a file extension indicates binary content unsuitable for CodeQuery indexing.
///
/// # Categories Blocked
///
/// - **Images**: png, jpg, jpeg, gif, webp, bmp, ico, tif, tiff, svg, heic, avif
/// - **Audio/Video**: mp3, wav, flac, m4a, ogg, mp4, mov, mkv, webm
/// - **Archives**: zip, tar, gz, tgz, bz2, xz, 7z, rar
/// - **Executables**: exe, dll, so, dylib, a, lib, o, obj, class, jar, wasm
/// - **Binary data**: pkl, db, sqlite, sqlite3
/// - **Office/PDF**: pdf, doc, docx, ppt, pptx, xls, xlsx
pub fn is_codequery_binary_ext(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        // Images
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "tif" | "tiff" | "svg" | "heic"
            | "avif"
            // Audio/video
            | "mp3" | "wav" | "flac" | "m4a" | "ogg" | "mp4" | "mov" | "mkv" | "webm"
            // Archives/bundles
            | "zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "7z" | "rar"
            // Executables/artifacts
            | "exe" | "dll" | "so" | "dylib" | "a" | "lib" | "o" | "obj" | "class" | "jar"
            | "wasm"
            // Binary data formats
            | "pkl" | "db" | "sqlite" | "sqlite3"
            // Office/PDF docs
            | "pdf" | "doc" | "docx" | "ppt" | "pptx" | "xls" | "xlsx"
    )
}

/// Checks if a file extension indicates source code suitable for CodeQuery indexing.
///
/// # Supported Languages
///
/// Rust, C/C++, Go, Java/Kotlin, Swift, Python, Ruby, PHP, JavaScript/TypeScript
pub fn is_codequery_indexable_ext(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "rs" | "c"
            | "h"
            | "cpp"
            | "hpp"
            | "go"
            | "java"
            | "kt"
            | "kts"
            | "swift"
            | "py"
            | "rb"
            | "php"
            | "js"
            | "jsx"
            | "ts"
            | "tsx"
    )
}

/// Checks if a filename (without extension) should be indexed by CodeQuery.
///
/// Currently always returns `false` - CodeQuery only indexes files with explicit code extensions.
#[inline]
pub fn is_codequery_indexable_filename(_file_name: &str) -> bool {
    false
}

/// Determines if a file path should be indexed by CodeQuery.
///
/// Applies multiple rules:
/// 1. Dotfiles excluded (files starting with `.`)
/// 2. Markdown excluded (`.md` files)
/// 3. Binary extensions blocked
/// 4. Only explicit code extensions pass
pub fn is_codequery_indexable_path(path: &Path) -> bool {
    let file_name = match path.file_name().and_then(|n| n.to_str()) {
        Some(name) => name,
        None => return false,
    };

    // Skip dotfiles
    if file_name.starts_with('.') {
        return false;
    }

    // Check extensionless files by name
    if is_codequery_indexable_filename(file_name) {
        return true;
    }

    let ext = match path.extension().and_then(|e| e.to_str()) {
        Some(e) => e,
        None => return false,
    };

    // Skip markdown (documentation, not code)
    if ext.eq_ignore_ascii_case("md") {
        return false;
    }

    // Block known binary formats
    if is_codequery_binary_ext(ext) {
        return false;
    }

    // Only allow explicit code extensions
    is_codequery_indexable_ext(ext)
}

/// Computes the filename to use for OpenAI upload, converting unsupported extensions to `.txt`.
///
/// If the file's extension is not in the allowed list, the filename is modified to use `.txt`.
pub fn compute_upload_filename(original_filename: &str) -> Cow<'_, str> {
    let p = Path::new(original_filename);

    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
    if is_allowed_upload_ext(ext) {
        Cow::Borrowed(original_filename)
    } else {
        let stem = p
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(original_filename);

        if stem == original_filename {
            Cow::Owned(format!("{}.txt", original_filename))
        } else {
            Cow::Owned(format!("{}.txt", stem))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allowed_upload_ext() {
        assert!(is_allowed_upload_ext("pdf"));
        assert!(is_allowed_upload_ext("PDF"));
        assert!(!is_allowed_upload_ext("exe"));
    }

    #[test]
    fn test_codequery_binary_ext() {
        assert!(is_codequery_binary_ext("png"));
        assert!(is_codequery_binary_ext("zip"));
        assert!(!is_codequery_binary_ext("rs"));
    }

    #[test]
    fn test_codequery_indexable_ext() {
        assert!(is_codequery_indexable_ext("rs"));
        assert!(is_codequery_indexable_ext("py"));
        assert!(!is_codequery_indexable_ext("md"));
        assert!(!is_codequery_indexable_ext("toml"));
    }

    #[test]
    fn test_compute_upload_filename() {
        assert_eq!(compute_upload_filename("test.pdf"), "test.pdf");
        assert_eq!(compute_upload_filename("test.rs"), "test.txt");
        assert_eq!(compute_upload_filename("Makefile"), "Makefile.txt");
    }
}
