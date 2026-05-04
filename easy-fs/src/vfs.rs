use super::{
    block_cache_sync_all, get_block_cache, BlockDevice, DirEntry, DiskInode, DiskInodeType,
    EasyFileSystem, DIRENT_SZ,
};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::{Mutex, MutexGuard};
/// Virtual filesystem layer over easy-fs
pub struct Inode {
    block_id: usize,
    block_offset: usize,
    fs: Arc<Mutex<EasyFileSystem>>,
    block_device: Arc<dyn BlockDevice>,
}

impl Inode {
    /// Create a vfs inode
    pub fn new(
        block_id: u32,
        block_offset: usize,
        fs: Arc<Mutex<EasyFileSystem>>,
        block_device: Arc<dyn BlockDevice>,
    ) -> Self {
        Self {
            block_id: block_id as usize,
            block_offset,
            fs,
            block_device,
        }
    }
    /// Call a function over a disk inode to read it
    fn read_disk_inode<V>(&self, f: impl FnOnce(&DiskInode) -> V) -> V {
        get_block_cache(self.block_id, Arc::clone(&self.block_device))
            .lock()
            .read(self.block_offset, f)
    }
    /// Call a function over a disk inode to modify it
    fn modify_disk_inode<V>(&self, f: impl FnOnce(&mut DiskInode) -> V) -> V {
        get_block_cache(self.block_id, Arc::clone(&self.block_device))
            .lock()
            .modify(self.block_offset, f)
    }
    /// Find inode under a disk inode by name
    fn find_inode_id(&self, name: &str, disk_inode: &DiskInode) -> Option<u32> {
        // assert it is a directory
        assert!(disk_inode.is_dir());
        let file_count = (disk_inode.size as usize) / DIRENT_SZ;
        let mut dirent = DirEntry::empty();
        for i in 0..file_count {
            assert_eq!(
                disk_inode.read_at(DIRENT_SZ * i, dirent.as_bytes_mut(), &self.block_device,),
                DIRENT_SZ,
            );
            if dirent.name() == name {
                return Some(dirent.inode_id() as u32);
            }
        }
        None
    }
    /// Find inode under current inode by name
    pub fn find(&self, name: &str) -> Option<Arc<Inode>> {
        let fs = self.fs.lock();
        self.read_disk_inode(|disk_inode| {
            self.find_inode_id(name, disk_inode).map(|inode_id| {
                let (block_id, block_offset) = fs.get_disk_inode_pos(inode_id);
                Arc::new(Self::new(
                    block_id,
                    block_offset,
                    self.fs.clone(),
                    self.block_device.clone(),
                ))
            })
        })
    }
    /// Increase the size of a disk inode
    fn increase_size(
        &self,
        new_size: u32,
        disk_inode: &mut DiskInode,
        fs: &mut MutexGuard<EasyFileSystem>,
    ) {
        if new_size < disk_inode.size {
            return;
        }
        let blocks_needed = disk_inode.blocks_num_needed(new_size);
        let mut v: Vec<u32> = Vec::new();
        for _ in 0..blocks_needed {
            v.push(fs.alloc_data());
        }
        disk_inode.increase_size(new_size, v, &self.block_device);
    }
    /// Create inode under current inode by name
    pub fn create(&self, name: &str) -> Option<Arc<Inode>> {
        let mut fs = self.fs.lock();
        let op = |root_inode: &DiskInode| {
            // assert it is a directory
            assert!(root_inode.is_dir());
            // has the file been created?
            self.find_inode_id(name, root_inode)
        };
        if self.read_disk_inode(op).is_some() {
            return None;
        }
        // create a new file
        // alloc a inode with an indirect block
        let new_inode_id = fs.alloc_inode();
        // initialize inode
        let (new_inode_block_id, new_inode_block_offset) = fs.get_disk_inode_pos(new_inode_id);
        get_block_cache(new_inode_block_id as usize, Arc::clone(&self.block_device))
            .lock()
            .modify(new_inode_block_offset, |new_inode: &mut DiskInode| {
                new_inode.initialize(DiskInodeType::File);
            });
        self.modify_disk_inode(|root_inode| {
            // append file in the dirent
            let file_count = (root_inode.size as usize) / DIRENT_SZ;
            let new_size = (file_count + 1) * DIRENT_SZ;
            // increase size
            self.increase_size(new_size as u32, root_inode, &mut fs);
            // write dirent
            let dirent = DirEntry::new(name, new_inode_id);
            root_inode.write_at(
                file_count * DIRENT_SZ,
                dirent.as_bytes(),
                &self.block_device,
            );
        });

        let (block_id, block_offset) = fs.get_disk_inode_pos(new_inode_id);
        block_cache_sync_all();
        // return inode
        Some(Arc::new(Self::new(
            block_id,
            block_offset,
            self.fs.clone(),
            self.block_device.clone(),
        )))
        // release efs lock automatically by compiler
    }
    /// List inodes under current inode
    pub fn ls(&self) -> Vec<String> {
        let _fs = self.fs.lock();
        self.read_disk_inode(|disk_inode| {
            let file_count = (disk_inode.size as usize) / DIRENT_SZ;
            let mut v: Vec<String> = Vec::new();
            for i in 0..file_count {
                let mut dirent = DirEntry::empty();
                assert_eq!(
                    disk_inode.read_at(i * DIRENT_SZ, dirent.as_bytes_mut(), &self.block_device,),
                    DIRENT_SZ,
                );
                v.push(String::from(dirent.name()));
            }
            v
        })
    }
    /// Read data from current inode
    pub fn read_at(&self, offset: usize, buf: &mut [u8]) -> usize {
        let _fs = self.fs.lock();
        self.read_disk_inode(|disk_inode| disk_inode.read_at(offset, buf, &self.block_device))
    }
    /// Write data to current inode
    pub fn write_at(&self, offset: usize, buf: &[u8]) -> usize {
        let mut fs = self.fs.lock();
        let size = self.modify_disk_inode(|disk_inode| {
            self.increase_size((offset + buf.len()) as u32, disk_inode, &mut fs);
            disk_inode.write_at(offset, buf, &self.block_device)
        });
        block_cache_sync_all();
        size
    }
    /// Clear the data in current inode
    pub fn clear(&self) {
        let mut fs = self.fs.lock();
        self.modify_disk_inode(|disk_inode| {
            let size = disk_inode.size;
            let data_blocks_dealloc = disk_inode.clear_size(&self.block_device);
            assert!(data_blocks_dealloc.len() == DiskInode::total_blocks(size) as usize);
            for data_block in data_blocks_dealloc.into_iter() {
                fs.dealloc_data(data_block);
            }
        });
        block_cache_sync_all();
    }
    /// Return this inode's on-disk inode id (used by fstat).
    pub fn get_inode_id(&self) -> u32 {
        let fs = self.fs.lock();
        fs.get_inode_id(self.block_id as u32, self.block_offset)
    }
    /// Whether the inode behind this handle is a regular file.
    pub fn is_file(&self) -> bool {
        let _fs = self.fs.lock();
        self.read_disk_inode(|disk_inode| disk_inode.is_file())
    }
    /// Whether the inode behind this handle is a directory.
    pub fn is_dir_inode(&self) -> bool {
        let _fs = self.fs.lock();
        self.read_disk_inode(|disk_inode| disk_inode.is_dir())
    }
    /// Count directory entries inside `self` (which must be a directory) that
    /// reference `target_inode_id`. Used to compute nlink for fstat.
    pub fn link_count_of(&self, target_inode_id: u32) -> u32 {
        let _fs = self.fs.lock();
        self.read_disk_inode(|disk_inode| {
            assert!(disk_inode.is_dir());
            let file_count = (disk_inode.size as usize) / DIRENT_SZ;
            let mut count = 0u32;
            let mut dirent = DirEntry::empty();
            for i in 0..file_count {
                disk_inode.read_at(DIRENT_SZ * i, dirent.as_bytes_mut(), &self.block_device);
                // Slot was emptied by `unlink`.
                if dirent.name().is_empty() {
                    continue;
                }
                if dirent.inode_id() == target_inode_id {
                    count += 1;
                }
            }
            count
        })
    }
    /// Add a hard link `new_name` -> the inode currently called `old_name`.
    /// Both names live in `self`, which must be a directory. Returns false if
    /// the names are equal, `old_name` doesn't exist, or `new_name` already
    /// exists.
    pub fn link(&self, old_name: &str, new_name: &str) -> bool {
        if old_name == new_name {
            return false;
        }
        let mut fs = self.fs.lock();
        // Look up old's inode id.
        let inode_id = match self
            .read_disk_inode(|disk_inode| self.find_inode_id(old_name, disk_inode))
        {
            Some(id) => id,
            None => return false,
        };
        // Refuse if new_name already exists.
        if self
            .read_disk_inode(|disk_inode| self.find_inode_id(new_name, disk_inode))
            .is_some()
        {
            return false;
        }
        // Append a new dirent that points to the same inode.
        self.modify_disk_inode(|root_inode| {
            let file_count = (root_inode.size as usize) / DIRENT_SZ;
            let new_size = (file_count + 1) * DIRENT_SZ;
            self.increase_size(new_size as u32, root_inode, &mut fs);
            let dirent = DirEntry::new(new_name, inode_id);
            root_inode.write_at(
                file_count * DIRENT_SZ,
                dirent.as_bytes(),
                &self.block_device,
            );
        });
        block_cache_sync_all();
        true
    }
    /// Remove the directory entry `name` from `self`. If that was the last
    /// hard link to the underlying inode, clear its data and free the inode.
    pub fn unlink(&self, name: &str) -> bool {
        let mut fs = self.fs.lock();
        // Find the dirent slot and the inode it references.
        let result = self.read_disk_inode(|disk_inode| {
            assert!(disk_inode.is_dir());
            let file_count = (disk_inode.size as usize) / DIRENT_SZ;
            let mut dirent = DirEntry::empty();
            for i in 0..file_count {
                disk_inode.read_at(DIRENT_SZ * i, dirent.as_bytes_mut(), &self.block_device);
                if dirent.name() == name {
                    return Some((i, dirent.inode_id()));
                }
            }
            None
        });
        let (idx, target_inode_id) = match result {
            Some(v) => v,
            None => return false,
        };
        // Overwrite the dirent with an empty entry; future find() calls skip it.
        self.modify_disk_inode(|root_inode| {
            let empty = DirEntry::empty();
            root_inode.write_at(idx * DIRENT_SZ, empty.as_bytes(), &self.block_device);
        });
        // Count remaining links to that inode.
        let nlink = self.read_disk_inode(|disk_inode| {
            let file_count = (disk_inode.size as usize) / DIRENT_SZ;
            let mut dirent = DirEntry::empty();
            let mut count = 0u32;
            for i in 0..file_count {
                disk_inode.read_at(DIRENT_SZ * i, dirent.as_bytes_mut(), &self.block_device);
                if dirent.name().is_empty() {
                    continue;
                }
                if dirent.inode_id() == target_inode_id {
                    count += 1;
                }
            }
            count
        });
        if nlink == 0 {
            // Last reference: clear data blocks then free the inode bit.
            let (target_block_id, target_block_offset) =
                fs.get_disk_inode_pos(target_inode_id);
            let blocks_to_dealloc = get_block_cache(
                target_block_id as usize,
                Arc::clone(&self.block_device),
            )
            .lock()
            .modify(target_block_offset, |di: &mut DiskInode| {
                let size = di.size;
                let v = di.clear_size(&self.block_device);
                assert!(v.len() == DiskInode::total_blocks(size) as usize);
                v
            });
            for db in blocks_to_dealloc {
                fs.dealloc_data(db);
            }
            fs.dealloc_inode(target_inode_id);
        }
        block_cache_sync_all();
        true
    }
}
