use crate::sync::{is_safe, Condvar, Mutex, MutexBlocking, MutexSpin, Semaphore, DEADLOCK_DETECTED};
use crate::task::{block_current_and_run_next, current_process, current_task};
use crate::timer::{add_timer, get_time_ms};
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
/// sleep syscall
pub fn sys_sleep(ms: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_sleep",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let expire_ms = get_time_ms() + ms;
    let task = current_task().unwrap();
    add_timer(expire_ms, task);
    block_current_and_run_next();
    0
}
/// mutex create syscall
pub fn sys_mutex_create(blocking: bool) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_mutex_create",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let process = current_process();
    let mutex: Option<Arc<dyn Mutex>> = if !blocking {
        Some(Arc::new(MutexSpin::new()))
    } else {
        Some(Arc::new(MutexBlocking::new()))
    };
    let mut process_inner = process.inner_exclusive_access();
    if let Some(id) = process_inner
        .mutex_list
        .iter()
        .enumerate()
        .find(|(_, item)| item.is_none())
        .map(|(id, _)| id)
    {
        process_inner.mutex_list[id] = mutex;
        // Ensure mutex_holders has enough space
        while id >= process_inner.mutex_holders.len() {
            process_inner.mutex_holders.push(None);
        }
        process_inner.mutex_holders[id] = None;
        id as isize
    } else {
        process_inner.mutex_list.push(mutex);
        process_inner.mutex_holders.push(None);
        process_inner.mutex_list.len() as isize - 1
    }
}
/// mutex lock syscall
pub fn sys_mutex_lock(mutex_id: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_mutex_lock",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let process = current_process();
    let current_task = current_task().unwrap();
    let current_tid = current_task
        .inner_exclusive_access()
        .res
        .as_ref()
        .unwrap()
        .tid;

    // Deadlock detection
    {
        let process_inner = process.inner_exclusive_access();
        if process_inner.deadlock_detect_enabled {
            let mutex_count = process_inner.mutex_list.len();
            let mutex_holders = process_inner.mutex_holders.clone();

            // If the current thread already holds this mutex, requesting it again
            // would be a self-deadlock.
            if let Some(Some(holder_tid)) = mutex_holders.get(mutex_id) {
                if *holder_tid == current_tid {
                    return DEADLOCK_DETECTED;
                }
            }

            let thread_count = process_inner.tasks.iter().filter(|t| t.is_some()).count();

            if !check_mutex_deadlock_safe(
                thread_count,
                mutex_count,
                current_tid,
                mutex_id,
                &mutex_holders,
            ) {
                return DEADLOCK_DETECTED;
            }
        }
    }

    let process_inner = process.inner_exclusive_access();
    let mutex = Arc::clone(process_inner.mutex_list[mutex_id].as_ref().unwrap());
    drop(process_inner);
    mutex.lock();

    // Record that this thread now holds the mutex.
    let mut process_inner = process.inner_exclusive_access();
    while mutex_id >= process_inner.mutex_holders.len() {
        process_inner.mutex_holders.push(None);
    }
    process_inner.mutex_holders[mutex_id] = Some(current_tid);

    0
}

/// Banker's-algorithm based deadlock check for mutex requests.
/// Each mutex is treated as a single-instance resource.
fn check_mutex_deadlock_safe(
    thread_count: usize,
    mutex_count: usize,
    current_tid: usize,
    mutex_id: usize,
    mutex_holders: &[Option<usize>],
) -> bool {
    if thread_count == 0 || mutex_count == 0 {
        return true;
    }

    // available[j] = 1 if mutex j is free, 0 otherwise
    let mut available = Vec::with_capacity(mutex_count);
    for j in 0..mutex_count {
        if j < mutex_holders.len() {
            available.push(if mutex_holders[j].is_none() { 1 } else { 0 });
        } else {
            available.push(1);
        }
    }

    // Collect all thread tids involved (holders + the current requester).
    let mut all_tids: Vec<usize> = mutex_holders.iter().filter_map(|h| *h).collect();
    if !all_tids.contains(&current_tid) {
        all_tids.push(current_tid);
    }
    let n = all_tids.len();

    let mut tid_to_idx = BTreeMap::new();
    for (idx, &tid) in all_tids.iter().enumerate() {
        tid_to_idx.insert(tid, idx);
    }

    // allocation[i][j] = 1 if thread i holds mutex j
    let mut allocation = vec![vec![0usize; mutex_count]; n];
    for j in 0..mutex_holders.len() {
        if let Some(holder_tid) = mutex_holders[j] {
            if let Some(&holder_idx) = tid_to_idx.get(&holder_tid) {
                if j < mutex_count {
                    allocation[holder_idx][j] = 1;
                }
            }
        }
    }

    // need[i][j] = 1 only for the current request
    let mut need = vec![vec![0usize; mutex_count]; n];
    if let Some(&current_idx) = tid_to_idx.get(&current_tid) {
        if mutex_id < mutex_count {
            need[current_idx][mutex_id] = 1;
        }
    }

    is_safe(&available, &allocation, &need)
}

