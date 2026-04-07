use std::cmp::Reverse;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use glob::Pattern;
use regex::RegexBuilder;
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

/// Maximum file size that can be read (10 MB).
const MAX_READ_SIZE: u64 = 10 * 1024 * 1024;

/// Maximum file size that can be written (10 MB).
const MAX_WRITE_SIZE: usize = 10 * 1024 * 1024;

/// Maximum number of PDF pages per request.
const MAX_PDF_PAGES: usize = 20;

/// Maximum PDF file size (20 MB).
const MAX_PDF_SIZE: u64 = 20 * 1024 * 1024;

/// Maximum image file size (20 MB).
const MAX_IMAGE_SIZE: u64 = 20 * 1024 * 1024;

/// Recognized image file extensions.
const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp"];

/// Return the MIME media type for a recognized image extension.
fn media_type_for_extension(ext: &str) -> Option<&'static str> {
    match ext.to_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

/// Check whether a path has a recognized image file extension.
fn is_image_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| IMAGE_EXTENSIONS.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

/// Read an image file, base64-encode it, and return a structured JSON payload
/// that downstream message conversion can turn into a multimodal content block.
fn read_image(path: &Path) -> io::Result<ReadFileOutput> {
    let metadata = fs::metadata(path)?;
    if metadata.len() > MAX_IMAGE_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "image too large ({} bytes, max {} bytes)",
                metadata.len(),
                MAX_IMAGE_SIZE
            ),
        ));
    }

    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let media_type = media_type_for_extension(ext).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "unsupported image format")
    })?;

    let bytes = fs::read(path)?;
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);

    let content = serde_json::json!({
        "type": "image",
        "source": {
            "type": "base64",
            "media_type": media_type,
            "data": encoded
        }
    })
    .to_string();

    Ok(ReadFileOutput {
        kind: String::from("image"),
        file: TextFilePayload {
            file_path: path.to_string_lossy().into_owned(),
            content,
            num_lines: 1,
            start_line: 1,
            total_lines: 1,
        },
    })
}

/// Check whether a file appears to contain binary content by examining
/// the first chunk for NUL bytes.
fn is_binary_file(path: &Path) -> io::Result<bool> {
    use std::io::Read;
    let mut file = fs::File::open(path)?;
    let mut buffer = [0u8; 8192];
    let bytes_read = file.read(&mut buffer)?;
    Ok(buffer[..bytes_read].contains(&0))
}

/// Validate that a resolved path stays within the given workspace root.
/// Returns the canonical path on success, or an error if the path escapes
/// the workspace boundary (e.g. via `../` traversal or symlink).
#[allow(dead_code)]
fn validate_workspace_boundary(resolved: &Path, workspace_root: &Path) -> io::Result<()> {
    if !resolved.starts_with(workspace_root) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "path {} escapes workspace boundary {}",
                resolved.display(),
                workspace_root.display()
            ),
        ));
    }
    Ok(())
}

/// Text payload returned by file-reading operations.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TextFilePayload {
    #[serde(rename = "filePath")]
    pub file_path: String,
    pub content: String,
    #[serde(rename = "numLines")]
    pub num_lines: usize,
    #[serde(rename = "startLine")]
    pub start_line: usize,
    #[serde(rename = "totalLines")]
    pub total_lines: usize,
}

/// Output envelope for the `read_file` tool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReadFileOutput {
    #[serde(rename = "type")]
    pub kind: String,
    pub file: TextFilePayload,
}

/// Structured patch hunk emitted by write and edit operations.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StructuredPatchHunk {
    #[serde(rename = "oldStart")]
    pub old_start: usize,
    #[serde(rename = "oldLines")]
    pub old_lines: usize,
    #[serde(rename = "newStart")]
    pub new_start: usize,
    #[serde(rename = "newLines")]
    pub new_lines: usize,
    pub lines: Vec<String>,
}

/// Output envelope for full-file write operations.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WriteFileOutput {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(rename = "filePath")]
    pub file_path: String,
    pub content: String,
    #[serde(rename = "structuredPatch")]
    pub structured_patch: Vec<StructuredPatchHunk>,
    #[serde(rename = "originalFile")]
    pub original_file: Option<String>,
    #[serde(rename = "gitDiff")]
    pub git_diff: Option<serde_json::Value>,
}

/// Output envelope for targeted string-replacement edits.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EditFileOutput {
    #[serde(rename = "filePath")]
    pub file_path: String,
    #[serde(rename = "oldString")]
    pub old_string: String,
    #[serde(rename = "newString")]
    pub new_string: String,
    #[serde(rename = "originalFile")]
    pub original_file: String,
    #[serde(rename = "structuredPatch")]
    pub structured_patch: Vec<StructuredPatchHunk>,
    #[serde(rename = "userModified")]
    pub user_modified: bool,
    #[serde(rename = "replaceAll")]
    pub replace_all: bool,
    #[serde(rename = "gitDiff")]
    pub git_diff: Option<serde_json::Value>,
}

/// Result of a glob-based filename search.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GlobSearchOutput {
    #[serde(rename = "durationMs")]
    pub duration_ms: u128,
    #[serde(rename = "numFiles")]
    pub num_files: usize,
    pub filenames: Vec<String>,
    pub truncated: bool,
}

/// Parameters accepted by the grep-style search tool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GrepSearchInput {
    pub pattern: String,
    pub path: Option<String>,
    pub glob: Option<String>,
    #[serde(rename = "output_mode")]
    pub output_mode: Option<String>,
    #[serde(rename = "-B")]
    pub before: Option<usize>,
    #[serde(rename = "-A")]
    pub after: Option<usize>,
    #[serde(rename = "-C")]
    pub context_short: Option<usize>,
    pub context: Option<usize>,
    #[serde(rename = "-n")]
    pub line_numbers: Option<bool>,
    #[serde(rename = "-i")]
    pub case_insensitive: Option<bool>,
    #[serde(rename = "type")]
    pub file_type: Option<String>,
    pub head_limit: Option<usize>,
    pub offset: Option<usize>,
    pub multiline: Option<bool>,
}

