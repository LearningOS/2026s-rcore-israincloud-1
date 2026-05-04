//! Process management syscalls
//!
use alloc::sync::Arc;

use crate::{
    fs::{open_file, OpenFlags},
    mm::{translated_byte_buffer, translated_refmut, translated_str, MapPermission, VirtAddr},
    task::{
        add_task, current_task, current_user_token, exit_current_and_run_next,
        suspend_current_and_run_next,
    },
    timer::get_time_us,
};

#[repr(C)]
#[derive(Debug)]
pub struct TimeVal {
    pub sec: usize,
    pub usec: usize,
}

pub fn sys_exit(exit_code: i32) -> ! {
    trace!("kernel:pid[{}] sys_exit", current_task().unwrap().pid.0);
    exit_current_and_run_next(exit_code);
    panic!("Unreachable in sys_exit!");
}

pub fn sys_yield() -> isize {
    //trace!("kernel: sys_yield");
    suspend_current_and_run_next();
    0
}

pub fn sys_getpid() -> isize {
    trace!("kernel: sys_getpid pid:{}", current_task().unwrap().pid.0);
    current_task().unwrap().pid.0 as isize
}

pub fn sys_fork() -> isize {
    trace!("kernel:pid[{}] sys_fork", current_task().unwrap().pid.0);
    let current_task = current_task().unwrap();
    let new_task = current_task.fork();
    let new_pid = new_task.pid.0;
    // modify trap context of new_task, because it returns immediately after switching
    let trap_cx = new_task.inner_exclusive_access().get_trap_cx();
    // we do not have to move to next instruction since we have done it before
    // for child process, fork returns 0
    trap_cx.x[10] = 0;
    // add new task to scheduler
    add_task(new_task);
    new_pid as isize
}

pub fn sys_exec(path: *const u8) -> isize {
    trace!("kernel:pid[{}] sys_exec", current_task().unwrap().pid.0);
    let token = current_user_token();
    let path = translated_str(token, path);
    if let Some(app_inode) = open_file(path.as_str(), OpenFlags::RDONLY) {
        let all_data = app_inode.read_all();
        let task = current_task().unwrap();
        task.exec(all_data.as_slice());
        0
    } else {
        -1
    }
}

/// If there is not a child process whose pid is same as given, return -1.
/// Else if there is a child process but it is still running, return -2.
pub fn sys_waitpid(pid: isize, exit_code_ptr: *mut i32) -> isize {
    //trace!("kernel: sys_waitpid");
    let task = current_task().unwrap();
    // find a child process

    // ---- access current PCB exclusively
    let mut inner = task.inner_exclusive_access();
    if !inner
        .children
        .iter()
        .any(|p| pid == -1 || pid as usize == p.getpid())
    {
        return -1;
        // ---- release current PCB
    }
    let pair = inner.children.iter().enumerate().find(|(_, p)| {
        // ++++ temporarily access child PCB exclusively
        p.inner_exclusive_access().is_zombie() && (pid == -1 || pid as usize == p.getpid())
        // ++++ release child PCB
    });
    if let Some((idx, _)) = pair {
        let child = inner.children.remove(idx);
        // confirm that child will be deallocated after being removed from children list
        assert_eq!(Arc::strong_count(&child), 1);
        let found_pid = child.getpid();
        // ++++ temporarily access child PCB exclusively
        let exit_code = child.inner_exclusive_access().exit_code;
        // ++++ release child PCB
        *translated_refmut(inner.memory_set.token(), exit_code_ptr) = exit_code;
        found_pid as isize
    } else {
        -2
    }
    // ---- release current PCB automatically
}