/// mutex unlock syscall
pub fn sys_mutex_unlock(mutex_id: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_mutex_unlock",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let process = current_process();
    let current_tid = current_task()
        .unwrap()
        .inner_exclusive_access()
        .res
        .as_ref()
        .unwrap()
        .tid;

    let process_inner = process.inner_exclusive_access();
    let mutex = Arc::clone(process_inner.mutex_list[mutex_id].as_ref().unwrap());
    drop(process_inner);
    mutex.unlock();

    // Clear the holder if this thread was the holder.
    let mut process_inner = process.inner_exclusive_access();
    if mutex_id < process_inner.mutex_holders.len()
        && process_inner.mutex_holders[mutex_id] == Some(current_tid)
    {
        process_inner.mutex_holders[mutex_id] = None;
    }

    0
}
/// semaphore create syscall
pub fn sys_semaphore_create(res_count: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_semaphore_create",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let process = current_process();
    let mut process_inner = process.inner_exclusive_access();
    let id = if let Some(id) = process_inner
        .semaphore_list
        .iter()
        .enumerate()
        .find(|(_, item)| item.is_none())
        .map(|(id, _)| id)
    {
        process_inner.semaphore_list[id] = Some(Arc::new(Semaphore::new(res_count)));
        // Make sure each thread's allocation row covers this semaphore id.
        for alloc in process_inner.sem_allocation.iter_mut() {
            while id >= alloc.len() {
                alloc.push(0);
            }
            alloc[id] = 0;
        }
        id
    } else {
        process_inner
            .semaphore_list
            .push(Some(Arc::new(Semaphore::new(res_count))));
        let new_id = process_inner.semaphore_list.len() - 1;
        // Append a column to every existing thread's allocation row.
        for alloc in process_inner.sem_allocation.iter_mut() {
            alloc.push(0);
        }
        new_id
    };
    id as isize
}
/// semaphore up syscall
pub fn sys_semaphore_up(sem_id: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_semaphore_up",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let process = current_process();
    let current_tid = current_task()
        .unwrap()
        .inner_exclusive_access()
        .res
        .as_ref()
        .unwrap()
        .tid;

    let process_inner = process.inner_exclusive_access();
    let sem = Arc::clone(process_inner.semaphore_list[sem_id].as_ref().unwrap());
    drop(process_inner);
    sem.up();

    // Decrement this thread's allocation count for the semaphore.
    let mut process_inner = process.inner_exclusive_access();
    if current_tid < process_inner.sem_allocation.len()
        && sem_id < process_inner.sem_allocation[current_tid].len()
        && process_inner.sem_allocation[current_tid][sem_id] > 0
    {
        process_inner.sem_allocation[current_tid][sem_id] -= 1;
    }

    0
}
/// semaphore down syscall
pub fn sys_semaphore_down(sem_id: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_semaphore_down",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let process = current_process();
    let current_tid = current_task()
        .unwrap()
        .inner_exclusive_access()
        .res
        .as_ref()
        .unwrap()
        .tid;

    // Deadlock detection (only meaningful when the resource is unavailable).
    {
        let process_inner = process.inner_exclusive_access();
        if process_inner.deadlock_detect_enabled {
            let sem_count = process_inner.semaphore_list.len();

            let sem_available: Vec<isize> = process_inner
                .semaphore_list
                .iter()
                .map(|sem| {
                    sem.as_ref()
                        .map(|s| s.inner.exclusive_access().count)
                        .unwrap_or(0)
                })
                .collect();

            if sem_id < sem_available.len() && sem_available[sem_id] <= 0 {
                let sem_allocation = process_inner.sem_allocation.clone();

                // Snapshot wait queues from each semaphore (tid lists).
                let sem_wait_queues: Vec<Vec<usize>> = process_inner
                    .semaphore_list
                    .iter()
                    .map(|sem| {
                        sem.as_ref()
                            .map(|s| {
                                s.inner
                                    .exclusive_access()
                                    .wait_queue
                                    .iter()
                                    .filter_map(|task| {
                                        task.inner_exclusive_access()
                                            .res
                                            .as_ref()
                                            .map(|r| r.tid)
                                    })
                                    .collect()
                            })
                            .unwrap_or_default()
                    })
                    .collect();

                if !check_semaphore_deadlock_with_queues(
                    sem_count,
                    current_tid,
                    sem_id,
                    &sem_available,
                    &sem_allocation,
                    &sem_wait_queues,
                ) {
                    return DEADLOCK_DETECTED;
                }
            }
        }
    }

    let process_inner = process.inner_exclusive_access();
    let sem = Arc::clone(process_inner.semaphore_list[sem_id].as_ref().unwrap());
    drop(process_inner);
    sem.down();

    // Increment this thread's allocation count for the semaphore.
    let mut process_inner = process.inner_exclusive_access();
    while current_tid >= process_inner.sem_allocation.len() {
        process_inner.sem_allocation.push(Vec::new());
    }
    while sem_id >= process_inner.sem_allocation[current_tid].len() {
        process_inner.sem_allocation[current_tid].push(0);
    }
    process_inner.sem_allocation[current_tid][sem_id] += 1;

    0
}

