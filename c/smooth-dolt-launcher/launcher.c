/*
 * smooth-dolt-launcher — clean-slate exec wrapper for `smooth-dolt`.
 *
 * When `smooth-dolt serve` is spawned from a long-running Tokio
 * process (Big Smooth), the Go runtime in the child wedges on the
 * first SQL query — all goroutines park in pthread_cond_wait while
 * pings still answer. Same root cause as pearl `th-1a61a7`. Two
 * things the parent contaminates the child with:
 *
 *   1. Inherited file descriptors (Tokio's epoll/kqueue + eventfd
 *      pipes). Go's runtime grabs poll fds at startup and the
 *      stale-from-parent ones break netpoll.
 *   2. The pthread signal mask. Tokio installs blocking signal
 *      masks on its worker threads; Go's runtime needs SIGURG /
 *      preemption signals it can't deliver while masked.
 *
 * Closing fds and clearing the mask from inside Go is too late —
 * the runtime has already spun up its scheduler and grabbed
 * resources. From inside Rust, we'd need `pre_exec`, which is
 * `unsafe` (forbidden by workspace policy).
 *
 * This 30-line C program runs BEFORE Go and BEFORE Rust. It does:
 *
 *   - sigprocmask(SIG_SETMASK, &empty_mask): clear inherited mask
 *   - closefrom(3): close every fd > 2 except the ones the next
 *     program needs (we don't need any beyond stdio)
 *   - setsid(): new session, no controlling terminal
 *   - execv(argv[1], &argv[1]): replace ourselves with the real
 *     program. argv[1] is the binary; argv[2..] are its args.
 *
 * Usage:
 *   smooth-dolt-launcher /path/to/smooth-dolt serve <data-dir> --socket <path>
 *
 * Built by scripts/build-smooth-dolt-launcher.sh; ships alongside
 * smooth-dolt + th in target/release/.
 */

#define _DARWIN_C_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/resource.h>
#include <unistd.h>

static void close_inherited_fds(void) {
    /* Manual loop bounded by RLIMIT_NOFILE. On macOS, closefrom()
     * isn't reliably exposed across SDK versions, so we just use
     * the portable loop. The cost is a few hundred close() syscalls
     * (most returning EBADF) which adds < 1 ms. Worth it for
     * deterministic cleanup. */
    struct rlimit rl;
    int cap = 1024;
    if (getrlimit(RLIMIT_NOFILE, &rl) == 0 && rl.rlim_cur < (rlim_t)cap) {
        cap = (int)rl.rlim_cur;
    }
    for (int fd = 3; fd < cap; fd++) {
        (void)close(fd);
    }
}

int main(int argc, char **argv) {
    if (argc < 2) {
        fprintf(stderr, "usage: smooth-dolt-launcher <program> [args...]\n");
        return 2;
    }

    /* 1. Reset signal mask. Tokio installs SIGINT/SIGTERM/SIGUSR1
     *    blocks on its worker threads; the child fork inherits them.
     *    Go's runtime wedges if it can't receive SIGURG (used for
     *    goroutine preemption). Empty mask = unblock everything. */
    sigset_t empty;
    sigemptyset(&empty);
    if (sigprocmask(SIG_SETMASK, &empty, NULL) != 0) {
        fprintf(stderr, "smooth-dolt-launcher: sigprocmask: %s\n", strerror(errno));
        /* Non-fatal — try to keep going; the child may survive
         * partial inheritance. */
    }

    /* Reset all signal HANDLERS to default. The parent may have
     * installed handlers for SIGTERM etc. that we don't want. */
    for (int sig = 1; sig < NSIG; sig++) {
        struct sigaction sa;
        memset(&sa, 0, sizeof(sa));
        sa.sa_handler = SIG_DFL;
        sigemptyset(&sa.sa_mask);
        (void)sigaction(sig, &sa, NULL);
    }

    /* 2. Close inherited fds. Tokio epoll/kqueue + eventfd pipes
     *    have to go before Go's runtime claims poll resources. */
    close_inherited_fds();

    /* 3. New session, no controlling terminal. setsid fails if we're
     *    already the session leader (rare for a child) — non-fatal. */
    (void)setsid();

    /* 4. Replace ourselves with the requested program. argv[0] for
     *    the executed program is argv[1] of this launcher. */
    execv(argv[1], &argv[1]);

    /* execv only returns on failure. */
    fprintf(stderr, "smooth-dolt-launcher: execv %s: %s\n", argv[1], strerror(errno));
    return 127;
}
