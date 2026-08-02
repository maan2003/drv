use std::{
    env, io,
    os::{
        fd::{AsRawFd as _, FromRawFd as _, OwnedFd, RawFd},
        unix::process::CommandExt as _,
    },
    process::{Child, Command, ExitStatus},
    thread,
    time::Duration,
};

const CHILD_FD: RawFd = 3;
const REAP_INTERVAL: Duration = Duration::from_millis(10);

fn socketpair() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [-1; 2];
    // SAFETY: `fds` points to storage for the two descriptors returned by
    // socketpair. Successful descriptors are immediately wrapped as owned.
    if unsafe {
        libc::socketpair(
            libc::AF_UNIX,
            libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
            0,
            fds.as_mut_ptr(),
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a successful socketpair call returned two fresh descriptors.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

fn child_command(
    program: &str,
    args: &[String],
    endpoint: RawFd,
    other_endpoint: RawFd,
) -> Command {
    let mut command = Command::new(program);
    command.args(args);
    // SAFETY: this closure performs only async-signal-safe descriptor syscalls.
    // It neither allocates nor accesses shared state after fork.
    unsafe {
        command.pre_exec(move || {
            if endpoint == CHILD_FD {
                let flags = libc::fcntl(CHILD_FD, libc::F_GETFD);
                if flags < 0 || libc::fcntl(CHILD_FD, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0
                {
                    return Err(io::Error::last_os_error());
                }
                if other_endpoint != CHILD_FD {
                    libc::close(other_endpoint);
                }
            } else {
                if libc::dup2(endpoint, CHILD_FD) < 0 {
                    return Err(io::Error::last_os_error());
                }
                libc::close(endpoint);
                // If the other endpoint was fd 3, dup2 already closed it.
                if other_endpoint != CHILD_FD && other_endpoint != endpoint {
                    libc::close(other_endpoint);
                }
            }
            Ok(())
        });
    }
    command
}

fn stop(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn supervise(mut daemon: Child, mut link: Child) -> io::Result<()> {
    loop {
        if let Some(status) = daemon.try_wait()? {
            stop(&mut link);
            return child_exit("provider daemon", status);
        }
        if let Some(status) = link.try_wait()? {
            stop(&mut daemon);
            return child_exit("link peer", status);
        }
        thread::sleep(REAP_INTERVAL);
    }
}

fn child_exit(name: &str, status: ExitStatus) -> io::Result<()> {
    Err(io::Error::other(format!("{name} exited: {status}")))
}

fn main() -> io::Result<()> {
    let args: Vec<_> = env::args().skip(1).collect();
    let [daemon_program, device, link_program, link_args @ ..] = args.as_slice() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "usage: netstack3-link-supervisor DAEMON DEVICE LINK_PROGRAM [LINK_ARG ...]",
        ));
    };
    let (daemon_endpoint, link_endpoint) = socketpair()?;
    let daemon_args = [device.clone(), CHILD_FD.to_string()];
    let mut daemon = child_command(
        daemon_program,
        &daemon_args,
        daemon_endpoint.as_raw_fd(),
        link_endpoint.as_raw_fd(),
    )
    .spawn()?;
    let link = match child_command(
        link_program,
        link_args,
        link_endpoint.as_raw_fd(),
        daemon_endpoint.as_raw_fd(),
    )
    .env("NETSTACK3_ETHERNET_FD", CHILD_FD.to_string())
    .spawn()
    {
        Ok(link) => link,
        Err(error) => {
            stop(&mut daemon);
            return Err(error);
        }
    };
    drop((daemon_endpoint, link_endpoint));
    supervise(daemon, link)
}
