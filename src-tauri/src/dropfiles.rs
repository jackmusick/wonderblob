//! Map dropped OS paths to a flat list of file paths to upload.
//!
//! Tauri delivers OS drops as filesystem *paths* (not bytes) via the webview
//! `onDragDropEvent` — the native layer intercepts the drop, so DOM `ondrop`
//! never fires (see tauri-apps/tauri#14373). We hand each resolved path to the
//! existing, tested `enqueue_upload` path.
//!
//! v1 scope: top-level files + the immediate file children of dropped
//! directories (one level). Recursive trees with remote `mkdir` mirroring are a
//! tracked post-v1 enhancement.

/// Expand dropped paths into a flat list of file paths to upload.
///
/// - A dropped file is passed through.
/// - A dropped directory contributes its immediate file children (one level);
///   nested directories are skipped in v1.
/// - Paths that no longer exist are silently dropped.
pub fn expand_dropped(paths: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for p in paths {
        let path = std::path::Path::new(p);
        if path.is_dir() {
            if let Ok(rd) = std::fs::read_dir(path) {
                for e in rd.flatten() {
                    if e.path().is_file() {
                        out.push(e.path().to_string_lossy().into_owned());
                    }
                }
            }
        } else if path.is_file() {
            out.push(p.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_dir_to_immediate_files_only() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.txt"), b"a").unwrap();
        std::fs::write(d.path().join("b.txt"), b"b").unwrap();
        std::fs::create_dir(d.path().join("sub")).unwrap();
        // One level deeper: must be skipped in v1.
        std::fs::write(d.path().join("sub/c.txt"), b"c").unwrap();
        let mut got = expand_dropped(&[d.path().to_string_lossy().into()]);
        got.sort();
        assert_eq!(got.len(), 2); // a.txt + b.txt, NOT sub/c.txt
        assert!(got
            .iter()
            .all(|p| p.ends_with("a.txt") || p.ends_with("b.txt")));
    }

    #[test]
    fn passes_plain_files_through() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("x.bin");
        std::fs::write(&f, b"x").unwrap();
        assert_eq!(
            expand_dropped(&[f.to_string_lossy().into()]),
            vec![f.to_string_lossy().to_string()]
        );
    }

    #[test]
    fn skips_nonexistent_paths() {
        let d = tempfile::tempdir().unwrap();
        let missing = d.path().join("nope.txt");
        assert!(expand_dropped(&[missing.to_string_lossy().into()]).is_empty());
    }

    #[test]
    fn mixes_files_and_dirs() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("loose.txt");
        std::fs::write(&f, b"x").unwrap();
        let sub = d.path().join("folder");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("inside.txt"), b"y").unwrap();
        let mut got = expand_dropped(&[f.to_string_lossy().into(), sub.to_string_lossy().into()]);
        got.sort();
        assert_eq!(got.len(), 2);
    }
}