/// Result payload returned by the grep-style search tool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GrepSearchOutput {
    pub mode: Option<String>,
    #[serde(rename = "numFiles")]
    pub num_files: usize,
    pub filenames: Vec<String>,
    pub content: Option<String>,
    #[serde(rename = "numLines")]
    pub num_lines: Option<usize>,
    #[serde(rename = "numMatches")]
    pub num_matches: Option<usize>,
    #[serde(rename = "appliedLimit")]
    pub applied_limit: Option<usize>,
    #[serde(rename = "appliedOffset")]
    pub applied_offset: Option<usize>,
}

/// Render a Jupyter `.ipynb` notebook file into human-readable cell-by-cell output.
fn render_notebook(path: &Path) -> io::Result<String> {
    let raw = fs::read_to_string(path)?;
    let notebook: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid notebook JSON: {e}"),
        )
    })?;

    let cells = notebook
        .get("cells")
        .and_then(|v| v.as_array())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "notebook JSON missing 'cells' array",
            )
        })?;

    let mut output = String::new();
    for (i, cell) in cells.iter().enumerate() {
        let cell_type = cell
            .get("cell_type")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        output.push_str(&format!("--- Cell {} [{}] ---\n", i + 1, cell_type));

        // Render source (string or array of strings)
        if let Some(source) = cell.get("source") {
            output.push_str(&json_text_value(source));
        }

        // For code cells, render outputs
        if cell_type == "code" {
            if let Some(outputs) = cell.get("outputs").and_then(|v| v.as_array()) {
                for out in outputs {
                    let output_type = out
                        .get("output_type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    match output_type {
                        "stream" => {
                            if let Some(text) = out.get("text") {
                                output.push_str(&json_text_value(text));
                            }
                        }
                        "execute_result" | "display_data" => {
                            if let Some(text) = out
                                .get("data")
                                .and_then(|d| d.get("text/plain"))
                            {
                                output.push_str(&json_text_value(text));
                            }
                        }
                        "error" => {
                            let ename = out
                                .get("ename")
                                .and_then(|v| v.as_str())
                                .unwrap_or("Error");
                            let evalue = out
                                .get("evalue")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            output.push_str(&format!("{ename}: {evalue}\n"));
                        }
                        _ => {}
                    }
                }
            }
        }

        output.push('\n');
    }

    Ok(output)
}

/// Extract text from a JSON value that may be a string or an array of strings
/// (common in `.ipynb` cell source and output fields).
fn json_text_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => {
            let mut result = s.clone();
            if !result.ends_with('\n') {
                result.push('\n');
            }
            result
        }
        serde_json::Value::Array(arr) => {
            let mut result = String::new();
            for item in arr {
                if let Some(s) = item.as_str() {
                    result.push_str(s);
                }
            }
            if !result.is_empty() && !result.ends_with('\n') {
                result.push('\n');
            }
            result
        }
        _ => String::new(),
    }
}

/// Parse a page range specification into 0-indexed page numbers.
///
/// Supported formats:
/// - `"3"` — single page (returns `[2]`)
/// - `"1-5"` — inclusive range (returns `[0,1,2,3,4]`)
/// - `"1,3,5"` — comma-separated list
/// - `"1-3,7,9-10"` — mixed ranges and single pages
///
/// All returned indices are 0-based. The function validates that the total
/// number of requested pages does not exceed `MAX_PDF_PAGES` and that all
/// page numbers fall within `1..=total_pages`.
fn parse_page_range(spec: &str, total_pages: usize) -> io::Result<Vec<usize>> {
    let mut pages = Vec::new();
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((start_str, end_str)) = part.split_once('-') {
            let start: usize = start_str.trim().parse().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("invalid page number in range: '{start_str}'"),
                )
            })?;
            let end: usize = end_str.trim().parse().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("invalid page number in range: '{end_str}'"),
                )
            })?;
            if start == 0 || end == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "page numbers are 1-based; 0 is not valid",
                ));
            }
            if start > end {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("invalid range: start ({start}) > end ({end})"),
                ));
            }
            if end > total_pages {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "page {end} out of bounds (document has {total_pages} pages)"
                    ),
                ));
            }
            for p in start..=end {
                pages.push(p - 1); // convert to 0-indexed
            }
        } else {
            let page: usize = part.parse().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("invalid page number: '{part}'"),
                )
            })?;
            if page == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "page numbers are 1-based; 0 is not valid",
                ));
            }
            if page > total_pages {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "page {page} out of bounds (document has {total_pages} pages)"
                    ),
                ));
            }
            pages.push(page - 1); // convert to 0-indexed
        }
    }

    // Deduplicate while preserving order
    let mut seen = std::collections::HashSet::new();
    pages.retain(|p| seen.insert(*p));

    if pages.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "page range resolved to zero pages",
        ));
    }
    if pages.len() > MAX_PDF_PAGES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "requested {} pages, maximum is {MAX_PDF_PAGES}",
                pages.len()
            ),
        ));
    }

    Ok(pages)
}