/// get time with second and microsecond
/// Writes the current time to user-space `ts`. Handles the case where the
/// TimeVal struct straddles two physical pages by writing through the
/// per-page slices returned by translated_byte_buffer.
pub fn sys_get_time(ts: *mut TimeVal, _tz: usize) -> isize {
    trace!(
        "kernel:pid[{}] sys_get_time",
        current_task().unwrap().pid.0
    );
    let us = get_time_us();
    let tv = TimeVal {
        sec: us / 1_000_000,
        usec: us % 1_000_000,
    };
    // Reinterpret TimeVal as raw bytes.
    let src = unsafe {
        core::slice::from_raw_parts(
            &tv as *const TimeVal as *const u8,
            core::mem::size_of::<TimeVal>(),
        )
    };
    let dsts = translated_byte_buffer(current_user_token(), ts as *const u8, src.len());
    let mut copied = 0;
    for dst in dsts {
        let n = dst.len();
        dst.copy_from_slice(&src[copied..copied + n]);
        copied += n;
    }
    0
}

/// Map `len` bytes starting at `start` into the current process's address space
/// with the permissions encoded in `port` (bits 0..2 = R W X). Returns 0 on
/// success and -1 on misaligned start, illegal port, or overlap with an
/// existing mapping.
pub fn sys_mmap(start: usize, len: usize, port: usize) -> isize {
    trace!(
        "kernel:pid[{}] sys_mmap",
        current_task().unwrap().pid.0
    );
    // Reject misaligned start.
    if start % crate::config::PAGE_SIZE != 0 {
        return -1;
    }
    // port bits beyond 0..2 must be zero, and at least one of R/W/X must be set.
    if (port & !0x7) != 0 || (port & 0x7) == 0 {
        return -1;
    }
    if len == 0 {
        return 0;
    }
    let mut perm = MapPermission::U;
    if port & 0x1 != 0 {
        perm |= MapPermission::R;
    }
    if port & 0x2 != 0 {
        perm |= MapPermission::W;
    }
    if port & 0x4 != 0 {
        perm |= MapPermission::X;
    }
    let task = current_task().unwrap();
    let mut inner = task.inner_exclusive_access();
    if inner
        .memory_set
        .mmap(VirtAddr::from(start), VirtAddr::from(start + len), perm)
    {
        0
    } else {
        -1
    }
}

/// Unmap the pages covering `[start, start+len)`. Returns -1 if any page in
/// that range is not currently mapped, or if `start` is misaligned.
pub fn sys_munmap(start: usize, len: usize) -> isize {
    trace!(
        "kernel:pid[{}] sys_munmap",
        current_task().unwrap().pid.0
    );
    if start % crate::config::PAGE_SIZE != 0 {
        return -1;
    }
    if len == 0 {
        return 0;
    }
    let task = current_task().unwrap();
    let mut inner = task.inner_exclusive_access();
    if inner
        .memory_set
        .munmap(VirtAddr::from(start), VirtAddr::from(start + len))
    {
        0
    } else {
        -1
    }
}

/// change data segment size
pub fn sys_sbrk(size: i32) -> isize {
    trace!("kernel:pid[{}] sys_sbrk", current_task().unwrap().pid.0);
    if let Some(old_brk) = current_task().unwrap().change_program_brk(size) {
        old_brk as isize
    } else {
        -1
    }
}

/// Spawn a new child process running the program at `path`.
/// Returns the new pid on success, -1 if the path can't be opened.
pub fn sys_spawn(path: *const u8) -> isize {
    trace!(
        "kernel:pid[{}] sys_spawn",
        current_task().unwrap().pid.0
    );
    let token = current_user_token();
    let path = translated_str(token, path);
    if let Some(app_inode) = open_file(path.as_str(), OpenFlags::RDONLY) {
        let all_data = app_inode.read_all();
        let parent = current_task().unwrap();
        let new_task = parent.spawn(all_data.as_slice());
        let new_pid = new_task.pid.0;
        add_task(new_task);
        new_pid as isize
    } else {
        -1
    }
}

/// Set the current task's stride-scheduling priority. `prio` must be >= 2.
/// Returns the new priority on success, -1 if `prio` is out of range.
pub fn sys_set_priority(prio: isize) -> isize {
    trace!(
        "kernel:pid[{}] sys_set_priority({})",
        current_task().unwrap().pid.0,
        prio
    );
    if prio < 2 {
        return -1;
    }
    let task = current_task().unwrap();
    task.inner_exclusive_access().priority = prio as usize;
    prio
}
