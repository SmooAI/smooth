---
'@smooai/smooth': patch
---

Big Smooth knows which Smoo org it is signed in as, and tool calls collapse by default.

The daemon folds its signed-in Smoo AI identity (user + active org id) into the
persona, alongside the skills index. It previously had no ambient knowledge of
its own identity and answered "I'm not logged in yet, you'd need to run
`th auth login`" while holding a valid session.

Tool calls in the web UI now render collapsed to a one-line header, with output
behind a disclosure. Failed calls open themselves, so a silent failure does not
read as a success.
