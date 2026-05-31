//! Test harness for the cross-process file-lock regression test on
//! [`smooth_pearls::registry::auto_register_at`] — pearl `th-9799fa`.
//!
//! Invoked by `auto_register_at_serializes_cross_process_writers` in
//! `registry.rs`. Reads `(registry_file, project_root)` from argv and
//! calls `auto_register_at` once. The parent test forks N copies and
//! asserts all N entries survive.

fn main() {
    let mut args = std::env::args().skip(1);
    let registry_file = args.next().expect("usage: <registry_file> <project_root>");
    let project_root = args.next().expect("usage: <registry_file> <project_root>");
    let registry_file = std::path::PathBuf::from(registry_file);
    let project_root = std::path::PathBuf::from(project_root);
    std::fs::create_dir_all(&project_root).expect("create project root");
    smooth_pearls::registry::auto_register_at(&project_root, &registry_file).expect("auto_register_at");
}
