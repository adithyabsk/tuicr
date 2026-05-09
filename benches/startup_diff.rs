//! Startup diff benchmarks: `cargo bench --bench startup_diff`
//!
//! **deferred/** — full diffs (expensive, loaded on-demand after commit selection)
//! **startup/**  — lightweight existence checks (cheap, called eagerly at launch)

use std::fs;
use std::path::Path;

use criterion::{Criterion, criterion_group, criterion_main};
use git2::Repository;

use tuicr::syntax::SyntaxHighlighter;
use tuicr::vcs::git::diff::{get_staged_diff, get_unstaged_diff, get_working_tree_diff};

/// Create a repo with `num_dirs` directories × `files_per_dir` tracked files,
/// plus 20 untracked directories × 50 files each, with one tracked file modified.
fn create_large_repo(dir: &Path, num_dirs: usize, files_per_dir: usize) -> Repository {
    let repo = Repository::init(dir).expect("failed to init repo");
    let mut index = repo.index().expect("failed to open index");

    for d in 0..num_dirs {
        let subdir = dir.join(format!("dir_{d:04}"));
        fs::create_dir_all(&subdir).expect("mkdir failed");
        for f in 0..files_per_dir {
            let rel = format!("dir_{d:04}/file_{f:04}.py");
            fs::write(dir.join(&rel), format!("# file {d}/{f}\n")).expect("write failed");
            index.add_path(Path::new(&rel)).expect("add failed");
        }
    }
    index.write().expect("index write failed");
    let tree_id = index.write_tree().expect("write tree failed");
    {
        let tree = repo.find_tree(tree_id).expect("find tree failed");
        let sig = git2::Signature::now("Test", "t@t.com").expect("sig");
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .expect("commit failed");
    }

    fs::write(dir.join("dir_0000/file_0000.py"), "# changed\n").expect("write failed");

    // Add untracked directories
    for d in 0..20 {
        let udir = dir.join(format!("untracked_{d:04}"));
        fs::create_dir_all(&udir).expect("mkdir");
        for f in 0..50 {
            fs::write(udir.join(format!("ut_{f:04}.txt")), "untracked\n").expect("write");
        }
    }

    repo
}

fn bench_startup_diffs(c: &mut Criterion) {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let repo = create_large_repo(temp_dir.path(), 100, 50);
    let hl = SyntaxHighlighter::default();

    // --- deferred/ : full diff computation (called on-demand, not at startup) ---
    {
        let mut group = c.benchmark_group("deferred");

        group.bench_function("get_staged_diff", |b| {
            b.iter(|| { let _ = get_staged_diff(&repo, &hl); });
        });

        group.bench_function("get_unstaged_diff", |b| {
            b.iter(|| get_unstaged_diff(&repo, &hl).unwrap());
        });

        group.bench_function("get_working_tree_diff", |b| {
            b.iter(|| get_working_tree_diff(&repo, &hl).unwrap());
        });

        group.finish();
    }

    // --- startup/ : lightweight checks (called eagerly at launch) ---
    {
        let mut group = c.benchmark_group("startup");

        group.bench_function("has_staged_changes", |b| {
            b.iter(|| {
                let head_tree = repo.head().ok().and_then(|h| h.peel_to_tree().ok());
                let idx = repo.index().expect("index");
                repo.diff_tree_to_index(head_tree.as_ref(), Some(&idx), None)
                    .map(|d| d.deltas().next().is_some())
                    .unwrap_or(false)
            });
        });

        group.bench_function("has_unstaged_changes", |b| {
            b.iter(|| {
                let idx = repo.index().expect("index");
                repo.diff_index_to_workdir(Some(&idx), None)
                    .map(|d| d.deltas().next().is_some())
                    .unwrap_or(false)
            });
        });

        group.finish();
    }
}

criterion_group!(benches, bench_startup_diffs);
criterion_main!(benches);
