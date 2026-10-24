/* pty_run.c - Ejecuta un comando en un PTY real, capturando stdout/stderr.
 * Compilar: cc -o pty_run pty_run.c
 * Uso: ./pty_run <comando> [args...]
 */
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
#include <fcntl.h>
#include <pty.h>
#include <sys/wait.h>
#include <signal.h>
#include <string.h>
#include <errno.h>

int main(int argc, char** argv) {
    if (argc < 2) {
        fprintf(stderr, "Uso: %s <comando> [args...]\n", argv[0]);
        return 1;
    }

    int master_fd;
    pid_t pid = forkpty(&master_fd, NULL, NULL, NULL);
    if (pid < 0) {
        perror("forkpty");
        return 1;
    }

    if (pid == 0) {
        // Hijo: ejecutar el comando
        execvp(argv[1], &argv[1]);
        perror("execvp");
        _exit(127);
    }

    // Padre: leer del master y escribir en stdout
    char buf[4096];
    ssize_t n;
    while ((n = read(master_fd, buf, sizeof buf)) > 0) {
        if (write(STDOUT_FILENO, buf, n) != n) break;
    }

    int status;
    waitpid(pid, &status, 0);
    return WIFEXITED(status) ? WEXITSTATUS(status) : 1;
}