//! Synchronization and interior mutability primitives

mod condvar;
mod mutex;
mod semaphore;
mod up;

pub use condvar::Condvar;
pub use mutex::{Mutex, MutexBlocking, MutexSpin};
pub use semaphore::Semaphore;
pub use up::UPSafeCell;

use alloc::vec;
use alloc::vec::Vec;

/// Error code returned by sync syscalls when a deadlock is detected.
pub const DEADLOCK_DETECTED: isize = -0xDEAD;

/// Banker's algorithm safety check.
///
/// * `available[j]`        - currently available units of resource `j`.
/// * `allocation[i][j]`    - units of resource `j` currently held by thread `i`.
/// * `need[i][j]`          - units of resource `j` thread `i` may still request.
///
/// Returns `true` if there exists a safe execution sequence (i.e. the
/// system is in a safe state and the current request can be granted
/// without risking deadlock); otherwise returns `false`.
pub fn is_safe(
    available: &[usize],
    allocation: &[Vec<usize>],
    need: &[Vec<usize>],
) -> bool {
    let n = allocation.len();
    let m = available.len();
    if n == 0 {
        return true;
    }

    let mut work: Vec<usize> = available.to_vec();
    let mut finish = vec![false; n];

    // Repeatedly try to find a thread whose remaining need can be satisfied.
    loop {
        let mut progressed = false;
        for i in 0..n {
            if finish[i] {
                continue;
            }
            // Need[i] <= Work ?
            let mut ok = true;
            for j in 0..m {
                let need_ij = need.get(i).and_then(|row| row.get(j)).copied().unwrap_or(0);
                if need_ij > work[j] {
                    ok = false;
                    break;
                }
            }
            if ok {
                // Pretend thread i finishes and returns its allocation.
                for j in 0..m {
                    let alloc_ij = allocation
                        .get(i)
                        .and_then(|row| row.get(j))
                        .copied()
                        .unwrap_or(0);
                    work[j] += alloc_ij;
                }
                finish[i] = true;
                progressed = true;
            }
        }
        if !progressed {
            break;
        }
    }

    finish.iter().all(|&f| f)
}
