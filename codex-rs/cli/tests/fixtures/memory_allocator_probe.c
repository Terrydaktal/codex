#define _GNU_SOURCE
#include <dirent.h>
#include <dlfcn.h>
#include <fcntl.h>
#include <malloc.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>

/* Record only allocator control metadata, never allocations or user contents. */
int mallopt(int parameter, int value) {
    const char *result = getenv("CODEX_TEST_ALLOCATOR_ORDER_RESULT");
    if (parameter == M_MMAP_THRESHOLD && result != NULL) {
        int threads = 0;
        DIR *tasks = opendir("/proc/self/task");
        if (tasks != NULL) {
            struct dirent *entry;
            while ((entry = readdir(tasks)) != NULL) {
                if (entry->d_name[0] >= '0' && entry->d_name[0] <= '9') {
                    ++threads;
                }
            }
            closedir(tasks);
        }
        int fd = open(result, O_WRONLY | O_CREAT | O_APPEND | O_CLOEXEC, 0600);
        if (fd >= 0) {
            char line[80];
            int length = snprintf(line, sizeof(line), "threads=%d threshold=%d\n", threads, value);
            if (length > 0 && (size_t)length < sizeof(line)) {
                (void)write(fd, line, (size_t)length);
            }
            close(fd);
        }
    }
    int (*real_mallopt)(int, int) = dlsym(RTLD_NEXT, "mallopt");
    return real_mallopt != NULL ? real_mallopt(parameter, value) : 0;
}
