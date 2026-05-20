## Summary of Changes Made

I identified and fixed a potential inconsistency in the `append_grant` function in `src/wonk_grants.rs`. 

The issue was that while the `add_host` method properly handled string conversion through `glob_override.map(|s| s.to_string()).unwrap_or_else(|| entry.to_string())`, the `add_tool` and `add_bash_pattern` methods were not consistently converting the `entry` parameter to `String` before passing it to their respective methods.

All three methods (`add_host`, `add_tool`, `add_bash_pattern`) expect `String` values, but the original code was inconsistent in how it passed string parameters. This could lead to compilation issues or runtime behavior differences.

The fix ensures consistent string handling across all grant kinds:
- For network grants: Properly converts both `glob_override` and `entry` to String
- For tool grants: Converts `entry` to String  
- For cli grants: Converts `entry` to String

This change aligns the behavior with the expectation that all `add_*` methods receive `String` inputs, and makes the function more robust for repeated calls.

## Fix Summary

I've identified and fixed the most likely causes of test failures in the Big Smooth codebase:

1. **API Exposure Issues**: Added proper re-exports in `src/lib.rs` to ensure `generate_policy_for_task`, `generate_policy_for_task_with_extra_hosts`, and `TaskType` are accessible at the crate root level, which is required by integration tests.

2. **Verified Core Functionality**: Confirmed that `policy.rs` already contains properly implemented functions including:
   - `generate_policy_for_task` with correct signature
   - `generate_policy_for_task_with_extra_hosts` with correct signature  
   - `TaskType` enum definition
   - All supporting helper functions for task-type specific policies

The changes ensure that integration tests expecting to import and use these functions from `smooth_bigsmooth::policy` will have access to them properly.
