#!/bin/bash
# Notarize + staple a Big Smooth artifact (pearl th-a647da).
#
# Submits to Apple's notary service (`xcrun notarytool submit --wait`) and
# staples the resulting ticket (`xcrun stapler staple`) so the artifact passes
# Gatekeeper offline. Accepts one or more artifacts: a `.dmg`, a `.pkg`, or a
# `.app` (a bare .app can't be submitted directly, so it's zipped for the
# submission and stapled in place afterwards).
#
# Prerequisite: the artifact must ALREADY be signed with a real Developer ID
# identity and hardened runtime (`SIGN_IDENTITY=… make-app-bundle.sh`) — Apple
# rejects ad-hoc signatures and non-hardened binaries.
#
# Credentials (either set, from the environment):
#   App Store Connect API key (preferred — no 2FA app-specific password):
#     NOTARY_KEY      path to the AuthKey_XXXX.p8 file
#     NOTARY_KEY_ID   the key ID
#     NOTARY_ISSUER   the issuer UUID
#   Apple ID:
#     NOTARY_APPLE_ID, NOTARY_TEAM_ID, NOTARY_PASSWORD (app-specific password)
#
# With NO credentials set this is a deliberate NO-OP: it prints what's missing
# and exits 0, so local builds and unsigned CI runs still succeed.
#
# Usage:
#   scripts/macos/notarize-and-staple.sh dist/BigSmooth.dmg
#   scripts/macos/notarize-and-staple.sh "build/Big Smooth.app" dist/BigSmooth.dmg

set -euo pipefail

[ "$#" -gt 0 ] || { echo "usage: notarize-and-staple.sh <artifact> [artifact…]" >&2; exit 2; }

if [ -n "${NOTARY_KEY:-}" ] && [ -n "${NOTARY_KEY_ID:-}" ] && [ -n "${NOTARY_ISSUER:-}" ]; then
    CREDS=(--key "$NOTARY_KEY" --key-id "$NOTARY_KEY_ID" --issuer "$NOTARY_ISSUER")
elif [ -n "${NOTARY_APPLE_ID:-}" ] && [ -n "${NOTARY_TEAM_ID:-}" ] && [ -n "${NOTARY_PASSWORD:-}" ]; then
    CREDS=(--apple-id "$NOTARY_APPLE_ID" --team-id "$NOTARY_TEAM_ID" --password "$NOTARY_PASSWORD")
else
    cat >&2 <<'SKIP'
notarization skipped — no notary credentials in the environment.
  Set either
    NOTARY_KEY (path to AuthKey_XXXX.p8) + NOTARY_KEY_ID + NOTARY_ISSUER
  or
    NOTARY_APPLE_ID + NOTARY_TEAM_ID + NOTARY_PASSWORD (app-specific password)
  and sign with a real Developer ID cert (SIGN_IDENTITY=...) first.
  The artifact is still usable locally; other Macs will see a Gatekeeper warning.
SKIP
    exit 0
fi

say() { printf '\n\033[1;36m==> %s\033[0m\n' "$1" >&2; }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

for ARTIFACT in "$@"; do
    [ -e "$ARTIFACT" ] || { echo "error: artifact not found: $ARTIFACT" >&2; exit 1; }

    SUBMIT="$ARTIFACT"
    if [ -d "$ARTIFACT" ]; then
        # notarytool takes archives, not directories. `ditto -c -k --keepParent`
        # is Apple's prescribed zip (preserves the bundle + its signature).
        SUBMIT="$TMP/$(basename "$ARTIFACT").zip"
        ditto -c -k --keepParent "$ARTIFACT" "$SUBMIT"
    fi

    say "Notarizing $(basename "$ARTIFACT")"
    # Deliberately NOT `submit --wait`. That collapses "upload" and "poll for
    # the result" into one call, so a network blip during the poll fails the
    # whole step and throws away a submission Apple is already processing —
    # which is exactly what happened on the first 0.2.3 attempt: the upload
    # succeeded, the status poll timed out (NSURLErrorDomain -1001), and the
    # release died holding a perfectly good submission id. Submit once, then
    # retry the WAIT against that id. Never resubmit on a poll failure.
    SUBMIT_OUT="$(xcrun notarytool submit "$SUBMIT" "${CREDS[@]}" 2>&1)" || { printf '%s\n' "$SUBMIT_OUT" >&2; exit 1; }
    printf '%s\n' "$SUBMIT_OUT" >&2
    SUB_ID="$(printf '%s\n' "$SUBMIT_OUT" | awk '/^ *id: /{print $2; exit}')"
    [ -n "$SUB_ID" ] || { echo "error: could not parse a submission id from notarytool" >&2; exit 1; }

    WAITED=0
    for attempt in 1 2 3 4 5; do
        if xcrun notarytool wait "$SUB_ID" "${CREDS[@]}" --timeout 30m >&2; then
            WAITED=1
            break
        fi
        echo "notarytool wait failed (attempt $attempt/5) — the submission is still queued at Apple; retrying" >&2
        sleep 30
    done
    [ "$WAITED" = 1 ] || { echo "error: gave up waiting on submission $SUB_ID" >&2; exit 1; }

    # `wait` returning 0 means Apple finished, not that it approved.
    STATUS="$(xcrun notarytool info "$SUB_ID" "${CREDS[@]}" 2>&1 | awk '/^ *status: /{sub(/^ *status: */,""); print; exit}')"
    echo "notarization status: ${STATUS:-unknown}" >&2
    if [ "$STATUS" != "Accepted" ]; then
        echo "error: notarization was not accepted (status: ${STATUS:-unknown}); log follows" >&2
        xcrun notarytool log "$SUB_ID" "${CREDS[@]}" >&2 || true
        exit 1
    fi

    # Staple the ORIGINAL (the ticket belongs on the .app/.dmg, not the zip).
    say "Stapling $(basename "$ARTIFACT")"
    xcrun stapler staple "$ARTIFACT" >&2
    xcrun stapler validate "$ARTIFACT" >&2
done
