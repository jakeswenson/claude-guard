//! What the guard knows about the repository around `cwd`.

use std::path::Path;

/// True when `cwd` or any ancestor holds a `.jj` directory. A colocated
/// repo has both `.jj` and `.git`, and counts as jj.
pub fn in_jj_repo(cwd: &Path) -> bool {
  cwd.ancestors().any(|dir| dir.join(".jj").is_dir())
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::fs;

  #[test]
  fn a_jj_directory_anywhere_above_cwd_counts() {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("repo");
    fs::create_dir_all(repo.join(".jj")).unwrap();
    fs::create_dir_all(repo.join(".git")).unwrap();
    let deep = repo.join("src").join("nested");
    fs::create_dir_all(&deep).unwrap();

    assert!(in_jj_repo(&repo));
    assert!(in_jj_repo(&deep));
  }

  #[test]
  fn a_plain_git_repo_does_not_count() {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("repo");
    fs::create_dir_all(repo.join(".git")).unwrap();

    assert!(!in_jj_repo(&repo));
  }

  #[test]
  fn a_jj_file_is_not_a_jj_directory() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join(".jj"), "").unwrap();

    assert!(!in_jj_repo(root.path()));
  }
}
