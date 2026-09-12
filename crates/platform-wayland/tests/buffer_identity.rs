//! 回归测试：候选窗 shm buffer 与渲染用 mmap 必须同源。
//!
//! 修复前候选窗走两个独立 memfd：渲染写 B，合成器读 A，候选窗永远空白。
//! 此测试用与 `PopupCanvas::create_buffer` 相同的 memfd + mmap 序列，断言
//! 「写 mmap 指针 → 从 fd 读回」能看到同一份字节。

use std::io::Read;
use std::os::unix::io::FromRawFd;

/// 复刻 `PopupCanvas::create_buffer` 的 memfd + ftruncate + mmap 序列，
/// 返回 (fd, ptr, size)。不建 wl_shm pool（无需 Wayland 连接）。
fn memfd_mmap(size: usize) -> (i32, *mut u8) {
    let fd = unsafe {
        libc::memfd_create(
            b"kime-buffer-identity\0".as_ptr() as *const i8,
            libc::MFD_CLOEXEC,
        )
    };
    assert!(fd >= 0, "memfd_create 失败");
    assert!(
        unsafe { libc::ftruncate(fd, size as libc::off_t) } >= 0,
        "ftruncate 失败"
    );
    let ptr = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            fd,
            0,
        )
    };
    assert_ne!(ptr, libc::MAP_FAILED, "mmap 失败");
    (fd, ptr as *mut u8)
}

#[test]
fn mmap_writes_are_visible_through_the_same_fd() {
    let size = 64 * 4;
    let (fd, ptr) = memfd_mmap(size);

    // 模拟 render.rs 写入像素
    let pixels = unsafe { std::slice::from_raw_parts_mut(ptr, size) };
    for (i, px) in pixels.chunks_exact_mut(4).enumerate() {
        px.copy_from_slice(&[0x1E, 0x1E, 0x26, (i as u8) | 0x80]);
    }

    // 合成器视角：从 pool 的 fd 独立读回
    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    let mut seen = Vec::new();
    use std::io::Seek;
    file.seek(std::io::SeekFrom::Start(0)).unwrap();
    file.read_to_end(&mut seen).unwrap();

    assert_eq!(seen.len(), size, "fd 大小与 mmap 长度不一致");
    assert_eq!(
        &seen[..],
        unsafe { std::slice::from_raw_parts(ptr, size) },
        "同一 memfd 的 mmap 写入未反映到 fd —— buffer 与 mmap 不同源"
    );
    // 具体抽查一个像素，避免两侧同为空导致的假通过
    assert_eq!(&seen[0..4], &[0x1E, 0x1E, 0x26, 0x80]);

    unsafe { libc::munmap(ptr as *mut libc::c_void, size) };
}

#[test]
fn separate_memfds_do_not_share_pixels() {
    // 这就是修复前的形态：buffer 一个 memfd，mmap 另一个 memfd。
    let size = 64 * 4;
    let (fd_a, ptr_a) = memfd_mmap(size);
    let (_fd_b, ptr_b) = memfd_mmap(size);

    unsafe { std::slice::from_raw_parts_mut(ptr_b, size) }.fill(0xAB);

    let mut file_a = unsafe { std::fs::File::from_raw_fd(fd_a) };
    let mut seen_a = Vec::new();
    file_a.read_to_end(&mut seen_a).unwrap();

    assert!(
        seen_a.iter().all(|&b| b == 0),
        "写 B 不应影响 A —— 正是此隔离导致候选窗空白"
    );
    let _ = ptr_a;

    unsafe { libc::munmap(ptr_a as *mut libc::c_void, size) };
    unsafe { libc::munmap(ptr_b as *mut libc::c_void, size) };
}