/// Read a PDF file and extract text content, optionally filtering to specific pages.
fn read_pdf(path: &Path, pages: Option<&str>) -> io::Result<ReadFileOutput> {
    let metadata = fs::metadata(path)?;
    if metadata.len() > MAX_PDF_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "PDF too large ({} bytes, max {} bytes)",
                metadata.len(),
                MAX_PDF_SIZE
            ),
        ));
    }

    let bytes = fs::read(path)?;
    let full_text = pdf_extract::extract_text_from_mem(&bytes).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("failed to extract PDF text: {e}"),
        )
    })?;

    // pdf_extract returns the full document text. We split on form-feed
    // characters (\x0c) which typically delimit pages in the extracted output.
    let raw_pages: Vec<&str> = full_text.split('\x0c').collect();
    // Remove trailing empty page that often results from a trailing form-feed
    let all_pages: Vec<&str> = if raw_pages.last().map_or(false, |p| p.trim().is_empty()) {
        raw_pages[..raw_pages.len() - 1].to_vec()
    } else {
        raw_pages
    };
    let total_pages = all_pages.len().max(1); // at least 1 page even if no FF found

    let selected_indices = if let Some(spec) = pages {
        parse_page_range(spec, total_pages)?
    } else {
        if total_pages > MAX_PDF_PAGES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "PDF has {total_pages} pages. Provide a `pages` parameter to select \
                     up to {MAX_PDF_PAGES} pages (e.g., \"1-5\")."
                ),
            ));
        }
        (0..total_pages).collect()
    };

    let mut content = String::new();
    for &idx in &selected_indices {
        let page_text = all_pages.get(idx).unwrap_or(&"");
        content.push_str(&format!("--- Page {} ---\n", idx + 1));
        content.push_str(page_text.trim());
        content.push_str("\n\n");
    }

    let num_lines = content.lines().count();

    Ok(ReadFileOutput {
        kind: String::from("pdf"),
        file: TextFilePayload {
            file_path: path.to_string_lossy().into_owned(),
            content,
            num_lines,
            start_line: 1,
            total_lines: num_lines,
        },
    })
}

/// Reads a text file and returns a line-windowed payload.
pub fn read_file(
    path: &str,
    offset: Option<usize>,
    limit: Option<usize>,
    pages: Option<&str>,
) -> io::Result<ReadFileOutput> {
    let absolute_path = normalize_path(path)?;

    // Detect PDF files before the generic size/binary checks (PDFs are binary
    // but we handle them specially via text extraction).
    if absolute_path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("pdf"))
        == Some(true)
    {
        return read_pdf(&absolute_path, pages);
    }

    // Detect image files before the generic size/binary checks (images are binary
    // but we handle them specially via base64 encoding).
    if is_image_extension(&absolute_path) {
        return read_image(&absolute_path);
    }

    // Check file size before reading
    let metadata = fs::metadata(&absolute_path)?;
    if metadata.len() > MAX_READ_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "file is too large ({} bytes, max {} bytes)",
                metadata.len(),
                MAX_READ_SIZE
            ),
        ));
    }

    // Detect Jupyter notebooks before the binary check (ipynb files are JSON
    // but may contain embedded binary-looking data in output cells).
    if absolute_path.extension().and_then(|e| e.to_str()) == Some("ipynb") {
        let rendered = render_notebook(&absolute_path)?;
        let lines: Vec<&str> = rendered.lines().collect();
        let total = lines.len();
        let start = offset.unwrap_or(0).min(total);
        let end = limit.map_or(total, |l| (start + l).min(total));
        let content = lines[start..end].join("\n");
        return Ok(ReadFileOutput {
            kind: String::from("notebook"),
            file: TextFilePayload {
                file_path: absolute_path.to_string_lossy().into_owned(),
                content,
                num_lines: end - start,
                start_line: start + 1,
                total_lines: total,
            },
        });
    }

    // Detect binary files
    if is_binary_file(&absolute_path)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "file appears to be binary",
        ));
    }

    let content = fs::read_to_string(&absolute_path)?;
    let lines: Vec<&str> = content.lines().collect();
    let start_index = offset.unwrap_or(0).min(lines.len());
    let end_index = limit.map_or(lines.len(), |limit| {
        start_index.saturating_add(limit).min(lines.len())
    });
    let selected = lines[start_index..end_index].join("\n");

    Ok(ReadFileOutput {
        kind: String::from("text"),
        file: TextFilePayload {
            file_path: absolute_path.to_string_lossy().into_owned(),
            content: selected,
            num_lines: end_index.saturating_sub(start_index),
            start_line: start_index.saturating_add(1),
            total_lines: lines.len(),
        },
    })
}

/// Replaces a file's contents and returns patch metadata.
pub fn write_file(path: &str, content: &str) -> io::Result<WriteFileOutput> {
    if content.len() > MAX_WRITE_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "content is too large ({} bytes, max {} bytes)",
                content.len(),
                MAX_WRITE_SIZE
            ),
        ));
    }

    let absolute_path = normalize_path_allow_missing(path)?;
    let original_file = fs::read_to_string(&absolute_path).ok();
    if let Some(parent) = absolute_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&absolute_path, content)?;

    Ok(WriteFileOutput {
        kind: if original_file.is_some() {
            String::from("update")
        } else {
            String::from("create")
        },
        file_path: absolute_path.to_string_lossy().into_owned(),
        content: content.to_owned(),
        structured_patch: make_patch(original_file.as_deref().unwrap_or(""), content),
        original_file,
        git_diff: None,
    })
}

/// Performs an in-file string replacement and returns patch metadata.
pub fn edit_file(
    path: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
) -> io::Result<EditFileOutput> {
    let absolute_path = normalize_path(path)?;
    let original_file = fs::read_to_string(&absolute_path)?;
    if old_string == new_string {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "old_string and new_string must differ",
        ));
    }
    if !original_file.contains(old_string) {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "old_string not found in file",
        ));
    }

    let updated = if replace_all {
        original_file.replace(old_string, new_string)
    } else {
        original_file.replacen(old_string, new_string, 1)
    };
    fs::write(&absolute_path, &updated)?;

    Ok(EditFileOutput {
        file_path: absolute_path.to_string_lossy().into_owned(),
        old_string: old_string.to_owned(),
        new_string: new_string.to_owned(),
        original_file: original_file.clone(),
        structured_patch: make_patch(&original_file, &updated),
        user_modified: false,
        replace_all,
        git_diff: None,
    })
}

