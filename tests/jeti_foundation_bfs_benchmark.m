/*
 * Objective-C mirror of tests/jeti_foundation_bfs_benchmark.jeti.
 *
 * Same maze, same noise formula, same BFS, same modes, same seeds and the same
 * timed region — the only differences are the spelling and the container class
 * (NSMutableArray<NSNumber *> where Jeti uses NPMutableArray<NPNumber *>).
 * The deterministic output lines must match the Jeti build exactly; only the
 * title and the elapsed/average lines differ.
 *
 *   clang -O2 -fobjc-arc -framework Foundation \
 *       tests/jeti_foundation_bfs_benchmark.m -o /tmp/objc_bfs
 *   /tmp/objc_bfs medium | advanced | random1000
 *   /tmp/objc_bfs rawmedium | rawadvanced | raw1000
 */

#import <Foundation/Foundation.h>

#define _POSIX_C_SOURCE 200809L
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <time.h>

#define BFS_MAX_CELLS 262144

static int g_rows = 256;
static int g_cols = 256;
static uint8_t g_maze[BFS_MAX_CELLS];
static uint8_t g_seen[BFS_MAX_CELLS];
static int g_raw_queue[BFS_MAX_CELLS];
static int g_raw_distance[BFS_MAX_CELLS];

static unsigned int maze_noise(unsigned int r, unsigned int c, unsigned int seed) {
    unsigned int x = r * 1103515245u + c * 12345u + seed * 2654435761u;
    x ^= x >> 16;
    x *= 2246822519u;
    return x ^ (x >> 13);
}

static void build_maze(int rows, int cols, unsigned int seed) {
    g_rows = rows;
    g_cols = cols;
    for (int r = 0; r < g_rows; r++) {
        for (int c = 0; c < g_cols; c++) {
            /* About 30% walls, with a guaranteed path along the top/right. */
            int cell = r * g_cols + c;
            g_maze[cell] = (maze_noise((unsigned int)r, (unsigned int)c, seed) % 100u) < 30u;
            g_seen[cell] = 0;
        }
    }

    for (int c = 0; c < g_cols; c++) g_maze[c] = 0;
    for (int r = 0; r < g_rows; r++) g_maze[r * g_cols + g_cols - 1] = 0;
    g_maze[0] = 0;
    g_maze[(g_rows - 1) * g_cols + g_cols - 1] = 0;
}

@interface MazeSolver : NSObject
- (int)shortestPath;
- (int)shortestPathRaw;
- (int)visitedCells;
@end

@implementation MazeSolver {
    int _visited;
}

- (int)shortestPath {
    /* Foundation is the queue: every cell is boxed as an NSNumber object. */
    NSMutableArray<NSNumber *> *queue =
        [NSMutableArray arrayWithCapacity:(NSUInteger)(g_rows * g_cols / 4 + 4)];
    int start = 0;
    int goal = (g_rows - 1) * g_cols + (g_cols - 1);
    int distance[BFS_MAX_CELLS];

    for (int i = 0; i < g_rows * g_cols; i++) distance[i] = -1;
    [queue addObject:[NSNumber numberWithInt:start]];
    g_seen[0] = 1;
    distance[start] = 0;
    _visited = 0;

    NSUInteger head = 0;
    while (head < [queue count]) {
        NSNumber *boxed = [queue objectAtIndex:head++];
        int cell = [boxed intValue];
        int row = cell / g_cols;
        int col = cell % g_cols;
        _visited++;

        if (cell == goal) return distance[cell];

        int nr[4] = { row - 1, row + 1, row, row };
        int nc[4] = { col, col, col - 1, col + 1 };
        for (int i = 0; i < 4; i++) {
            if (nr[i] < 0 || nr[i] >= g_rows || nc[i] < 0 || nc[i] >= g_cols) continue;
            int neighbor = nr[i] * g_cols + nc[i];
            if (g_maze[neighbor] || g_seen[neighbor]) continue;

            g_seen[neighbor] = 1;
            int next = neighbor;
            distance[next] = distance[cell] + 1;
            [queue addObject:[NSNumber numberWithInt:next]];
        }
    }
    return -1;
}

