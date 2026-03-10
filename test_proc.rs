use nix::sched::{unshare, CloneFlags};
use nix::mount::{mount, MsFlags};

fn main() {
    unshare(CloneFlags::CLONE_NEWUSER | CloneFlags::CLONE_NEWPID | CloneFlags::CLONE_NEWNS).unwrap();
    let res = mount(Some("proc"), "/tmp", Some("proc"), MsFlags::empty(), None::<&str>);
    println!("Empty flags: {:?}", res);
    
    let res2 = mount(Some("proc"), "/tmp", Some("proc"), MsFlags::MS_NOSUID | MsFlags::MS_NODEV | MsFlags::MS_NOEXEC, None::<&str>);
    println!("With flags: {:?}", res2);
}