/// Expands a glob pattern and returns matching filenames.
pub fn glob_search(pattern: &str, path: Option<&str>) -> io::Result<GlobSearchOutput> {
    let started = Instant::now();
    let base_dir = path
        .map(normalize_path)
        .transpose()?
        .unwrap_or(std::env::current_dir()?);
    let search_pattern = if Path::new(pattern).is_absolute() {
        pattern.to_owned()
    } else {
        base_dir.join(pattern).to_string_lossy().into_owned()
    };

    let mut matches = Vec::new();
    let entries = glob::glob(&search_pattern)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))?;
    for entry in entries.flatten() {
        if entry.is_file() {
            matches.push(entry);
        }
    }

    matches.sort_by_key(|path| {
        fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .ok()
            .map(Reverse)
    });

    let truncated = matches.len() > 100;
    let filenames = matches
        .into_iter()
        .take(100)
        .map(|path| path.to_string_lossy().into_owned())
        .collect::<Vec<_>>();

    Ok(GlobSearchOutput {
        duration_ms: started.elapsed().as_millis(),
        num_files: filenames.len(),
        filenames,
        truncated,
    })
}

/// Runs a regex search over workspace files with optional context lines.
pub fn grep_search(input: &GrepSearchInput) -> io::Result<GrepSearchOutput> {
    let base_path = input
        .path
        .as_deref()
        .map(normalize_path)
        .transpose()?
        .unwrap_or(std::env::current_dir()?);

    let regex = RegexBuilder::new(&input.pattern)
        .case_insensitive(input.case_insensitive.unwrap_or(false))
        .dot_matches_new_line(input.multiline.unwrap_or(false))
        .build()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))?;

    let glob_filter = input
        .glob
        .as_deref()
        .map(Pattern::new)
        .transpose()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))?;
    let file_type = input.file_type.as_deref();
    let output_mode = input
        .output_mode
        .clone()
        .unwrap_or_else(|| String::from("files_with_matches"));
    let context = input.context.or(input.context_short).unwrap_or(0);

    let mut filenames = Vec::new();
    let mut content_lines = Vec::new();
    let mut total_matches = 0usize;

    for file_path in collect_search_files(&base_path)? {
        if !matches_optional_filters(&file_path, glob_filter.as_ref(), file_type) {
            continue;
        }

        let Ok(file_contents) = fs::read_to_string(&file_path) else {
            continue;
        };

        if output_mode == "count" {
            let count = regex.find_iter(&file_contents).count();
            if count > 0 {
                filenames.push(file_path.to_string_lossy().into_owned());
                total_matches += count;
            }
            continue;
        }

        let lines: Vec<&str> = file_contents.lines().collect();
        let mut matched_lines = Vec::new();
        for (index, line) in lines.iter().enumerate() {
            if regex.is_match(line) {
                total_matches += 1;
                matched_lines.push(index);
            }
        }

        if matched_lines.is_empty() {
            continue;
        }

        filenames.push(file_path.to_string_lossy().into_owned());
        if output_mode == "content" {
            for index in matched_lines {
                let start = index.saturating_sub(input.before.unwrap_or(context));
                let end = (index + input.after.unwrap_or(context) + 1).min(lines.len());
                for (current, line) in lines.iter().enumerate().take(end).skip(start) {
                    let prefix = if input.line_numbers.unwrap_or(true) {
                        format!("{}:{}:", file_path.to_string_lossy(), current + 1)
                    } else {
                        format!("{}:", file_path.to_string_lossy())
                    };
                    content_lines.push(format!("{prefix}{line}"));
                }
            }
        }
    }

    let (filenames, applied_limit, applied_offset) =
        apply_limit(filenames, input.head_limit, input.offset);
    let content_output = if output_mode == "content" {
        let (lines, limit, offset) = apply_limit(content_lines, input.head_limit, input.offset);
        return Ok(GrepSearchOutput {
            mode: Some(output_mode),
            num_files: filenames.len(),
            filenames,
            num_lines: Some(lines.len()),
            content: Some(lines.join("\n")),
            num_matches: None,
            applied_limit: limit,
            applied_offset: offset,
        });
    } else {
        None
    };

    Ok(GrepSearchOutput {
        mode: Some(output_mode.clone()),
        num_files: filenames.len(),
        filenames,
        content: content_output,
        num_lines: None,
        num_matches: (output_mode == "count").then_some(total_matches),
        applied_limit,
        applied_offset,
    })
}

fn collect_search_files(base_path: &Path) -> io::Result<Vec<PathBuf>> {
    if base_path.is_file() {
        return Ok(vec![base_path.to_path_buf()]);
    }

    let mut files = Vec::new();
    for entry in WalkDir::new(base_path) {
        let entry = entry.map_err(|error| io::Error::other(error.to_string()))?;
        if entry.file_type().is_file() {
            files.push(entry.path().to_path_buf());
        }
    }
    Ok(files)
}

fn matches_optional_filters(
    path: &Path,
    glob_filter: Option<&Pattern>,
    file_type: Option<&str>,
) -> bool {
    if let Some(glob_filter) = glob_filter {
        let path_string = path.to_string_lossy();
        if !glob_filter.matches(&path_string) && !glob_filter.matches_path(path) {
            return false;
        }
    }

    if let Some(file_type) = file_type {
        let extension = path.extension().and_then(|extension| extension.to_str());
        if extension != Some(file_type) {
            return false;
        }
    }

    true
}

fn apply_limit<T>(
    items: Vec<T>,
    limit: Option<usize>,
    offset: Option<usize>,
) -> (Vec<T>, Option<usize>, Option<usize>) {
    let offset_value = offset.unwrap_or(0);
    let mut items = items.into_iter().skip(offset_value).collect::<Vec<_>>();
    let explicit_limit = limit.unwrap_or(250);
    if explicit_limit == 0 {
        return (items, None, (offset_value > 0).then_some(offset_value));
    }

    let truncated = items.len() > explicit_limit;
    items.truncate(explicit_limit);
    (
        items,
        truncated.then_some(explicit_limit),
        (offset_value > 0).then_some(offset_value),
    )
}

