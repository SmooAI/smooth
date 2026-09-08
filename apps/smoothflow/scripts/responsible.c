#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
int main(int argc, char **argv) {
    pid_t (*f)(pid_t) = dlsym(RTLD_DEFAULT, "responsibility_get_pid_responsible_for_pid");
    if (!f) { puts("nosym"); return 1; }
    pid_t p = argc > 1 ? atoi(argv[1]) : getppid();
    printf("%d\n", f(p));
    return 0;
}
