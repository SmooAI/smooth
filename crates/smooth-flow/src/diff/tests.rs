//! Temp-repo tests for the whole pipeline: bases, turn snapshots, hunk
//! actions (including stale refusal), noise, caps and the review message.

use super::git::{run, snapshot_tree, tests::repo};
use super::*;

fn snap(seq: i64, kind: SnapKind, tree: &str) -> Snapshot {
    Snapshot {
        seq,
        kind,
        tree: tree.to_string(),
        at: format!("2026-10-02T00:00:0{seq}Z"),
    }
}

fn file<'a>(d: &'a Diff, path: &str) -> &'a DiffFile {
    d.files.iter().find(|f| f.path == path).unwrap_or_else(|| panic!("no {path} in {:?}", d.files.iter().map(|f| &f.path).collect::<Vec<_>>()))
}

fn texts(h: &Hunk) -> Vec<String> {
    h.lines
        .iter()
        .map(|l| {
            let p = match l.kind {
                LineKind::Add => '+',
                LineKind::Del => '-',
                LineKind::Ctx => ' ',
            };
            format!("{p}{}", l.text)
        })
        .collect()
}

#[test]
fn uncommitted_includes_untracked_and_counts() {
    let d = repo();
    let p = d.path();
    std::fs::write(p.join("a.txt"), "one\nTWO\nthree\n").unwrap();
    std::fs::write(p.join("new.rs"), "fn main() {}\n").unwrap();
    let diff = compute(p, DiffBase::Uncommitted, &[], None).unwrap();
    assert_eq!(diff.base, DiffBase::Uncommitted);
    assert_eq!(diff.from.label, "HEAD");
    assert_eq!(diff.to.label, "worktree");
    assert_eq!((diff.added, diff.deleted), (2, 1));
    let a = file(&diff, "a.txt");
    assert_eq!(a.status, FileStatus::Modified);
    assert_eq!(texts(&a.hunks[0]), vec![" one", "-two", "+TWO", " three"]);
    assert_eq!(a.hunks[0].lines[1].words, vec![[0, 3]], "the whole word changed, paired");
    let n = file(&diff, "new.rs");
    assert_eq!(n.status, FileStatus::Added);
    assert_eq!(n.language.as_deref(), Some("Rust"));
    assert!(!n.hunks[0].lines[0].syntax.is_empty(), "server-side syntax spans");
    assert_eq!(diff.legend.len(), TokenKind::NAMES.len());
}

#[test]
fn branch_is_against_the_merge_base() {
    let d = repo();
    let p = d.path();
    run(p, &["checkout", "-q", "-b", "feature"]).unwrap();
    std::fs::write(p.join("b.txt"), "b\n").unwrap();
    run(p, &["add", "b.txt"]).unwrap();
    run(p, &["commit", "-q", "-m", "b"]).unwrap();
    std::fs::write(p.join("c.txt"), "c\n").unwrap();
    let diff = compute(p, DiffBase::Branch, &[], None).unwrap();
    assert!(diff.from.label.contains("main"), "{:?}", diff.from);
    let paths: Vec<_> = diff.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, vec!["b.txt", "c.txt"], "committed and uncommitted work on the branch");
    let unc = compute(p, DiffBase::Uncommitted, &[], None).unwrap();
    assert_eq!(unc.files.len(), 1);
}

