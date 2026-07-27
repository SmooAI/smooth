---
'@smooai/smooth': patch
---

Fix the intermittently-red Release workflow: the file-tree walker's sort comparator called `Path::is_dir()` on every comparison, which is a live filesystem stat. When the walked tree changed mid-sort — parallel tests creating and dropping temp dirs under `/tmp` — a path could answer "directory" for one comparison and "file" for the next, making the ordering non-transitive and tripping Rust's `slice::sort` "comparison function does not correctly implement a total order" panic. The comparator now memoizes each path's verdict for the duration of the walk, so it stays a total order no matter what happens on disk underneath it.
