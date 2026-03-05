use std::path::{Path, PathBuf};

use anyhow::Result;

/// Directories to always skip when enumerating `.rs` files.
const SKIP_DIRS: &[&str] = &["target", "vendor", ".git", ".kuriboh", ".claude"];

/// Recursively enumerate `.rs` files under `dir`, skipping non-production paths.
///
/// If `filter_tests` is true, also skips files matching `*_test.rs` and
/// paths containing `/tests/` or `/benches/`. This is enabled when the
/// codebase has >300 `.rs` files to keep scouting costs manageable.
pub fn enumerate_files(dir: &Path, filter_tests: bool) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    enumerate_recursive(dir, dir, filter_tests, &mut files)?;
    files.sort();
    Ok(files)
}

fn enumerate_recursive(
    root: &Path,
    dir: &Path,
    filter_tests: bool,
    out: &mut Vec<PathBuf>,
) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if SKIP_DIRS.contains(&name) {
                continue;
            }
            if filter_tests && (name == "tests" || name == "benches") {
                continue;
            }
            enumerate_recursive(root, &path, filter_tests, out)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            if filter_tests {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    if stem.ends_with("_test") {
                        continue;
                    }
                }
            }
            // Store as relative path from root.
            if let Ok(rel) = path.strip_prefix(root) {
                out.push(rel.to_path_buf());
            } else {
                out.push(path);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn setup_tree(dir: &Path) {
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.rs"), "fn main() {}").unwrap();
        fs::write(dir.join("src/lib.rs"), "pub fn hello() {}").unwrap();
        fs::write(dir.join("src/foo_test.rs"), "// test").unwrap();
        fs::create_dir_all(dir.join("tests")).unwrap();
        fs::write(dir.join("tests/integration.rs"), "// int test").unwrap();
        fs::create_dir_all(dir.join("target/debug")).unwrap();
        fs::write(dir.join("target/debug/main.rs"), "// build artifact").unwrap();
        fs::create_dir_all(dir.join(".git")).unwrap();
        fs::write(dir.join(".git/hooks.rs"), "// git").unwrap();
    }

    #[test]
    fn enumerate_skips_target_and_git() {
        let dir = tempfile::tempdir().unwrap();
        setup_tree(dir.path());

        let files = enumerate_files(dir.path(), false).unwrap();
        let names: Vec<&str> = files.iter().filter_map(|p| p.to_str()).collect();

        assert!(names.contains(&"src/main.rs"));
        assert!(names.contains(&"src/lib.rs"));
        assert!(names.contains(&"src/foo_test.rs"));
        assert!(names.contains(&"tests/integration.rs"));
        assert!(!names.iter().any(|n| n.contains("target")));
        assert!(!names.iter().any(|n| n.contains(".git")));
    }

    #[test]
    fn enumerate_with_test_filter() {
        let dir = tempfile::tempdir().unwrap();
        setup_tree(dir.path());

        let files = enumerate_files(dir.path(), true).unwrap();
        let names: Vec<&str> = files.iter().filter_map(|p| p.to_str()).collect();

        assert!(names.contains(&"src/main.rs"));
        assert!(names.contains(&"src/lib.rs"));
        assert!(!names.contains(&"src/foo_test.rs"));
        assert!(!names.iter().any(|n| n.contains("tests/")));
    }
}