#[test]
fn turn_is_start_to_end_when_idle_and_start_to_now_when_live() {
    let d = repo();
    let p = d.path();
    let t0 = snapshot_tree(p).unwrap();
    std::fs::write(p.join("a.txt"), "one\ntwo\nthree\nfour\n").unwrap();
    let t1 = snapshot_tree(p).unwrap();
    // The user edits after the turn ended: not the agent's change.
    std::fs::write(p.join("mine.txt"), "mine\n").unwrap();
    let snaps = vec![snap(1, SnapKind::Start, &t0), snap(2, SnapKind::End, &t1)];
    let diff = compute(p, DiffBase::Turn, &snaps, None).unwrap();
    let turn = diff.turn.clone().unwrap();
    assert!(!turn.live);
    assert_eq!(turn.seq, 2);
    assert_eq!(diff.files.len(), 1, "{:?}", diff.files);
    assert_eq!(texts(&diff.files[0].hunks[0]).last().unwrap(), "+four");

    // A turn in flight: start → now.
    let live = vec![snap(1, SnapKind::Start, &t0), snap(2, SnapKind::End, &t1), snap(3, SnapKind::Start, &t1)];
    let diff = compute(p, DiffBase::Turn, &live, None).unwrap();
    assert!(diff.turn.unwrap().live);
    assert_eq!(diff.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), vec!["mine.txt"]);

    // No snapshots: an empty diff that says why.
    let none = compute(p, DiffBase::Turn, &[], None).unwrap();
    assert!(none.files.is_empty());
    assert!(none.note.unwrap().contains("No turn recorded yet"));
    // Two ends without a start: end → end.
    let ends = vec![snap(1, SnapKind::End, &t0), snap(2, SnapKind::End, &t1)];
    let diff = compute(p, DiffBase::Turn, &ends, None).unwrap();
    assert_eq!(diff.from.label, "previous turn end");
    assert_eq!(diff.files.len(), 1);
    // A pruned tree is an error, not a wrong diff.
    let gone = vec![snap(1, SnapKind::Start, "0123456789012345678901234567890123456789"), snap(2, SnapKind::End, &t1)];
    assert!(compute(p, DiffBase::Turn, &gone, None).unwrap_err().to_string().contains("pruned"));
}

#[test]
fn revert_one_hunk_leaves_the_others_and_refuses_when_stale() {
    let d = repo();
    let p = d.path();
    let body: String = (1..=30).map(|i| format!("line {i}\n")).collect();
    std::fs::write(p.join("long.txt"), &body).unwrap();
    run(p, &["add", "long.txt"]).unwrap();
    run(p, &["commit", "-q", "-m", "long"]).unwrap();
    // The first hunk inserts a line, so reverting it shifts the second one up.
    let edited = body.replace("line 2\n", "line 2\ninserted\n").replace("line 25\n", "line 25\nadded\n");
    std::fs::write(p.join("long.txt"), &edited).unwrap();
    let diff = compute(p, DiffBase::Uncommitted, &[], None).unwrap();
    let f = file(&diff, "long.txt");
    assert_eq!(f.hunks.len(), 2);
    let (first, second) = (f.hunks[0].id.clone(), f.hunks[1].id.clone());

    let out = act(p, DiffBase::Uncommitted, &[], &first, HunkAction::Revert).unwrap();
    assert_eq!(out.file, "long.txt");
    let now = std::fs::read_to_string(p.join("long.txt")).unwrap();
    assert!(!now.contains("inserted") && now.contains("added\n"), "only the first hunk reverted:\n{now}");
    // The second hunk moved up a line and kept its id.
    let diff = compute(p, DiffBase::Uncommitted, &[], None).unwrap();
    assert_eq!(file(&diff, "long.txt").hunks.iter().map(|h| h.id.clone()).collect::<Vec<_>>(), vec![second.clone()]);

    // Reverting it again: gone from the diff → stale, nothing written.
    let err = act(p, DiffBase::Uncommitted, &[], &first, HunkAction::Revert).unwrap_err().to_string();
    assert!(err.starts_with("stale:"), "{err}");

    // A turn diff whose hunk the user has since edited no longer applies.
    let t0 = run(p, &["rev-parse", "HEAD^{tree}"]).unwrap();
    let t1 = snapshot_tree(p).unwrap();
    std::fs::write(p.join("long.txt"), now.replace("added\n", "changed by me\n")).unwrap();
    let snaps = vec![snap(1, SnapKind::Start, &t0), snap(2, SnapKind::End, &t1)];
    let before = std::fs::read(p.join("long.txt")).unwrap();
    let err = act(p, DiffBase::Turn, &snaps, &second, HunkAction::Revert).unwrap_err().to_string();
    assert!(err.starts_with("stale:"), "{err}");
    assert_eq!(std::fs::read(p.join("long.txt")).unwrap(), before, "a refused revert writes nothing");
}

#[test]
fn revert_an_added_file_deletes_it_and_a_deleted_file_comes_back() {
    let d = repo();
    let p = d.path();
    std::fs::write(p.join("new.txt"), "x\n").unwrap();
    std::fs::remove_file(p.join("a.txt")).unwrap();
    let diff = compute(p, DiffBase::Uncommitted, &[], None).unwrap();
    assert_eq!(file(&diff, "a.txt").status, FileStatus::Deleted);
    for f in ["new.txt", "a.txt"] {
        let id = file(&diff, f).hunks[0].id.clone();
        act(p, DiffBase::Uncommitted, &[], &id, HunkAction::Revert).unwrap();
    }
    assert!(!p.join("new.txt").exists());
    assert_eq!(std::fs::read_to_string(p.join("a.txt")).unwrap(), "one\ntwo\nthree\n");
    assert!(compute(p, DiffBase::Uncommitted, &[], None).unwrap().files.is_empty());
}

