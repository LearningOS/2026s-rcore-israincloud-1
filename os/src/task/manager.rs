//!Implementation of [`TaskManager`]
use super::TaskControlBlock;
use crate::sync::UPSafeCell;
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use lazy_static::*;

/// BIG_STRIDE used by the stride scheduler. Each task's pass per pick is
/// BIG_STRIDE / priority, so smaller priority values yield larger passes
/// (i.e. less CPU time).
const BIG_STRIDE: usize = 65536;

///A array of `TaskControlBlock` that is thread-safe
pub struct TaskManager {
    ready_queue: VecDeque<Arc<TaskControlBlock>>,
}

/// Stride scheduler.
impl TaskManager {
    ///Creat an empty TaskManager
    pub fn new() -> Self {
        Self {
            ready_queue: VecDeque::new(),
        }
    }
    /// Add process back to ready queue
    pub fn add(&mut self, task: Arc<TaskControlBlock>) {
        self.ready_queue.push_back(task);
    }
    /// Pick the ready task with the smallest stride, advance its stride by
    /// its pass, and return it.
    pub fn fetch(&mut self) -> Option<Arc<TaskControlBlock>> {
        if self.ready_queue.is_empty() {
            return None;
        }
        // Find index of the task with the smallest stride.
        let mut min_idx = 0usize;
        let mut min_stride = self.ready_queue[0].inner_exclusive_access().stride;
        for (idx, task) in self.ready_queue.iter().enumerate().skip(1) {
            let s = task.inner_exclusive_access().stride;
            if s < min_stride {
                min_stride = s;
                min_idx = idx;
            }
        }
        let task = self.ready_queue.remove(min_idx).unwrap();
        // Advance the chosen task's stride by its pass.
        {
            let mut inner = task.inner_exclusive_access();
            let prio = inner.priority.max(2);
            inner.stride = inner.stride.wrapping_add(BIG_STRIDE / prio);
        }
        Some(task)
    }
}

lazy_static! {
    /// TASK_MANAGER instance through lazy_static!
    pub static ref TASK_MANAGER: UPSafeCell<TaskManager> =
        unsafe { UPSafeCell::new(TaskManager::new()) };
}

/// Add process to ready queue
pub fn add_task(task: Arc<TaskControlBlock>) {
    //trace!("kernel: TaskManager::add_task");
    TASK_MANAGER.exclusive_access().add(task);
}

/// Take a process out of the ready queue
pub fn fetch_task() -> Option<Arc<TaskControlBlock>> {
    //trace!("kernel: TaskManager::fetch_task");
    TASK_MANAGER.exclusive_access().fetch()
}
