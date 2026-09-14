//! Guest-only negative sandbox probes. Never run on a shared host.
use std::os::fd::AsRawFd;
fn main() {
    let caps = drv_dns_service::sandbox::lockdown().unwrap();
    let mode = std::env::args().nth(1).unwrap();
    unsafe {
        match mode.as_str() {
            "raw" => {
                libc::socket(libc::AF_INET, libc::SOCK_RAW, libc::IPPROTO_ICMP);
            }
            "netlink" => {
                libc::socket(libc::AF_NETLINK, libc::SOCK_DGRAM, 0);
            }
            "rights" => {
                libc::recvmsg(caps.nss.as_raw_fd(), std::ptr::null_mut(), 0);
            }
            "exec" => {
                libc::execve(c"/bin/sh".as_ptr(), std::ptr::null(), std::ptr::null());
            }
            "process" => {
                libc::syscall(libc::SYS_clone, libc::SIGCHLD, 0, 0, 0, 0);
            }
            "exec-memory" => {
                libc::mmap(
                    std::ptr::null_mut(),
                    4096,
                    libc::PROT_READ | libc::PROT_EXEC,
                    libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                    -1,
                    0,
                );
            }
            "file-map" => {
                libc::mmap(
                    std::ptr::null_mut(),
                    4096,
                    libc::PROT_READ,
                    libc::MAP_SHARED,
                    caps.config.as_raw_fd(),
                    0,
                );
            }
            "remap-alias" => {
                libc::syscall(libc::SYS_mremap, 0, 4096, 4096, libc::MREMAP_MAYMOVE | 4, 0);
            }
            "open" => {
                assert_eq!(libc::open(c"/etc/passwd".as_ptr(), libc::O_RDONLY), -1);
                assert_eq!(
                    std::io::Error::last_os_error().raw_os_error(),
                    Some(libc::ENOENT)
                );
                assert_eq!(libc::open(c"/dev/mem".as_ptr(), libc::O_RDWR), -1);
                println!("PASS_DNS_NO_FILESYSTEM");
                return;
            }
            _ => panic!("unknown probe"),
        }
    }
    panic!("forbidden syscall returned");
}