fn make_patch(original: &str, updated: &str) -> Vec<StructuredPatchHunk> {
    let mut lines = Vec::new();
    for line in original.lines() {
        lines.push(format!("-{line}"));
    }
    for line in updated.lines() {
        lines.push(format!("+{line}"));
    }

    vec![StructuredPatchHunk {
        old_start: 1,
        old_lines: original.lines().count(),
        new_start: 1,
        new_lines: updated.lines().count(),
        lines,
    }]
}

fn normalize_path(path: &str) -> io::Result<PathBuf> {
    let candidate = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        std::env::current_dir()?.join(path)
    };
    candidate.canonicalize()
}

fn normalize_path_allow_missing(path: &str) -> io::Result<PathBuf> {
    let candidate = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        std::env::current_dir()?.join(path)
    };

    if let Ok(canonical) = candidate.canonicalize() {
        return Ok(canonical);
    }

    if let Some(parent) = candidate.parent() {
        let canonical_parent = parent
            .canonicalize()
            .unwrap_or_else(|_| parent.to_path_buf());
        if let Some(name) = candidate.file_name() {
            return Ok(canonical_parent.join(name));
        }
    }

    Ok(candidate)
}

/// Read a file with workspace boundary enforcement.
#[allow(dead_code)]
pub fn read_file_in_workspace(
    path: &str,
    offset: Option<usize>,
    limit: Option<usize>,
    workspace_root: &Path,
) -> io::Result<ReadFileOutput> {
    let absolute_path = normalize_path(path)?;
    let canonical_root = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());
    validate_workspace_boundary(&absolute_path, &canonical_root)?;
    read_file(path, offset, limit, None)
}

/// Write a file with workspace boundary enforcement.
#[allow(dead_code)]
pub fn write_file_in_workspace(
    path: &str,
    content: &str,
    workspace_root: &Path,
) -> io::Result<WriteFileOutput> {
    let absolute_path = normalize_path_allow_missing(path)?;
    let canonical_root = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());
    validate_workspace_boundary(&absolute_path, &canonical_root)?;
    write_file(path, content)
}

/// Edit a file with workspace boundary enforcement.
#[allow(dead_code)]
pub fn edit_file_in_workspace(
    path: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
    workspace_root: &Path,
) -> io::Result<EditFileOutput> {
    let absolute_path = normalize_path(path)?;
    let canonical_root = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());
    validate_workspace_boundary(&absolute_path, &canonical_root)?;
    edit_file(path, old_string, new_string, replace_all)
}

