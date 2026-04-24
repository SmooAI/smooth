# Smooth — The Line History

This file is auto-maintained by `.github/workflows/the-line.yml`. Do not edit
by hand — the release workflow appends a row per tagged version.

Each row is the output of a `smooth-bench score --release` sweep published
alongside the corresponding GitHub Release. "overall%" is the single-number
"The Line" Smoo AI publishes; the per-language columns are the 20-task
sub-sweeps. `commit_sha` is the first 12 chars of the release commit.

A drop of more than 2 percentage points in `overall%` from the previous row
fails the workflow — a human has to investigate before the release goes
public.

| version | date | overall% | cpp% | go% | java% | javascript% | python% | rust% | cost_usd | commit_sha |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
