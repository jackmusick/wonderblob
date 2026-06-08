//! Shared helpers for object-store backends (S3, Azure Blob) that synthesize a
//! directory tree over a flat key namespace. Buckets/containers surface as the
//! root listing; "/bucket/prefix/..." addresses keys inside.

/// 8 MiB part/block size for multipart (S3) and block-list (Azure) uploads.
/// Above S3's 5 MiB minimum part size for all parts except the last.
pub const PART_SIZE: usize = 8 * 1024 * 1024;

/// A normalized object-store path split into container + key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjPath {
    /// Bucket (S3) or container (Azure). `None` only for the synthetic root "/".
    pub container: Option<String>,
    /// Key/blob name within the container; "" addresses the container root.
    pub key: String,
}

impl ObjPath {
    /// Parse "/", "/bucket", "/bucket/", "/bucket/a/b.txt".
    pub fn parse(path: &str) -> Self {
        let trimmed = path.trim_start_matches('/');
        if trimmed.is_empty() {
            return ObjPath {
                container: None,
                key: String::new(),
            };
        }
        match trimmed.split_once('/') {
            None => ObjPath {
                container: Some(trimmed.to_string()),
                key: String::new(),
            },
            Some((c, k)) => ObjPath {
                container: Some(c.to_string()),
                key: k.trim_start_matches('/').to_string(),
            },
        }
    }

    pub fn is_root(&self) -> bool {
        self.container.is_none()
    }

    pub fn is_container_root(&self) -> bool {
        self.container.is_some() && self.key.is_empty()
    }
}

/// Final path segment: "/a/b.txt" -> "b.txt", "/a/b/" -> "b".
pub fn basename(path: &str) -> String {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_splits_container_and_key() {
        assert_eq!(
            ObjPath::parse("/"),
            ObjPath {
                container: None,
                key: "".into()
            }
        );
        assert!(ObjPath::parse("/").is_root());
        assert_eq!(
            ObjPath::parse("/wbtest"),
            ObjPath {
                container: Some("wbtest".into()),
                key: "".into()
            }
        );
        assert!(ObjPath::parse("/wbtest").is_container_root());
        assert!(ObjPath::parse("/wbtest/").is_container_root());
        assert_eq!(
            ObjPath::parse("/wbtest/a/b.txt"),
            ObjPath {
                container: Some("wbtest".into()),
                key: "a/b.txt".into()
            }
        );
    }

    #[test]
    fn basename_strips_dirs_and_trailing_slash() {
        assert_eq!(basename("/a/b.txt"), "b.txt");
        assert_eq!(basename("/a/b/"), "b");
        assert_eq!(basename("solo"), "solo");
    }
}