#[test]
fn stage_and_unstage_touch_only_the_index_and_one_hunk() {
    let d = repo();
    let p = d.path();
    let body: String = (1..=30).map(|i| format!("line {i}\n")).collect();
    std::fs::write(p.join("long.txt"), &body).unwrap();
    run(p, &["add", "long.txt"]).unwrap();
    run(p, &["commit", "-q", "-m", "long"]).unwrap();
    std::fs::write(p.join("long.txt"), body.replace("line 2\n", "line TWO\n").replace("line 25\n", "line XXV\n")).unwrap();
    let diff = compute(p, DiffBase::Uncommitted, &[], None).unwrap();
    let ids: Vec<String> = file(&diff, "long.txt").hunks.iter().map(|h| h.id.clone()).collect();
    let worktree_before = std::fs::read(p.join("long.txt")).unwrap();

    act(p, DiffBase::Uncommitted, &[], &ids[0], HunkAction::Stage).unwrap();
    assert_eq!(std::fs::read(p.join("long.txt")).unwrap(), worktree_before, "stage never touches the worktree");
    let cached = run(p, &["diff", "--cached"]).unwrap();
    assert!(cached.contains("+line TWO") && !cached.contains("XXV"), "{cached}");
    let diff = compute(p, DiffBase::Uncommitted, &[], None).unwrap();
    let hs = &file(&diff, "long.txt").hunks;
    assert!(hs[0].staged && !hs[1].staged, "the staged mark follows the index");

    act(p, DiffBase::Uncommitted, &[], &ids[0], HunkAction::Unstage).unwrap();
    assert_eq!(run(p, &["diff", "--cached"]).unwrap(), "");
    // Unstaging what isn't staged is stale.
    assert!(act(p, DiffBase::Uncommitted, &[], &ids[1], HunkAction::Unstage).unwrap_err().to_string().starts_with("stale:"));
    // Stage is an uncommitted-view action.
    assert!(act(p, DiffBase::Branch, &[], &ids[1], HunkAction::Stage).is_err());

    // An untracked file stages as a new file.
    std::fs::write(p.join("fresh.txt"), "f\n").unwrap();
    let diff = compute(p, DiffBase::Uncommitted, &[], None).unwrap();
    act(p, DiffBase::Uncommitted, &[], &file(&diff, "fresh.txt").hunks[0].id, HunkAction::Stage).unwrap();
    assert!(run(p, &["diff", "--cached", "--name-status"]).unwrap().contains("A\tfresh.txt"));
}

#[test]
fn noise_is_collapsed_and_fetched_by_path() {
    let d = repo();
    let p = d.path();
    std::fs::write(p.join("Cargo.lock"), "lock\n").unwrap();
    std::fs::write(p.join("b.rs"), "fn b() {}\n").unwrap();
    let diff = compute(p, DiffBase::Uncommitted, &[], None).unwrap();
    let lock = file(&diff, "Cargo.lock");
    assert_eq!(lock.noise, Some(Noise::Lockfile));
    assert!(lock.collapsed_by_default);
    assert_eq!(lock.hunks_omitted, Some(Omitted::Collapsed));
    assert!(lock.hunks.is_empty());
    assert_eq!((lock.added, lock.deleted), (1, 0), "counts survive collapsing");
    let one = compute(p, DiffBase::Uncommitted, &[], Some("Cargo.lock")).unwrap();
    assert_eq!(one.files.len(), 1);
    assert_eq!(one.files[0].hunks.len(), 1);
    assert!(one.files[0].hunks_omitted.is_none());
    assert!(compute(p, DiffBase::Uncommitted, &[], Some("nope.txt")).is_err());
}

