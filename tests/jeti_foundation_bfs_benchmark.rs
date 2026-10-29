// Rust mirror of tests/jeti_foundation_bfs_benchmark.jeti.
//
// Same maze, same noise formula, same BFS, same modes, same seeds and the same
// timed region — only the container spelling differs. Three container tiers are
// offered so the comparison against the other languages is honest:
//
//   medium | advanced | random1000      -> Vec<Box<i32>>  (boxed per cell, the
//                                          analogue of NSNumber/NPNumber)
//   vecmedium | vecadvanced | vec1000    -> VecDeque<i32>  (value container, the
//                                          analogue of C++ std::queue<int>)
//   rawmedium | rawadvanced | raw1000    -> flat arrays (the raw C tier)
//
// Build (matching the -O2 the other clients use):
//   rustc -C opt-level=2 -C debug-assertions=off -C overflow-checks=off \
//       tests/jeti_foundation_bfs_benchmark.rs -o /tmp/bench/rust_bfs
//
// The noise function must wrap exactly like C's `unsigned int` arithmetic, so
// every operation is spelled wrapping_* rather than relying on release-mode
// overflow behaviour.

use std::collections::VecDeque;
use std::time::Instant;

const BFS_MAX_CELLS: usize = 262144;

static mut MAZE: [u8; BFS_MAX_CELLS] = [0; BFS_MAX_CELLS];
static mut SEEN: [u8; BFS_MAX_CELLS] = [0; BFS_MAX_CELLS];
static mut RAW_QUEUE: [i32; BFS_MAX_CELLS] = [0; BFS_MAX_CELLS];
static mut RAW_DISTANCE: [i32; BFS_MAX_CELLS] = [0; BFS_MAX_CELLS];

fn maze_noise(r: u32, c: u32, seed: u32) -> u32 {
    let mut x = r
        .wrapping_mul(1103515245)
        .wrapping_add(c.wrapping_mul(12345))
        .wrapping_add(seed.wrapping_mul(2654435761));
    x ^= x >> 16;
    x = x.wrapping_mul(2246822519);
    x ^ (x >> 13)
}

struct Maze {
    rows: usize,
    cols: usize,
}

impl Maze {
    /// About 30% walls, with a guaranteed path along the top/right.
    fn build(&mut self, rows: usize, cols: usize, seed: u32) {
        self.rows = rows;
        self.cols = cols;
        unsafe {
            for r in 0..rows {
                for c in 0..cols {
                    let cell = r * cols + c;
                    MAZE[cell] = if maze_noise(r as u32, c as u32, seed) % 100 < 30 { 1 } else { 0 };
                    SEEN[cell] = 0;
                }
            }
            for c in 0..cols {
                MAZE[c] = 0;
            }
            for r in 0..rows {
                MAZE[r * cols + cols - 1] = 0;
            }
            MAZE[0] = 0;
            MAZE[(rows - 1) * cols + cols - 1] = 0;
        }
    }

    /// Foundation tier: every cell is boxed into its own heap allocation.
    fn shortest_path_boxed(&self, visited: &mut i32) -> i32 {
        let cells = self.rows * self.cols;
        let goal = (self.rows - 1) * self.cols + (self.cols - 1);
        let mut distance = vec![-1i32; cells];
        let mut queue: Vec<Box<i32>> = Vec::with_capacity(cells / 4 + 4);

        unsafe {
            queue.push(Box::new(0));
            SEEN[0] = 1;
            distance[0] = 0;
            *visited = 0;

            let mut head = 0usize;
            while head < queue.len() {
                let cell = *queue[head];
                head += 1;
                let row = cell as usize / self.cols;
                let col = cell as usize % self.cols;
                *visited += 1;

                if cell as usize == goal {
                    return distance[cell as usize];
                }

                let nr = [row as i64 - 1, row as i64 + 1, row as i64, row as i64];
                let nc = [col as i64, col as i64, col as i64 - 1, col as i64 + 1];
                for i in 0..4 {
                    if nr[i] < 0 || nr[i] >= self.rows as i64 || nc[i] < 0 || nc[i] >= self.cols as i64 {
                        continue;
                    }
                    let neighbor = (nr[i] as usize) * self.cols + nc[i] as usize;
                    if MAZE[neighbor] != 0 || SEEN[neighbor] != 0 {
                        continue;
                    }
                    SEEN[neighbor] = 1;
                    let next = neighbor as i32;
                    distance[next as usize] = distance[cell as usize] + 1;
                    queue.push(Box::new(next));
                }
            }
        }
        -1
    }

    /// Value-container tier: the analogue of C++'s std::queue<int>.
    fn shortest_path_vecdeque(&self, visited: &mut i32) -> i32 {
        let cells = self.rows * self.cols;
        let goal = (self.rows - 1) * self.cols + (self.cols - 1);
        let mut distance = vec![-1i32; cells];
        let mut queue: VecDeque<i32> = VecDeque::with_capacity(cells / 4 + 4);

        unsafe {
            queue.push_back(0);
            SEEN[0] = 1;
            distance[0] = 0;
            *visited = 0;

            while let Some(cell) = queue.pop_front() {
                let row = cell as usize / self.cols;
                let col = cell as usize % self.cols;
                *visited += 1;

                if cell as usize == goal {
                    return distance[cell as usize];
                }

                let nr = [row as i64 - 1, row as i64 + 1, row as i64, row as i64];
                let nc = [col as i64, col as i64, col as i64 - 1, col as i64 + 1];
                for i in 0..4 {
                    if nr[i] < 0 || nr[i] >= self.rows as i64 || nc[i] < 0 || nc[i] >= self.cols as i64 {
                        continue;
                    }
                    let neighbor = (nr[i] as usize) * self.cols + nc[i] as usize;
                    if MAZE[neighbor] != 0 || SEEN[neighbor] != 0 {
                        continue;
                    }
                    SEEN[neighbor] = 1;
                    let next = neighbor as i32;
                    distance[next as usize] = distance[cell as usize] + 1;
                    queue.push_back(next);
                }
            }
        }
        -1
    }