/// Banker's-algorithm based deadlock check for semaphore requests, taking
/// already-blocked threads (in the wait queues) into account.
fn check_semaphore_deadlock_with_queues(
    sem_count: usize,
    current_tid: usize,
    sem_id: usize,
    sem_available: &[isize],
    sem_allocation: &[Vec<usize>],
    sem_wait_queues: &[Vec<usize>],
) -> bool {
    if sem_count == 0 {
        return true;
    }

    // All threads with allocations or in some wait queue, plus the current one.
    let mut all_tids: Vec<usize> = sem_allocation
        .iter()
        .enumerate()
        .filter(|(tid, alloc)| {
            alloc.iter().any(|&c| c > 0)
                || sem_wait_queues.iter().any(|queue| queue.contains(tid))
        })
        .map(|(tid, _)| tid)
        .collect();
    if !all_tids.contains(&current_tid) {
        all_tids.push(current_tid);
    }
    let n = all_tids.len();
    if n == 0 {
        return true;
    }

    let mut tid_to_idx = BTreeMap::new();
    for (idx, &tid) in all_tids.iter().enumerate() {
        tid_to_idx.insert(tid, idx);
    }

    // allocation[i][j] = units of semaphore j held by thread i
    let mut allocation = vec![vec![0usize; sem_count]; n];
    for (tid, alloc_vec) in sem_allocation.iter().enumerate() {
        if let Some(&idx) = tid_to_idx.get(&tid) {
            for (j, &count) in alloc_vec.iter().enumerate() {
                if j < sem_count && count > 0 {
                    allocation[idx][j] = count;
                }
            }
        }
    }

    // request[i][j] = units thread i is currently requesting (waiting for)
    let mut request = vec![vec![0usize; sem_count]; n];
    if let Some(&current_idx) = tid_to_idx.get(&current_tid) {
        if sem_id < sem_count {
            request[current_idx][sem_id] = 1;
        }
    }
    for (sem_j, queue) in sem_wait_queues.iter().enumerate() {
        for &tid in queue {
            if let Some(&idx) = tid_to_idx.get(&tid) {
                if sem_j < sem_count {
                    request[idx][sem_j] = 1;
                }
            }
        }
    }

    let available: Vec<usize> = (0..sem_count)
        .map(|j| {
            if j < sem_available.len() && sem_available[j] > 0 {
                sem_available[j] as usize
            } else {
                0
            }
        })
        .collect();

    // Standard deadlock-detection (request matrix) variant of the
    // banker's algorithm.
    let mut work = available;
    let mut finish = vec![false; n];

    loop {
        let mut found = false;
        for i in 0..n {
            if !finish[i] {
                let can_allocate = (0..sem_count).all(|j| request[i][j] <= work[j]);
                if can_allocate {
                    for j in 0..sem_count {
                        work[j] += allocation[i][j];
                    }
                    finish[i] = true;
                    found = true;
                }
            }
        }
        if !found {
            break;
        }
    }

    finish.iter().all(|&f| f)
}

/// condvar create syscall
pub fn sys_condvar_create() -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_condvar_create",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let process = current_process();
    let mut process_inner = process.inner_exclusive_access();
    let id = if let Some(id) = process_inner
        .condvar_list
        .iter()
        .enumerate()
        .find(|(_, item)| item.is_none())
        .map(|(id, _)| id)
    {
        process_inner.condvar_list[id] = Some(Arc::new(Condvar::new()));
        id
    } else {
        process_inner
            .condvar_list
            .push(Some(Arc::new(Condvar::new())));
        process_inner.condvar_list.len() - 1
    };
    id as isize
}
/// condvar signal syscall
pub fn sys_condvar_signal(condvar_id: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_condvar_signal",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let process = current_process();
    let process_inner = process.inner_exclusive_access();
    let condvar = Arc::clone(process_inner.condvar_list[condvar_id].as_ref().unwrap());
    drop(process_inner);
    condvar.signal();
    0
}
/// condvar wait syscall
pub fn sys_condvar_wait(condvar_id: usize, mutex_id: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_condvar_wait",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let process = current_process();
    let process_inner = process.inner_exclusive_access();
    let condvar = Arc::clone(process_inner.condvar_list[condvar_id].as_ref().unwrap());
    let mutex = Arc::clone(process_inner.mutex_list[mutex_id].as_ref().unwrap());
    drop(process_inner);
    condvar.wait(mutex);
    0
}
/// enable deadlock detection syscall
///
/// YOUR JOB: Implement deadlock detection, but might not all in this syscall
pub fn sys_enable_deadlock_detect(enabled: usize) -> isize {
    trace!("kernel: sys_enable_deadlock_detect");
    let process = current_process();
    let mut process_inner = process.inner_exclusive_access();

    match enabled {
        0 => {
            process_inner.deadlock_detect_enabled = false;
            0
        }
        1 => {
            process_inner.deadlock_detect_enabled = true;
            0
        }
        _ => -1, // invalid argument
    }
}
