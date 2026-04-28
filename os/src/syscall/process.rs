//! Process management syscalls
use crate::{
    config::PAGE_SIZE,
    mm::{translated_byte_buffer, MapPermission, PTEFlags, PageTable, VirtAddr},
    task::{
        change_program_brk, current_syscall_count, current_task_mmap, current_task_munmap,
        current_user_token, exit_current_and_run_next, suspend_current_and_run_next,
    },
    timer::get_time_us,
};

#[repr(C)]
#[derive(Debug)]
pub struct TimeVal {
    pub sec: usize,
    pub usec: usize,
}

/// task exits and submit an exit code
pub fn sys_exit(_exit_code: i32) -> ! {
    trace!("kernel: sys_exit");
    exit_current_and_run_next();
    panic!("Unreachable in sys_exit!");
}

/// current task gives up resources for other tasks
pub fn sys_yield() -> isize {
    trace!("kernel: sys_yield");
    suspend_current_and_run_next();
    0
}

/// YOUR JOB: get time with second and microsecond
/// HINT: You might reimplement it with virtual memory management.
/// HINT: What if [`TimeVal`] is splitted by two pages ?
pub fn sys_get_time(ts: *mut TimeVal, _tz: usize) -> isize {
    trace!("kernel: sys_get_time");
    let us = get_time_us();
    let tv = TimeVal {
        sec: us / 1_000_000,
        usec: us % 1_000_000,
    };
    let tv_bytes = unsafe {
        core::slice::from_raw_parts(
            &tv as *const _ as *const u8,
            core::mem::size_of::<TimeVal>(),
        )
    };
    let token = current_user_token();
    let mut buffers =
        translated_byte_buffer(token, ts as *const u8, core::mem::size_of::<TimeVal>());
    let mut offset = 0;
    for buffer in buffers.iter_mut() {
        let len = buffer.len();
        buffer.copy_from_slice(&tv_bytes[offset..offset + len]);
        offset += len;
    }
    0
}

/// TODO: Finish sys_trace to pass testcases
/// HINT: You might reimplement it with virtual memory management.
pub fn sys_trace(trace_request: usize, id: usize, data: usize) -> isize {
    trace!("kernel: sys_trace");
    match trace_request {
        2 => current_syscall_count(id),
        0 | 1 => {
            let page_table = PageTable::from_token(current_user_token());
            let va = VirtAddr::from(id);
            let vpn = va.floor();
            let offset = va.page_offset();
            if let Some(pte) = page_table.translate(vpn) {
                let flags = pte.flags();
                if !pte.is_valid() || !flags.contains(PTEFlags::U) {
                    return -1;
                }
                let phys_array = pte.ppn().get_bytes_array();
                if trace_request == 0 {
                    if !flags.contains(PTEFlags::R) {
                        return -1;
                    }
                    phys_array[offset] as isize
                } else {
                    if !flags.contains(PTEFlags::W) {
                        return -1;
                    }
                    phys_array[offset] = data as u8;
                    0
                }
            } else {
                -1
            }
        }
        _ => -1,
    }
}

// YOUR JOB: Implement mmap.
pub fn sys_mmap(start: usize, len: usize, port: usize) -> isize {
    trace!("kernel: sys_mmap");
    if start % PAGE_SIZE != 0 {
        return -1;
    }
    if port & !0x7 != 0 || port == 0 {
        return -1;
    }
    if len == 0 {
        return 0;
    }
    let Some(end) = start.checked_add(len) else {
        return -1;
    };
    let mut map_perm = MapPermission::U;
    if port & 1 != 0 {
        map_perm |= MapPermission::R;
    }
    if port & 2 != 0 {
        map_perm |= MapPermission::W;
    }
    if port & 4 != 0 {
        map_perm |= MapPermission::X;
    }
    let start_va = VirtAddr::from(start);
    let end_va = VirtAddr::from(end);
    current_task_mmap(start_va, end_va, map_perm)
}

// YOUR JOB: Implement munmap.
pub fn sys_munmap(start: usize, len: usize) -> isize {
    trace!("kernel: sys_munmap");
    if start % PAGE_SIZE != 0 {
        return -1;
    }
    if len == 0 {
        return 0;
    }
    let Some(end) = start.checked_add(len) else {
        return -1;
    };
    let start_va = VirtAddr::from(start);
    let end_va = VirtAddr::from(end);
    current_task_munmap(start_va, end_va)
}

/// change data segment size
pub fn sys_sbrk(size: i32) -> isize {
    trace!("kernel: sys_sbrk");
    if let Some(old_brk) = change_program_brk(size) {
        old_brk as isize
    } else {
        -1
    }
}
