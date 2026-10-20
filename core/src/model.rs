use serde::Serialize;

/// A contiguous run of file data on the device. `offset == None` means a
/// sparse run (reads as zeros).
#[derive(Debug, Clone)]
pub struct Extent {
    pub offset: Option<u64>,
    pub len: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Ntfs,
    Fat,
    Exfat,
    Carved,
}

/// How likely the recovered content is intact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Quality {
    /// Data clusters are still unallocated / structure fully validated.
    Good,
    /// Some clusters were reused, or the file was truncated.
    Poor,
    /// Not determinable.
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
pub struct FoundFile {
    pub id: u64,
    pub name: String,
    /// Folder path inside the volume ("/" separated), or a virtual group for
    /// carved files.
    pub path: String,
    pub size: u64,
    pub ext: String,
    pub category: &'static str,
    pub source: Source,
    pub deleted: bool,
    /// Last-modified time, unix seconds.
    pub modified: Option<i64>,
    /// FAT/exFAT store local wall-clock time without a zone: `modified` is
    /// then that wall-clock time encoded as if UTC.
    pub naive_time: bool,
    pub quality: Quality,
    /// Absolute byte offset of the first data block (for display / debug).
    pub offset: Option<u64>,
    /// Number of data runs (1 = contiguous).
    pub fragments: usize,
    #[serde(skip)]
    pub extents: Vec<Extent>,
    #[serde(skip)]
    pub resident: Option<Vec<u8>>,
}

pub fn ext_of(name: &str) -> String {
    match name.rfind('.') {
        Some(i) if i > 0 && i + 1 < name.len() && name.len() - i <= 8 => name[i + 1..].to_ascii_lowercase(),
        _ => String::new(),
    }
}

pub fn category_of(ext: &str) -> &'static str {
    match ext {
        "jpg" | "jpeg" | "png" | "gif" | "bmp" | "tif" | "tiff" | "webp" | "heic" | "heif" | "raw" | "cr2" | "nef"
        | "arw" | "dng" | "psd" | "svg" | "ico" => "image",
        "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "pdf" | "txt" | "rtf" | "odt" | "ods" | "odp" | "csv"
        | "xml" | "html" | "htm" | "md" | "hwp" | "json" => "document",
        "mp4" | "mov" | "avi" | "mkv" | "wmv" | "flv" | "3gp" | "m4v" | "webm" | "mpg" | "mpeg" | "ts" => "video",
        "mp3" | "wav" | "m4a" | "aac" | "flac" | "ogg" | "wma" | "amr" | "opus" => "audio",
        "zip" | "rar" | "7z" | "gz" | "tar" | "bz2" | "xz" | "iso" | "cab" => "archive",
        _ => "other",
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct VolumeInfo {
    pub offset: u64,
    pub size: u64,
    pub filesystem: String,
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Progress {
    /// "fs" | "carve" | "done"
    pub phase: String,
    pub message: String,
    pub done: u64,
    pub total: u64,
    pub found: u64,
}
