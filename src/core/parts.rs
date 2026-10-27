//! Where stream downloads keep their parts until they are joined: the
//! "Temporary directory" (Options > Save to), or next to the file.

use std::path::{Path, PathBuf};
use std::sync::RwLock;

static PARTS_ROOT: RwLock<Option<PathBuf>> = RwLock::new(None);

/// Sets the temporary directory (None keeps parts next to their file).
pub fn set_parts_root(root: Option<PathBuf>) {
    if let Ok(mut guard) = PARTS_ROOT.write() {
        *guard = root.filter(|r| !r.as_os_str().is_empty());
    }
}

/// The folder for the parts of `output` (`suffix` such as ".hls_parts").
/// A download that already keeps its parts next to the file goes on there.
pub fn parts_dir(output: &Path, suffix: &str) -> PathBuf {
    let mut name = output.as_os_str().to_os_string();
    name.push(suffix);
    let beside = PathBuf::from(name);
    let root = PARTS_ROOT.read().ok().and_then(|g| g.clone());
    match root {
        Some(root) if !beside.exists() => {
            use sha2::{Digest, Sha256};
            let hash = Sha256::digest(output.to_string_lossy().as_bytes());
            let tag: String = hash.iter().take(4).map(|b| format!("{:02x}", b)).collect();
            let file = output.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_else(|| "download".to_string());
            root.join(format!("{}.{}{}", file, tag, suffix))
        }
        _ => beside,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parts_go_to_the_temporary_directory() {
        let out = Path::new("C:/Downloads/Video/clip.mp4");
        set_parts_root(None);
        assert_eq!(parts_dir(out, ".hls_parts"), PathBuf::from("C:/Downloads/Video/clip.mp4.hls_parts"));
        set_parts_root(Some(PathBuf::from("D:/Temp")));
        let p = parts_dir(out, ".hls_parts");
        assert!(p.starts_with("D:/Temp") && p.to_string_lossy().ends_with(".hls_parts"), "{}", p.display());
        assert_eq!(p, parts_dir(out, ".hls_parts"), "the same file always maps to the same folder");
        set_parts_root(None);
    }
}