#[test]
fn caps_are_explicit_never_silent() {
    let d = repo();
    let p = d.path();
    let huge: String = (0..MAX_HUNK_LINES + 50).map(|i| format!("l{i}\n")).collect();
    std::fs::write(p.join("big.txt"), &huge).unwrap();
    std::fs::write(p.join("wide.txt"), format!("{}\n", "w".repeat(MAX_LINE_CHARS + 10))).unwrap();
    // Asked for directly (so `large` noise doesn't collapse it).
    let one = compute(p, DiffBase::Uncommitted, &[], Some("big.txt")).unwrap();
    let f = &one.files[0];
    assert!(f.truncated && f.hunks[0].truncated);
    assert_eq!(f.hunks[0].lines.len(), MAX_HUNK_LINES);
    assert_eq!(f.added, u32::try_from(MAX_HUNK_LINES + 50).unwrap(), "counts are the real ones");
    let w = compute(p, DiffBase::Uncommitted, &[], Some("wide.txt")).unwrap();
    let l = &w.files[0].hunks[0].lines[0];
    assert!(l.truncated);
    assert_eq!(l.text.chars().count(), MAX_LINE_CHARS);
}

#[test]
fn budget_stubs_files_in_order() {
    let d = repo();
    let p = d.path();
    for i in 0..6 {
        let body: String = (0..400).map(|j| format!("file {i} line {j} with some text\n")).collect();
        std::fs::write(p.join(format!("f{i}.txt")), body).unwrap();
    }
    let (files, _) = raw(p, &resolve(p, DiffBase::Uncommitted, &[]).unwrap()).unwrap();
    let diff = build(&files, false, &HashSet::new(), None, 64 * 1024);
    let full: Vec<bool> = diff.files.iter().map(|f| f.hunks_omitted.is_none()).collect();
    assert!(full[0], "the first file fits");
    assert!(full.iter().any(|x| !x), "later files are stubs: {full:?}");
    for f in diff.files.iter().filter(|f| f.hunks_omitted.is_some()) {
        assert_eq!(f.hunks_omitted, Some(Omitted::Budget));
        assert!(f.hunks.is_empty());
        assert_eq!(f.added, 400);
    }
    assert!(serde_json::to_vec(&diff).unwrap().len() < 80 * 1024);
}

#[test]
fn review_message_quotes_each_location() {
    let d = repo();
    let p = d.path();
    std::fs::write(p.join("a.txt"), "one\nTWO\nthree\nfour\n").unwrap();
    let r = resolve(p, DiffBase::Uncommitted, &[]).unwrap();
    let (files, _) = raw(p, &r).unwrap();
    let id = files[0].hunk_ids()[0].clone();
    let comments = vec![
        ReviewComment {
            file: "a.txt".into(),
            hunk_id: Some(id),
            line_range: Some([2, 2]),
            side: None,
            text: "Why uppercase?".into(),
        },
        ReviewComment {
            file: "a.txt".into(),
            hunk_id: Some("deadbeefdeadbeef".into()),
            line_range: Some([4, 4]),
            side: None,
            text: "and this".into(),
        },
        ReviewComment {
            file: "a.txt".into(),
            hunk_id: None,
            line_range: None,
            side: None,
            text: "General\nsecond line".into(),
        },
    ];
    let m = review_message(DiffBase::Turn, &files, &comments);
    assert!(m.starts_with("Code review of your last turn from SmoothFlow — 3 comments."), "{m}");
    assert!(m.contains("1. a.txt:2\n   ```diff\n   -two\n   +TWO\n   ```\n   Why uppercase?"), "{m}");
    assert!(m.contains("2. a.txt:4\n   (that hunk has changed"), "{m}");
    assert!(m.contains("3. a.txt\n"), "{m}");
    assert!(m.contains("   General\n   second line"), "{m}");
}

#[test]
fn non_repo_is_an_error() {
    let d = tempfile::tempdir().unwrap();
    assert!(compute(d.path(), DiffBase::Uncommitted, &[], None).is_err());
}

#[test]
fn store_keeps_the_newest_snapshots() {
    let st = crate::store::FlowStore::open_in_memory().unwrap();
    for i in 0..(snapshot::KEEP_SNAPSHOTS + 5) {
        let kind = if i % 2 == 0 { SnapKind::Start } else { SnapKind::End };
        st.add_snapshot("fs-1", kind, &format!("t{i}"), snapshot::KEEP_SNAPSHOTS).unwrap();
    }
    let s = st.snapshots("fs-1").unwrap();
    assert_eq!(s.len(), snapshot::KEEP_SNAPSHOTS);
    assert_eq!(s.last().unwrap().tree, format!("t{}", snapshot::KEEP_SNAPSHOTS + 4));
    assert!(st.snapshots("fs-2").unwrap().is_empty());
}
