// Raw C++ baseline for tests/jeti_foundation_bfs_benchmark.jeti.
// Deliberately uses no STL: same flat buffers, maze formula, modes, and
// timed build+clear+BFS region as the Jeti raw benchmark.

#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <time.h>

#define BFS_MAX_CELLS 262144

static int g_rows = 256;
static int g_cols = 256;
static uint8_t g_maze[BFS_MAX_CELLS];
static uint8_t g_seen[BFS_MAX_CELLS];
static int g_queue[BFS_MAX_CELLS];
static int g_distance[BFS_MAX_CELLS];

static unsigned int maze_noise(unsigned int r, unsigned int c, unsigned int seed) {
    unsigned int x = r * 1103515245u + c * 12345u + seed * 2654435761u;
    x ^= x >> 16;
    x *= 2246822519u;
    return x ^ (x >> 13);
}

static void build_maze(int rows, int cols, unsigned int seed) {
    g_rows = rows;
    g_cols = cols;
    for (int r = 0; r < g_rows; ++r) {
        for (int c = 0; c < g_cols; ++c) {
            int cell = r * g_cols + c;
            g_maze[cell] = (maze_noise((unsigned int)r, (unsigned int)c, seed) % 100u) < 30u;
            g_seen[cell] = 0;
        }
    }
    for (int c = 0; c < g_cols; ++c) g_maze[c] = 0;
    for (int r = 0; r < g_rows; ++r) g_maze[r * g_cols + g_cols - 1] = 0;
    g_maze[0] = 0;
    g_maze[(g_rows - 1) * g_cols + g_cols - 1] = 0;
}

static int shortest_path(int *visited) {
    int cells = g_rows * g_cols;
    int goal = (g_rows - 1) * g_cols + (g_cols - 1);
    for (int i = 0; i < cells; ++i) g_distance[i] = -1;

    int head = 0;
    int tail = 0;
    g_queue[tail++] = 0;
    g_seen[0] = 1;
    g_distance[0] = 0;
    *visited = 0;

    static const int dr[4] = {-1, 1, 0, 0};
    static const int dc[4] = {0, 0, -1, 1};

    while (head < tail) {
        int cell = g_queue[head++];
        int row = cell / g_cols;
        int col = cell % g_cols;
        ++*visited;
        if (cell == goal) return g_distance[cell];

        for (int i = 0; i < 4; ++i) {
            int nr = row + dr[i];
            int nc = col + dc[i];
            if (nr < 0 || nr >= g_rows || nc < 0 || nc >= g_cols) continue;
            int neighbor = nr * g_cols + nc;
            if (g_maze[neighbor] || g_seen[neighbor]) continue;
            g_seen[neighbor] = 1;
            g_queue[tail++] = neighbor;
            g_distance[neighbor] = g_distance[cell] + 1;
        }
    }
    return -1;
}

static double monotonic_ms() {
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    return (double)now.tv_sec * 1000.0 + (double)now.tv_nsec / 1000000.0;
}

int main(int argc, char **argv) {
    int rows = 256;
    int cols = 256;
    int boards = 1;
    unsigned int seed = 7u;
    const char *mode = "advanced";

    if (argc > 1 && strcmp(argv[1], "medium") == 0) {
        rows = 128;
        cols = 128;
        seed = 3u;
        mode = "medium";
    } else if (argc > 1 && strcmp(argv[1], "random1000") == 0) {
        rows = 64;
        cols = 64;
        boards = 1000;
        seed = 100u;
        mode = "random1000";
    } else if (argc > 1 && strcmp(argv[1], "advanced") != 0) {
        printf("usage: %s [medium|advanced|random1000]\n", argv[0]);
        return 2;
    }

    build_maze(rows, cols, seed);
    int visited = 0;
    (void)shortest_path(&visited);

    long long total_visited = 0;
    long long total_distance = 0;
    double before = monotonic_ms();
    for (int board = 0; board < boards; ++board) {
        build_maze(rows, cols, seed + (unsigned int)board * 101u);
        int distance = shortest_path(&visited);
        total_distance += distance;
        total_visited += visited;
    }
    double after = monotonic_ms();

    printf("C++ raw BFS\n");
    printf("mode=%s\n", mode);
    printf("mazes=%d size=%dx%d walls≈30%%\n", boards, rows, cols);
    printf("average path=%.1f steps\n", (double)total_distance / (double)boards);
    printf("average visited=%.1f cells\n", (double)total_visited / (double)boards);
    printf("elapsed=%.3f ms\n", after - before);
    printf("average=%.3f ms\n", (after - before) / (double)boards);
    return 0;
}
