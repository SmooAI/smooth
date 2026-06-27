//! Animated boot indicator for `th` cold-start and `th up`.
//!
//! Pearl th-7840d8 — replaces the bare `println!("Starting Smooth...")`
//! cold-boot line (and the matching path inside `th up`) with a
//! per-step spinner cascade so the user can see what's happening
//! while the Safehouse microVM and in-VM cast services come up.
//!
//! Visuals:
//!
//! ```text
//! ✻ Smooth booting
//!   ⠋ starting Safehouse microVM…
//!   ⠋ cast online (wonk · goalie · narc · scribe · archivist · diver · groove)…
//!   ⠋ operative pool warm…
//!   ⠋ health check…
//! ```
//!
//! On success each line flips to a green `✓ <label>` and stays in the
//! transcript. On timeout / failure it flips to a red `✗ <label> —
//! <reason>`. There's no final summary line — the steps themselves
//! are the receipt.
//!
//! Each [`BootStep`] is one `indicatif::ProgressBar` mounted on a
//! shared [`indicatif::MultiProgress`]. The spinner template is
//! deliberately spelled out (rather than using indicatif's built-in
//! `unicode_spinner` style key) so the ANSI palette is locked in
//! regardless of where indicatif's defaults move next.
//!
//! The state machine — `Pending → Running → Done(Ok|Fail)` — is
//! tested in `mod tests` without touching a real terminal.

use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use owo_colors::OwoColorize;
use std::time::Duration;

use crate::gradient;

/// Tick interval for the spinner animation. 80ms matches what the
/// rest of the Smooth CLI uses (see e.g. `smooth-code`'s status spinner).
const SPINNER_TICK_MS: u64 = 80;

/// Spinner glyphs — indicatif's canonical Braille cycle. Spelled out
/// (rather than via `unicode_spinner`) so we don't lock ourselves to
/// upstream defaults.
const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Multi-step boot indicator. Holds a shared `MultiProgress` and
/// hands out [`BootStep`] handles via [`BootIndicator::step`].
///
/// The header line — `✻ Smooth booting` in cyan/bold — is printed
/// once on construction; per-step lines are printed by indicatif as
/// the spinners tick.
pub struct BootIndicator {
    mp: MultiProgress,
}

impl BootIndicator {
    /// Mount a `MultiProgress`, print the header, and return the
    /// handle. The caller is expected to call [`Self::step`] one or
    /// more times, then [`Self::finish`] (or drop the value, which
    /// has the same effect).
    #[must_use]
    pub fn new() -> Self {
        // Header — print directly (not as a ProgressBar) so it
        // doesn't get cleared on Drop and so it always lands above
        // the first spinner.
        println!();
        println!("  {} {} {}", "✻".cyan().bold(), gradient::smooth(), "booting".bold());
        println!("  {}", gradient::flow_rule(32, '─'));
        Self { mp: MultiProgress::new() }
    }

    /// Start a new step with `label`. Returns a [`BootStep`] handle
    /// the caller must finalize via [`BootStep::ok`] or
    /// [`BootStep::fail`].
    ///
    /// Steps render in the order they're created.
    pub fn step(&self, label: &str) -> BootStep {
        // Template breakdown:
        //   "    {spinner:.cyan} {msg}"
        //
        // `:.cyan` styles the spinner glyph; the message stays in
        // the default terminal color so it doesn't compete with the
        // ✓ / ✗ that replaces the spinner on completion.
        let style = ProgressStyle::with_template("    {spinner:.cyan} {msg}")
            .expect("static template parses")
            .tick_strings(SPINNER_FRAMES);

        let pb = self.mp.add(ProgressBar::new_spinner());
        pb.set_style(style);
        pb.set_message(format!("{label}…"));
        pb.enable_steady_tick(Duration::from_millis(SPINNER_TICK_MS));

        BootStep {
            bar: pb,
            label: label.to_string(),
        }
    }

    /// Drain the `MultiProgress` and leave all finalized lines in
    /// the terminal transcript. Equivalent to dropping `self`.
    pub fn finish(self) {
        // MultiProgress's Drop already flushes & releases stdout.
        // We provide `finish()` as the documented terminator so
        // callers don't have to know that.
        drop(self);
    }
}

impl Default for BootIndicator {
    fn default() -> Self {
        Self::new()
    }
}

/// Handle for one boot step. Must be finalized via [`Self::ok`] or
/// [`Self::fail`] — dropping without finalizing leaves a still-
/// spinning line, which indicatif then clears on `MultiProgress`
/// drop.
pub struct BootStep {
    bar: ProgressBar,
    label: String,
}

impl BootStep {
    /// Mark the step done. Replaces the spinner with a green `✓`
    /// and leaves the label in the transcript.
    pub fn ok(self) {
        // `finish_with_message` writes the final frame using the
        // current style — but we want to *replace* the spinner
        // glyph with a static green ✓, so we switch templates first.
        let style = ProgressStyle::with_template("    {msg}").expect("static template parses");
        self.bar.set_style(style);
        self.bar.finish_with_message(format!("{} {}", "✓".green().bold(), self.label));
    }

    /// Mark the step failed with `reason`. Replaces the spinner with
    /// a red `✗` and appends ` — <reason>` to the label.
    pub fn fail(self, reason: &str) {
        let style = ProgressStyle::with_template("    {msg}").expect("static template parses");
        self.bar.set_style(style);
        self.bar
            .finish_with_message(format!("{} {} {} {}", "✗".red().bold(), self.label.red(), "—".dimmed(), reason.red()));
    }

    /// Update the step's label in-place. Used when a step has
    /// sub-progress information worth showing (e.g. swap "health
    /// check" → "health check (3/4 services up)").
    #[allow(dead_code)] // exposed for callers that want incremental updates
    pub fn update(&self, label: &str) {
        self.bar.set_message(format!("{label}…"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke-test: creating + finalizing a step doesn't panic and
    /// doesn't leak a still-running spinner thread.
    #[test]
    fn step_ok_completes_cleanly() {
        let ind = BootIndicator::new();
        let step = ind.step("starting Safehouse microVM");
        // No real work — just check the lifecycle.
        step.ok();
        ind.finish();
    }

    #[test]
    fn step_fail_completes_cleanly() {
        let ind = BootIndicator::new();
        let step = ind.step("health check");
        step.fail("timeout after 30s");
        ind.finish();
    }

    /// Multiple steps in flight at once — exercise the
    /// MultiProgress codepath.
    #[test]
    fn multiple_steps_finalize_in_arbitrary_order() {
        let ind = BootIndicator::new();
        let s1 = ind.step("starting Safehouse microVM");
        let s2 = ind.step("cast online (wonk · goalie · narc · scribe · archivist · diver · groove)");
        let s3 = ind.step("operative pool warm");
        let s4 = ind.step("health check");

        // Finalize out of order to confirm the API doesn't require
        // a fixed sequence.
        s2.ok();
        s4.fail("timeout");
        s1.ok();
        s3.ok();

        ind.finish();
    }

    /// `update` must not panic on an in-flight step.
    #[test]
    fn step_update_is_safe() {
        let ind = BootIndicator::new();
        let step = ind.step("cast online");
        step.update("cast online (3/7 services up)");
        step.ok();
        ind.finish();
    }

    /// Indicator must be safe to drop without explicit finish().
    #[test]
    fn indicator_drops_without_finish_call() {
        let ind = BootIndicator::new();
        let step = ind.step("starting Safehouse microVM");
        step.ok();
        // No ind.finish() — Drop should handle it.
    }
}
