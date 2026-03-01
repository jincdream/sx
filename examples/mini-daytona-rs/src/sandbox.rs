use nix::mount::{mount, MsFlags};
use nix::sched::{clone, CloneFlags};
use nix::sys::signal::Signal;
use nix::sys::wait::waitpid;
use nix::unistd::{chdir, chroot, execvp, sethostname};
use std::ffi::CString;

const STACK_SIZE: usize = 1024 * 1024;

pub fn run_sandbox(merged_dir: &str, cmd: &[&str]) -> anyhow::Result<()> {
    let mut stack = vec![0u8; STACK_SIZE];

    let flags = CloneFlags::CLONE_NEWNS
        | CloneFlags::CLONE_NEWPID
        | CloneFlags::CLONE_NEWUTS
        | CloneFlags::CLONE_NEWIPC
        | CloneFlags::CLONE_NEWNET;

    let merged_dir_c = CString::new(merged_dir)?;
    let cmd_c: Vec<CString> = cmd.iter().map(|s| CString::new(*s).unwrap()).collect();

    let child_pid = unsafe {
        clone(
            Box::new(|| {
                if let Err(e) = child(merged_dir_c.as_c_str(), &cmd_c) {
                    eprintln!("Child error: {}", e);
                    return -1;
                }
                0
            }),
            &mut stack,
            flags,
            Some(Signal::SIGCHLD as i32),
        )?
    };

    let _status = waitpid(child_pid, None)?;
    Ok(())
}

fn child(merged_dir: &std::ffi::CStr, cmd: &[CString]) -> anyhow::Result<()> {
    sethostname("mini-daytona")?;

    mount(
        None::<&str>,
        "/",
        None::<&str>,
        MsFlags::MS_PRIVATE | MsFlags::MS_REC,
        None::<&str>,
    )?;

    chroot(merged_dir)?;
    chdir("/")?;

    mount(
        Some("proc"),
        "/proc",
        Some("proc"),
        MsFlags::empty(),
        None::<&str>,
    )?;

    if !cmd.is_empty() {
        execvp(&cmd[0], cmd)?;
    } else {
        let sh = CString::new("/bin/sh")?;
        execvp(&sh, std::slice::from_ref(&sh))?;
    }

    Ok(())
}