    /// Raw tier: the hot path is only primitive flat storage.
    fn shortest_path_raw(&self, visited: &mut i32) -> i32 {
        let cells = self.rows * self.cols;
        let goal = (self.rows - 1) * self.cols + (self.cols - 1);

        unsafe {
            for i in 0..cells {
                RAW_DISTANCE[i] = -1;
            }
            let mut head = 0usize;
            let mut tail = 0usize;
            RAW_QUEUE[tail] = 0;
            tail += 1;
            SEEN[0] = 1;
            RAW_DISTANCE[0] = 0;
            *visited = 0;

            while head < tail {
                let cell = RAW_QUEUE[head];
                head += 1;
                let row = cell as usize / self.cols;
                let col = cell as usize % self.cols;
                *visited += 1;
                if cell as usize == goal {
                    return RAW_DISTANCE[cell as usize];
                }

                let nr = [row as i64 - 1, row as i64 + 1, row as i64, row as i64];
                let nc = [col as i64, col as i64, col as i64 - 1, col as i64 + 1];
                for i in 0..4 {
                    if nr[i] < 0 || nr[i] >= self.rows as i64 || nc[i] < 0 || nc[i] >= self.cols as i64 {
                        continue;
                    }
                    let neighbor = (nr[i] as usize) * self.cols + nc[i] as usize;
                    if MAZE[neighbor] != 0 || SEEN[neighbor] != 0 {
                        continue;
                    }
                    SEEN[neighbor] = 1;
                    RAW_QUEUE[tail] = neighbor as i32;
                    tail += 1;
                    RAW_DISTANCE[neighbor] = RAW_DISTANCE[cell as usize] + 1;
                }
            }
        }
        -1
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Tier {
    Boxed,
    VecDeque,
    Raw,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let arg = args.get(1).map(|s| s.as_str()).unwrap_or("advanced");

    let mut rows = 256usize;
    let mut cols = 256usize;
    let mut boards = 1usize;
    let mut seed = 7u32;

    let (mode, tier): (&str, Tier) = match arg {
        "medium" => {
            rows = 128;
            cols = 128;
            seed = 3;
            ("medium", Tier::Boxed)
        }
        "advanced" => ("advanced", Tier::Boxed),
        "random1000" => {
            rows = 64;
            cols = 64;
            boards = 1000;
            seed = 100;
            ("random1000", Tier::Boxed)
        }
        "vecmedium" => {
            rows = 128;
            cols = 128;
            seed = 3;
            ("vecmedium", Tier::VecDeque)
        }
        "vecadvanced" => ("vecadvanced", Tier::VecDeque),
        "vec1000" => {
            rows = 64;
            cols = 64;
            boards = 1000;
            seed = 100;
            ("vec1000", Tier::VecDeque)
        }
        "rawmedium" => {
            rows = 128;
            cols = 128;
            seed = 3;
            ("rawmedium", Tier::Raw)
        }
        "rawadvanced" => ("rawadvanced", Tier::Raw),
        "raw1000" => {
            rows = 64;
            cols = 64;
            boards = 1000;
            seed = 100;
            ("raw1000", Tier::Raw)
        }
        _ => {
            eprintln!("usage: {} [medium|advanced|random1000|vecmedium|vecadvanced|vec1000|rawmedium|rawadvanced|raw1000]", args[0]);
            std::process::exit(2);
        }
    };

    let mut maze = Maze { rows, cols };
    maze.build(rows, cols, seed);

    let mut visited = 0i32;
    let run = |maze: &Maze, tier: Tier, visited: &mut i32| -> i32 {
        match tier {
            Tier::Boxed => maze.shortest_path_boxed(visited),
            Tier::VecDeque => maze.shortest_path_vecdeque(visited),
            Tier::Raw => maze.shortest_path_raw(visited),
        }
    };

    // Warm up once so the reported sample is not dominated by first-use setup.
    let _ = run(&maze, tier, &mut visited);

    let mut total_visited: i64 = 0;
    let mut total_distance: i64 = 0;
    let before = Instant::now();
    for board in 0..boards {
        maze.build(rows, cols, seed + (board as u32) * 101);
        let distance = run(&maze, tier, &mut visited);
        total_distance += distance as i64;
        total_visited += visited as i64;
    }
    let elapsed = before.elapsed().as_secs_f64() * 1000.0;

    println!("Rust BFS");
    println!("mode={mode}");
    println!("mazes={boards} size={rows}x{cols} walls≈30%");
    println!("average path={:.1} steps", total_distance as f64 / boards as f64);
    println!("average visited={:.1} cells", total_visited as f64 / boards as f64);
    println!("elapsed={elapsed:.3} ms");
    println!("average={:.3} ms", elapsed / boards as f64);
}