/// Check whether a path is a symlink that resolves outside the workspace.
#[allow(dead_code)]
pub fn is_symlink_escape(path: &Path, workspace_root: &Path) -> io::Result<bool> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_symlink() {
        return Ok(false);
    }
    let resolved = path.canonicalize()?;
    let canonical_root = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());
    Ok(!resolved.starts_with(&canonical_root))
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        edit_file, glob_search, grep_search, is_image_extension, is_symlink_escape,
        media_type_for_extension, parse_page_range, read_file, read_file_in_workspace,
        read_image, render_notebook, write_file, GrepSearchInput, MAX_IMAGE_SIZE, MAX_PDF_PAGES,
        MAX_PDF_SIZE, MAX_WRITE_SIZE,
    };

    fn temp_path(name: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should move forward")
            .as_nanos();
        std::env::temp_dir().join(format!("clawd-native-{name}-{unique}"))
    }

    /// Like `temp_path` but preserves the file extension (e.g. `.ipynb`) so
    /// that extension-based dispatch in `read_file` works correctly.
    fn temp_path_ext(stem: &str, ext: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should move forward")
            .as_nanos();
        std::env::temp_dir().join(format!("clawd-native-{stem}-{unique}.{ext}"))
    }

    #[test]
    fn reads_and_writes_files() {
        let path = temp_path("read-write.txt");
        let write_output = write_file(path.to_string_lossy().as_ref(), "one\ntwo\nthree")
            .expect("write should succeed");
        assert_eq!(write_output.kind, "create");

        let read_output = read_file(path.to_string_lossy().as_ref(), Some(1), Some(1), None)
            .expect("read should succeed");
        assert_eq!(read_output.file.content, "two");
    }

    #[test]
    fn edits_file_contents() {
        let path = temp_path("edit.txt");
        write_file(path.to_string_lossy().as_ref(), "alpha beta alpha")
            .expect("initial write should succeed");
        let output = edit_file(path.to_string_lossy().as_ref(), "alpha", "omega", true)
            .expect("edit should succeed");
        assert!(output.replace_all);
    }

    #[test]
    fn rejects_binary_files() {
        let path = temp_path("binary-test.bin");
        std::fs::write(&path, b"\x00\x01\x02\x03binary content").expect("write should succeed");
        let result = read_file(path.to_string_lossy().as_ref(), None, None, None);
        assert!(result.is_err());
        let error = result.unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("binary"));
    }

    #[test]
    fn rejects_oversized_writes() {
        let path = temp_path("oversize-write.txt");
        let huge = "x".repeat(MAX_WRITE_SIZE + 1);
        let result = write_file(path.to_string_lossy().as_ref(), &huge);
        assert!(result.is_err());
        let error = result.unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("too large"));
    }

    #[test]
    fn enforces_workspace_boundary() {
        let workspace = temp_path("workspace-boundary");
        std::fs::create_dir_all(&workspace).expect("workspace dir should be created");
        let inside = workspace.join("inside.txt");
        write_file(inside.to_string_lossy().as_ref(), "safe content")
            .expect("write inside workspace should succeed");

        // Reading inside workspace should succeed
        let result =
            read_file_in_workspace(inside.to_string_lossy().as_ref(), None, None, &workspace);
        assert!(result.is_ok());

        // Reading outside workspace should fail
        let outside = temp_path("outside-boundary.txt");
        write_file(outside.to_string_lossy().as_ref(), "unsafe content")
            .expect("write outside should succeed");
        let result =
            read_file_in_workspace(outside.to_string_lossy().as_ref(), None, None, &workspace);
        assert!(result.is_err());
        let error = result.unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("escapes workspace"));
    }

    #[test]
    fn detects_symlink_escape() {
        let workspace = temp_path("symlink-workspace");
        std::fs::create_dir_all(&workspace).expect("workspace dir should be created");
        let outside = temp_path("symlink-target.txt");
        std::fs::write(&outside, "target content").expect("target should write");

        let link_path = workspace.join("escape-link.txt");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, &link_path).expect("symlink should create");
            assert!(is_symlink_escape(&link_path, &workspace).expect("check should succeed"));
        }

        // Non-symlink file should not be an escape
        let normal = workspace.join("normal.txt");
        std::fs::write(&normal, "normal content").expect("normal file should write");
        assert!(!is_symlink_escape(&normal, &workspace).expect("check should succeed"));
    }

    #[test]
    fn globs_and_greps_directory() {
        let dir = temp_path("search-dir");
        std::fs::create_dir_all(&dir).expect("directory should be created");
        let file = dir.join("demo.rs");
        write_file(
            file.to_string_lossy().as_ref(),
            "fn main() {\n println!(\"hello\");\n}\n",
        )
        .expect("file write should succeed");

        let globbed = glob_search("**/*.rs", Some(dir.to_string_lossy().as_ref()))
            .expect("glob should succeed");
        assert_eq!(globbed.num_files, 1);

        let grep_output = grep_search(&GrepSearchInput {
            pattern: String::from("hello"),
            path: Some(dir.to_string_lossy().into_owned()),
            glob: Some(String::from("**/*.rs")),
            output_mode: Some(String::from("content")),
            before: None,
            after: None,
            context_short: None,
            context: None,
            line_numbers: Some(true),
            case_insensitive: Some(false),
            file_type: None,
            head_limit: Some(10),
            offset: Some(0),
            multiline: Some(false),
        })
        .expect("grep should succeed");
        assert!(grep_output.content.unwrap_or_default().contains("hello"));
    }

    // --- Notebook (.ipynb) tests ---

    #[test]
    fn notebook_render_code_cell_with_output() {
        let path = temp_path("code-cell.ipynb");
        let notebook_json = r#"{
            "cells": [
                {
                    "cell_type": "code",
                    "source": ["print('hello')\n", "print('world')"],
                    "outputs": [
                        {
                            "output_type": "stream",
                            "name": "stdout",
                            "text": ["hello\n", "world\n"]
                        }
                    ]
                }
            ],
            "metadata": {},
            "nbformat": 4,
            "nbformat_minor": 5
        }"#;
        std::fs::write(&path, notebook_json).expect("write notebook");
        let rendered = render_notebook(&path).expect("render should succeed");
        assert!(rendered.contains("--- Cell 1 [code] ---"));
        assert!(rendered.contains("print('hello')"));
        assert!(rendered.contains("print('world')"));
        assert!(rendered.contains("hello\n"));
        assert!(rendered.contains("world\n"));
    }

    #[test]
    fn notebook_render_markdown_cell() {
        let path = temp_path("markdown-cell.ipynb");
        let notebook_json = r##"{
            "cells": [
                {
                    "cell_type": "markdown",
                    "source": "# Title\n\nSome **bold** text.",
                    "metadata": {}
                }
            ],
            "metadata": {},
            "nbformat": 4,
            "nbformat_minor": 5
        }"##;
        std::fs::write(&path, notebook_json).expect("write notebook");
        let rendered = render_notebook(&path).expect("render should succeed");
        assert!(rendered.contains("--- Cell 1 [markdown] ---"));
        assert!(rendered.contains("# Title"));
        assert!(rendered.contains("Some **bold** text."));
        // Markdown cells should NOT have outputs rendered
        assert!(!rendered.contains("output_type"));
    }

    #[test]
    fn notebook_source_as_string_and_array() {
        let path = temp_path("source-formats.ipynb");
        let notebook_json = r#"{
            "cells": [
                {
                    "cell_type": "code",
                    "source": "x = 1",
                    "outputs": []
                },
                {
                    "cell_type": "code",
                    "source": ["y = 2\n", "z = 3"],
                    "outputs": []
                }
            ],
            "metadata": {},
            "nbformat": 4,
            "nbformat_minor": 5
        }"#;
        std::fs::write(&path, notebook_json).expect("write notebook");
        let rendered = render_notebook(&path).expect("render should succeed");
        assert!(rendered.contains("x = 1"));
        assert!(rendered.contains("y = 2"));
        assert!(rendered.contains("z = 3"));
    }

    #[test]
    fn notebook_execute_result_and_error_outputs() {
        let path = temp_path("outputs.ipynb");
        let notebook_json = r#"{
            "cells": [
                {
                    "cell_type": "code",
                    "source": "1 + 1",
                    "outputs": [
                        {
                            "output_type": "execute_result",
                            "data": { "text/plain": "2" },
                            "metadata": {},
                            "execution_count": 1
                        }
                    ]
                },
                {
                    "cell_type": "code",
                    "source": "raise ValueError('bad')",
                    "outputs": [
                        {
                            "output_type": "error",
                            "ename": "ValueError",
                            "evalue": "bad",
                            "traceback": []
                        }
                    ]
                }
            ],
            "metadata": {},
            "nbformat": 4,
            "nbformat_minor": 5
        }"#;
        std::fs::write(&path, notebook_json).expect("write notebook");
        let rendered = render_notebook(&path).expect("render should succeed");
        // execute_result should show the plain text
        assert!(rendered.contains("2"));
        // error output should show ename: evalue
        assert!(rendered.contains("ValueError: bad"));
    }

    #[test]
    fn notebook_read_file_with_offset_limit() {
        let path = temp_path_ext("offset-limit", "ipynb");
        let notebook_json = r#"{
            "cells": [
                {
                    "cell_type": "code",
                    "source": "line_a = 1",
                    "outputs": []
                },
                {
                    "cell_type": "code",
                    "source": "line_b = 2",
                    "outputs": []
                }
            ],
            "metadata": {},
            "nbformat": 4,
            "nbformat_minor": 5
        }"#;
        std::fs::write(&path, notebook_json).expect("write notebook");

        // Read full notebook via read_file
        let full = read_file(path.to_string_lossy().as_ref(), None, None, None)
            .expect("read should succeed");
        assert_eq!(full.kind, "notebook");
        assert!(full.file.total_lines > 0);

        // Read with offset and limit
        let sliced = read_file(path.to_string_lossy().as_ref(), Some(1), Some(2), None)
            .expect("read with offset/limit should succeed");
        assert_eq!(sliced.kind, "notebook");
        assert_eq!(sliced.file.num_lines, 2);
        assert_eq!(sliced.file.start_line, 2);
    }

    #[test]
    fn notebook_invalid_json_returns_error() {
        let path = temp_path_ext("invalid", "ipynb");
        std::fs::write(&path, "this is not json").expect("write should succeed");
        let result = read_file(path.to_string_lossy().as_ref(), None, None, None);
        assert!(result.is_err());
        let error = result.unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("invalid notebook JSON"));
    }

    #[test]
    fn notebook_missing_cells_returns_error() {
        let path = temp_path_ext("no-cells", "ipynb");
        std::fs::write(&path, r#"{"metadata": {}}"#).expect("write should succeed");
        let result = read_file(path.to_string_lossy().as_ref(), None, None, None);
        assert!(result.is_err());
        let error = result.unwrap_err();
        assert!(error.to_string().contains("missing 'cells' array"));
    }

    #[test]
    fn notebook_display_data_output() {
        let path = temp_path("display-data.ipynb");
        let notebook_json = r#"{
            "cells": [
                {
                    "cell_type": "code",
                    "source": "display(df)",
                    "outputs": [
                        {
                            "output_type": "display_data",
                            "data": { "text/plain": ["<DataFrame>\n", "  col1  col2"] },
                            "metadata": {}
                        }
                    ]
                }
            ],
            "metadata": {},
            "nbformat": 4,
            "nbformat_minor": 5
        }"#;
        std::fs::write(&path, notebook_json).expect("write notebook");
        let rendered = render_notebook(&path).expect("render should succeed");
        assert!(rendered.contains("<DataFrame>"));
        assert!(rendered.contains("col1  col2"));
    }

    // --- PDF tests ---

    #[test]
    fn pdf_parse_page_range_single() {
        let pages = parse_page_range("3", 10).expect("should parse single page");
        assert_eq!(pages, vec![2]); // 0-indexed
    }

    #[test]
    fn pdf_parse_page_range_range() {
        let pages = parse_page_range("1-5", 10).expect("should parse range");
        assert_eq!(pages, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn pdf_parse_page_range_mixed() {
        let pages = parse_page_range("1-3,7,9-10", 10).expect("should parse mixed");
        assert_eq!(pages, vec![0, 1, 2, 6, 8, 9]);
    }

    #[test]
    fn pdf_parse_page_range_deduplicates() {
        let pages = parse_page_range("1-3,2-4", 10).expect("should deduplicate");
        assert_eq!(pages, vec![0, 1, 2, 3]); // no duplicate for page 2 and 3
    }

    #[test]
    fn pdf_parse_page_range_exceeds_max() {
        let result = parse_page_range("1-25", 30);
        assert!(result.is_err());
        let error = result.unwrap_err();
        assert!(error.to_string().contains("maximum is 20"));
    }

    #[test]
    fn pdf_parse_page_range_out_of_bounds() {
        let result = parse_page_range("15", 10);
        assert!(result.is_err());
        let error = result.unwrap_err();
        assert!(error.to_string().contains("out of bounds"));
    }

    #[test]
    fn pdf_parse_page_range_zero_page() {
        let result = parse_page_range("0", 10);
        assert!(result.is_err());
        let error = result.unwrap_err();
        assert!(error.to_string().contains("1-based"));
    }

    #[test]
    fn pdf_parse_page_range_invalid_format() {
        let result = parse_page_range("abc", 10);
        assert!(result.is_err());
    }

    #[test]
    fn pdf_parse_page_range_reversed_range() {
        let result = parse_page_range("5-3", 10);
        assert!(result.is_err());
        let error = result.unwrap_err();
        assert!(error.to_string().contains("start (5) > end (3)"));
    }

    #[test]
    fn pdf_too_large() {
        // Verify the constant is sensible
        assert_eq!(MAX_PDF_SIZE, 20 * 1024 * 1024);
        assert_eq!(MAX_PDF_PAGES, 20);
    }

    #[test]
    fn pdf_extension_detected_by_read_file() {
        // A file with .pdf extension but invalid content should return a PDF
        // extraction error, not a "binary" error — proving the PDF path is taken.
        let path = temp_path_ext("not-a-real-pdf", "pdf");
        std::fs::write(&path, b"this is not a real pdf").expect("write should succeed");
        let result = read_file(path.to_string_lossy().as_ref(), None, None, None);
        assert!(result.is_err());
        let error = result.unwrap_err();
        // The error should come from pdf_extract, not the binary check
        assert!(
            error.to_string().contains("PDF")
                || error.to_string().contains("pdf"),
            "expected PDF-related error, got: {}",
            error
        );
    }

    #[test]
    fn pdf_pages_param_ignored_for_non_pdf() {
        // pages parameter should be ignored for non-PDF files
        let path = temp_path("text-with-pages.txt");
        write_file(path.to_string_lossy().as_ref(), "hello\nworld")
            .expect("write should succeed");
        let result = read_file(
            path.to_string_lossy().as_ref(),
            None,
            None,
            Some("1-3"),
        );
        // Should succeed — pages param is simply ignored for non-PDF files
        assert!(result.is_ok());
    }

    // --- Image tests ---

    #[test]
    fn test_is_image_extension() {
        assert!(is_image_extension(std::path::Path::new("photo.png")));
        assert!(is_image_extension(std::path::Path::new("photo.PNG")));
        assert!(is_image_extension(std::path::Path::new("photo.jpg")));
        assert!(is_image_extension(std::path::Path::new("photo.JPG")));
        assert!(is_image_extension(std::path::Path::new("photo.jpeg")));
        assert!(is_image_extension(std::path::Path::new("photo.JPEG")));
        assert!(is_image_extension(std::path::Path::new("photo.gif")));
        assert!(is_image_extension(std::path::Path::new("photo.GIF")));
        assert!(is_image_extension(std::path::Path::new("photo.webp")));
        assert!(is_image_extension(std::path::Path::new("photo.WEBP")));
        // Non-image extensions
        assert!(!is_image_extension(std::path::Path::new("file.txt")));
        assert!(!is_image_extension(std::path::Path::new("file.pdf")));
        assert!(!is_image_extension(std::path::Path::new("file.rs")));
        assert!(!is_image_extension(std::path::Path::new("file.bin")));
        assert!(!is_image_extension(std::path::Path::new("file.svg")));
        assert!(!is_image_extension(std::path::Path::new("noext")));
    }

    #[test]
    fn test_media_type_for_extension() {
        assert_eq!(media_type_for_extension("png"), Some("image/png"));
        assert_eq!(media_type_for_extension("PNG"), Some("image/png"));
        assert_eq!(media_type_for_extension("jpg"), Some("image/jpeg"));
        assert_eq!(media_type_for_extension("JPG"), Some("image/jpeg"));
        assert_eq!(media_type_for_extension("jpeg"), Some("image/jpeg"));
        assert_eq!(media_type_for_extension("JPEG"), Some("image/jpeg"));
        assert_eq!(media_type_for_extension("gif"), Some("image/gif"));
        assert_eq!(media_type_for_extension("GIF"), Some("image/gif"));
        assert_eq!(media_type_for_extension("webp"), Some("image/webp"));
        assert_eq!(media_type_for_extension("WEBP"), Some("image/webp"));
        assert_eq!(media_type_for_extension("txt"), None);
        assert_eq!(media_type_for_extension("svg"), None);
        assert_eq!(media_type_for_extension("bmp"), None);
    }

    #[test]
    fn test_read_image_base64() {
        let path = temp_path_ext("test-image", "png");
        // Write a small fake PNG (just bytes, not a real PNG — we're testing
        // the base64 encoding, not image validity).
        let fake_png = b"\x89PNG\r\n\x1a\nhello image bytes";
        std::fs::write(&path, fake_png).expect("write should succeed");

        let output = read_image(&path).expect("read_image should succeed");
        assert_eq!(output.kind, "image");
        assert_eq!(output.file.num_lines, 1);
        assert_eq!(output.file.start_line, 1);
        assert_eq!(output.file.total_lines, 1);

        // Parse the content as JSON and verify structure
        let parsed: serde_json::Value =
            serde_json::from_str(&output.file.content).expect("content should be valid JSON");
        assert_eq!(parsed["type"], "image");
        assert_eq!(parsed["source"]["type"], "base64");
        assert_eq!(parsed["source"]["media_type"], "image/png");

        // Verify the base64 data round-trips correctly
        use base64::Engine;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(parsed["source"]["data"].as_str().unwrap())
            .expect("base64 should decode");
        assert_eq!(decoded, fake_png);
    }

    #[test]
    fn test_read_image_too_large() {
        // Verify the size constant is 20 MB
        assert_eq!(MAX_IMAGE_SIZE, 20 * 1024 * 1024);
    }

    #[test]
    fn test_image_extension_detected_by_read_file() {
        // A file with an image extension should be handled by the image path,
        // not rejected as binary.
        let path = temp_path_ext("image-via-read-file", "png");
        let fake_data = b"\x89PNG\r\nfake data\x00\x01\x02";
        std::fs::write(&path, fake_data).expect("write should succeed");

        let result = read_file(path.to_string_lossy().as_ref(), None, None, None);
        assert!(result.is_ok(), "image should not be rejected as binary");
        let output = result.unwrap();
        assert_eq!(output.kind, "image");
    }

    #[test]
    fn test_non_image_binary_still_rejected() {
        // A .bin file with binary content should still be rejected
        let path = temp_path_ext("binary-not-image", "bin");
        std::fs::write(&path, b"\x00\x01\x02binary content").expect("write should succeed");

        let result = read_file(path.to_string_lossy().as_ref(), None, None, None);
        assert!(result.is_err());
        let error = result.unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("binary"));
    }

    #[test]
    fn test_read_image_jpg_media_type() {
        let path = temp_path_ext("test-image", "jpg");
        std::fs::write(&path, b"fake jpeg data").expect("write should succeed");

        let output = read_image(&path).expect("read_image should succeed");
        let parsed: serde_json::Value =
            serde_json::from_str(&output.file.content).expect("content should be valid JSON");
        assert_eq!(parsed["source"]["media_type"], "image/jpeg");
    }

    #[test]
    fn test_read_image_webp_media_type() {
        let path = temp_path_ext("test-image", "webp");
        std::fs::write(&path, b"fake webp data").expect("write should succeed");

        let output = read_image(&path).expect("read_image should succeed");
        let parsed: serde_json::Value =
            serde_json::from_str(&output.file.content).expect("content should be valid JSON");
        assert_eq!(parsed["source"]["media_type"], "image/webp");
    }
}
