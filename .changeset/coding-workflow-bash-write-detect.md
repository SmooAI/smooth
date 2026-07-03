---
'@smooai/smooth': patch
---

Fix coding_workflow mislabelling real code turns as THINK mode.

Two related bugs in the operative's single-agent coding loop, both of which made
dispatched code tasks bail without finishing:

1. **Bash writes now count as edits (pearl th-a03c53).** The "no edits" detector
   only tracked named edit tools (`edit_file`, `write_file`, …), so an agent that
   wrote a file via bash — a heredoc (`cat > f <<EOF`), `python3 -c "open(...,'w')"`,
   `tee`, `sed -i`, a `>`/`>>` redirect — looked like it did nothing. The
   affine-cipher bench run exited "no edits" while the file had grown from a 6-line
   stub to a 70-line impl. `conversation_made_edits` now also scans `bash`/`shell`
   tool-calls for write shapes (`bash_command_writes_files`), skipping fd-dups
   (`2>&1`) and null sinks (`>/dev/null`) so read-only exploration still counts as
   no edit.

2. **THINK-mode exit no longer fires on iteration 1 (pearl th-fc8a51).** A real
   code task that whiffed its first turn with 0 edits (cpp/bank-account: 23s, FAIL —
   the same task solved 17/17 on a focused rerun) was bailed as if it were a chat
   question. The no-edit → THINK decision is now a pure, table-tested function
   (`decide_no_edit`) on its own retry budget: the earliest a no-edit turn can exit
   as THINK is iteration 2, and it no longer steals the no-test retry a later coding
   turn needs.