- (int)shortestPathRaw {
    /* Same BFS, but the hot path is only primitive C storage. */
    int cells = g_rows * g_cols;
    int start = 0;
    int goal = (g_rows - 1) * g_cols + (g_cols - 1);
    for (int i = 0; i < cells; i++) g_raw_distance[i] = -1;

    int head = 0;
    int tail = 0;
    g_raw_queue[tail++] = start;
    g_seen[0] = 1;
    g_raw_distance[start] = 0;
    _visited = 0;

    while (head < tail) {
        int cell = g_raw_queue[head++];
        int row = cell / g_cols;
        int col = cell % g_cols;
        _visited++;
        if (cell == goal) return g_raw_distance[cell];

        int nr[4] = { row - 1, row + 1, row, row };
        int nc[4] = { col, col, col - 1, col + 1 };
        for (int i = 0; i < 4; i++) {
            if (nr[i] < 0 || nr[i] >= g_rows || nc[i] < 0 || nc[i] >= g_cols) continue;
            int neighbor = nr[i] * g_cols + nc[i];
            if (g_maze[neighbor] || g_seen[neighbor]) continue;
            g_seen[neighbor] = 1;
            g_raw_queue[tail++] = neighbor;
            g_raw_distance[neighbor] = g_raw_distance[cell] + 1;
        }
    }
    return -1;
}

- (int)visitedCells {
    return _visited;
}

@end

/* CLOCK_MONOTONIC is 6 on this platform; the Jeti benchmark calls it the same
 * way (a macro constant is not visible to its checker). */
static double monotonic_ms(void) {
    struct timespec now;
    clock_gettime(6, &now);
    return (double)now.tv_sec * 1000.0 + (double)now.tv_nsec / 1000000.0;
}

int main(int argc, char **argv) {
    @autoreleasepool {
        int rows = 256;
        int cols = 256;
        int boards = 1;
        unsigned int seed = 7u;
        int raw = 0;
        NSString *mode = @"advanced";

        if (argc > 1 && strcmp(argv[1], "medium") == 0) {
            rows = 128;
            cols = 128;
            seed = 3u;
            mode = [NSString stringWithUTF8String:"medium"];
        } else if (argc > 1 && strcmp(argv[1], "random1000") == 0) {
            rows = 64;
            cols = 64;
            boards = 1000;
            seed = 100u;
            mode = [NSString stringWithUTF8String:"random1000"];
        } else if (argc > 1 && strcmp(argv[1], "rawmedium") == 0) {
            rows = 128;
            cols = 128;
            seed = 3u;
            raw = 1;
            mode = [NSString stringWithUTF8String:"rawmedium"];
        } else if (argc > 1 && strcmp(argv[1], "rawadvanced") == 0) {
            raw = 1;
            mode = [NSString stringWithUTF8String:"rawadvanced"];
        } else if (argc > 1 && strcmp(argv[1], "raw1000") == 0) {
            rows = 64;
            cols = 64;
            boards = 1000;
            seed = 100u;
            raw = 1;
            mode = [NSString stringWithUTF8String:"raw1000"];
        } else if (argc > 1 && strcmp(argv[1], "advanced") != 0) {
            printf("usage: %s [medium|advanced|random1000|rawmedium|rawadvanced|raw1000]\n", argv[0]);
            return 2;
        }

        build_maze(rows, cols, seed);
        MazeSolver *solver = [[MazeSolver alloc] init];

        /* Warm up once so the reported sample is not dominated by first-use setup. */
        (void)[solver shortestPath];
        long long total_visited = 0;
        long long total_distance = 0;
        double before = monotonic_ms();
        for (int board = 0; board < boards; board++) {
            build_maze(rows, cols, seed + (unsigned int)board * 101u);
            int distance = raw ? [solver shortestPathRaw] : [solver shortestPath];
            total_distance += distance;
            total_visited += [solver visitedCells];
        }
        double after = monotonic_ms();

        NSString *title = @"Objective-C Foundation BFS";
        printf("%s\n", [title UTF8String]);
        printf("mode=%s\n", [mode UTF8String]);
        printf("mazes=%d size=%dx%d walls≈30%%\n", boards, rows, cols);
        printf("average path=%.1f steps\n", (double)total_distance / (double)boards);
        printf("average visited=%.1f cells\n", (double)total_visited / (double)boards);
        printf("elapsed=%.3f ms\n", after - before);
        printf("average=%.3f ms\n", (after - before) / (double)boards);
    }
    return 0;
}
