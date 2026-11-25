//! A session's changes and files, against a real repository and worktree.

use agenthost_host::files::{changes_of, list_of, read_of, Source};
use agenthost_proto::jsonrpc::codes;
use agenthost_proto::*;
use std::path::Path;
use std::process::Command;

fn tempdir() -> tempfile::TempDir {
    let root = std::env::var_os("AGENTHOST_TEST_TMP").unwrap_or_else(|| std::env::temp_dir().into_os_string());
    tempfile::Builder::new().prefix("ah").tempdir_in(root).unwrap()
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false", "-c", "init.defaultBranch=main"])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// A repository with one commit, and a worktree of it on branch `s`.
fn repo(t: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let (repo, wt) = (t.join("repo"), t.join("wt"));
    std::fs::create_dir_all(repo.join("src")).unwrap();
    git(&repo, &["init", "-q"]);
    std::fs::write(repo.join("src/a.rs"), "fn a() {}\nfn b() {}\n").unwrap();
    std::fs::write(repo.join("README.md"), "# Hi\n").unwrap();
    std::fs::write(repo.join(".gitignore"), "target/\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "one"]);
    git(&repo, &["worktree", "add", "-q", "-b", "s", wt.to_str().unwrap(), "HEAD"]);
    (repo, wt)
}

#[tokio::test]
async fn changes_cover_commits_uncommitted_work_and_new_files() {
    let t = tempdir();
    let (repo, wt) = repo(t.path());
    assert_eq!(changes_of(&repo, Some(&wt), "s").await.unwrap().patch, "");

    std::fs::write(wt.join("src/a.rs"), "fn a() {}\nfn c() {}\n").unwrap();
    git(&wt, &["commit", "-q", "-am", "two"]);
    std::fs::write(wt.join("README.md"), "# Hello\n").unwrap();
    std::fs::write(wt.join("new file.txt"), "one\ntwo").unwrap();
    std::fs::write(wt.join("blob.bin"), [0u8, 1, 2]).unwrap();
    std::fs::create_dir_all(wt.join("target")).unwrap();
    std::fs::write(wt.join("target/out"), "ignored").unwrap();
    // The workspace moving on doesn't count as the session's change.
    std::fs::write(repo.join("other.txt"), "theirs\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "theirs"]);

    let c = changes_of(&repo, Some(&wt), "s").await.unwrap();
    assert_eq!(c.branch, "s");
    assert!(!c.truncated);
    let p = &c.patch;
    assert!(p.contains("-fn b() {}\n+fn c() {}\n"), "{p}");
    assert!(p.contains("-# Hi\n+# Hello\n"), "{p}");
    assert!(p.contains("diff --git a/new file.txt b/new file.txt\nnew file mode 100644\n--- /dev/null\n+++ b/new file.txt\n@@ -0,0 +1,2 @@\n+one\n+two\n\\ No newline at end of file\n"), "{p}");
    assert!(p.contains("Binary files /dev/null and b/blob.bin differ"), "{p}");
    assert!(!p.contains("other.txt") && !p.contains("target/out"), "{p}");

    // Without the worktree (an archived session), the branch's commits are left.
    let c = changes_of(&repo, None, "s").await.unwrap();
    assert!(c.patch.contains("+fn c() {}") && !c.patch.contains("Hello"), "{}", c.patch);
}

#[tokio::test]
async fn files_list_what_git_would_and_plain_folders_whole() {
    let t = tempdir();
    let (repo, wt) = repo(t.path());
    std::fs::write(wt.join("new.txt"), "x").unwrap();
    std::fs::create_dir_all(wt.join("target")).unwrap();
    std::fs::write(wt.join("target/out"), "ignored").unwrap();
    let f = list_of(Source::Dir { root: wt.clone(), git: true }).await.unwrap();
    assert_eq!(f.files, [".gitignore", "README.md", "new.txt", "src/a.rs"]);

    let f = list_of(Source::Branch { repo: repo.clone(), branch: "s".into() }).await.unwrap();
    assert_eq!(f.files, [".gitignore", "README.md", "src/a.rs"]);

    let plain = t.path().join("plain");
    std::fs::create_dir_all(plain.join("a/.git")).unwrap();
    std::fs::write(plain.join("a/.git/config"), "").unwrap();
    std::fs::write(plain.join("a/b.txt"), "").unwrap();
    std::fs::write(plain.join("z.txt"), "").unwrap();
    let f = list_of(Source::Dir { root: plain, git: false }).await.unwrap();
    assert_eq!(f.files, ["a/b.txt", "z.txt"]);
}

#[tokio::test]
async fn reads_stay_inside_the_folder() {
    let t = tempdir();
    let (repo, wt) = repo(t.path());
    let dir = || Source::Dir { root: wt.clone(), git: true };
    let f = read_of(dir(), "src/a.rs").await.unwrap();
    assert_eq!((f.text.as_deref(), f.size, f.truncated), (Some("fn a() {}\nfn b() {}\n"), 20, false));

    std::fs::write(t.path().join("secret"), "s3cret").unwrap();
    std::os::unix::fs::symlink(t.path().join("secret"), wt.join("link")).unwrap();
    std::os::unix::fs::symlink("src/a.rs", wt.join("inside")).unwrap();
    for bad in ["../secret", "/etc/passwd", "src/../../secret", "", "link"] {
        let e = read_of(dir(), bad).await.unwrap_err();
        assert_eq!(e.code, codes::INVALID_PARAMS, "{bad}: {e:?}");
    }
    assert!(read_of(dir(), "inside").await.unwrap().text.is_some());
    assert_eq!(read_of(dir(), "nope.rs").await.unwrap_err().code, codes::NOT_FOUND);
    assert_eq!(read_of(dir(), "src").await.unwrap_err().code, codes::INVALID_PARAMS);

    std::fs::write(wt.join("blob.bin"), [0u8, 159, 146, 150]).unwrap();
    let f = read_of(dir(), "blob.bin").await.unwrap();
    assert_eq!((f.text, f.size), (None, 4));

    // A long file comes back cut, on a character boundary.
    let long = "é".repeat(FILE_READ_MAX_BYTES / 2 + 10);
    std::fs::write(wt.join("long.txt"), format!("a{long}")).unwrap();
    let f = read_of(dir(), "long.txt").await.unwrap();
    assert!(f.truncated && f.size as usize == long.len() + 1);
    assert_eq!(f.text.unwrap().len(), FILE_READ_MAX_BYTES - 1);

    let f = read_of(Source::Branch { repo, branch: "s".into() }, "README.md").await.unwrap();
    assert_eq!(f.text.as_deref(), Some("# Hi\n"));
}

/// With a remote, changes are measured against its main branch as it is now,
/// not the local one, which may be behind.
#[tokio::test]
async fn changes_compare_with_the_remotes_main_branch() {
    let t = tempdir();
    let (repo, wt) = repo(t.path());
    let origin = t.path().join("origin.git");
    git(t.path(), &["clone", "-q", "--bare", repo.to_str().unwrap(), origin.to_str().unwrap()]);
    git(&repo, &["remote", "add", "origin", origin.to_str().unwrap()]);
    git(&repo, &["fetch", "-q", "origin"]);

    // Someone else's commit lands on the remote; the local main never hears of it.
    let other = t.path().join("other");
    git(t.path(), &["clone", "-q", origin.to_str().unwrap(), other.to_str().unwrap()]);
    std::fs::write(other.join("upstream.txt"), "theirs\n").unwrap();
    git(&other, &["add", "."]);
    git(&other, &["commit", "-q", "-m", "upstream"]);
    git(&other, &["push", "-q", "origin", "HEAD"]);
    // The session brings its branch up to date with the remote, then changes a file.
    git(&wt, &["pull", "-q", "--no-rebase", origin.to_str().unwrap(), "HEAD"]);
    std::fs::write(wt.join("README.md"), "# Hello\n").unwrap();
    // The local branch wanders off; it isn't what the session is compared with.
    std::fs::write(repo.join("local.txt"), "mine\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "local only"]);

    let c = changes_of(&repo, Some(&wt), "s").await.unwrap();
    assert_eq!(c.against.as_deref(), Some("origin/main"));
    assert!(c.patch.contains("+# Hello"), "{}", c.patch);
    assert!(!c.patch.contains("upstream.txt") && !c.patch.contains("local.txt"), "{}", c.patch);

    // No remote: the branch the workspace has checked out.
    let t2 = tempdir();
    let (repo2, wt2) = self::repo(t2.path());
    assert_eq!(changes_of(&repo2, Some(&wt2), "s").await.unwrap().against, None);
}
