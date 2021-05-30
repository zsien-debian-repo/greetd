use std::{env, ffi::CString, os::unix::net::UnixDatagram};

use nix::{
    sys::wait::waitpid,
    unistd::{execve, fork, initgroups, setgid, setsid, setuid, ForkResult, Gid, Uid},
};
use pam_sys::{PamFlag, PamItemType};
use serde::{Deserialize, Serialize};
use users::os::unix::UserExt;

use super::{
    conv::SessionConv,
    prctl::{prctl, PrctlOption},
};
use crate::{error::Error, pam::session::PamSession, terminal};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum AuthMessageType {
    Visible,
    Secret,
    Info,
    Error,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum TerminalMode {
    Terminal {
        path: String,
        vt: usize,
        switch: bool,
    },
    Stdin,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ParentToSessionChild {
    InitiateLogin {
        service: String,
        class: String,
        user: String,
        authenticate: bool,
        tty: TerminalMode,
        source_profile: bool,
    },
    PamResponse {
        resp: Option<String>,
    },
    Args {
        cmd: Vec<String>,
    },
    Start,
    Cancel,
}

impl ParentToSessionChild {
    pub fn recv(sock: &UnixDatagram) -> Result<ParentToSessionChild, Error> {
        let mut data = [0; 10240];
        let len = sock.recv(&mut data[..])?;
        let msg = serde_json::from_slice(&data[..len])?;
        Ok(msg)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum SessionChildToParent {
    Success,
    Error(Error),
    PamMessage { style: AuthMessageType, msg: String },
    FinalChildPid(u64),
}

impl SessionChildToParent {
    pub fn send(&self, sock: &UnixDatagram) -> Result<(), Error> {
        let out = serde_json::to_vec(self)?;
        sock.send(&out)?;
        Ok(())
    }
}

/// The entry point for the session worker process. The session worker is
/// responsible for the entirety of the session setup and execution. It is
/// started by Session::start.
fn worker(sock: &UnixDatagram) -> Result<(), Error> {
    let (service, class, user, authenticate, tty, source_profile) =
        match ParentToSessionChild::recv(sock)? {
            ParentToSessionChild::InitiateLogin {
                service,
                class,
                user,
                authenticate,
                tty,
                source_profile,
            } => (service, class, user, authenticate, tty, source_profile),
            ParentToSessionChild::Cancel => return Err("cancelled".into()),
            msg => return Err(format!("expected InitiateLogin or Cancel, got: {:?}", msg).into()),
        };

    let conv = Box::pin(SessionConv::new(sock));
    let mut pam = PamSession::start(&service, &user, conv)?;

    if authenticate {
        pam.authenticate(PamFlag::NONE)?;
    }
    pam.acct_mgmt(PamFlag::NONE)?;

    // Not the credentials you think.
    pam.setcred(PamFlag::ESTABLISH_CRED)?;

    // Mark authentication as a success.
    SessionChildToParent::Success.send(sock)?;

    // Fetch our arguments from the parent.
    let cmd = match ParentToSessionChild::recv(sock)? {
        ParentToSessionChild::Args { cmd } => cmd,
        ParentToSessionChild::Cancel => return Err("cancelled".into()),
        msg => return Err(format!("expected Args or Cancel, got: {:?}", msg).into()),
    };

    SessionChildToParent::Success.send(sock)?;

    // Await start request from our parent.
    match ParentToSessionChild::recv(sock)? {
        ParentToSessionChild::Start => (),
        ParentToSessionChild::Cancel => return Err("cancelled".into()),
        msg => return Err(format!("expected Start or Cancel, got: {:?}", msg).into()),
    };

    let pam_username = pam.get_user()?;

    let user = users::get_user_by_name(&pam_username).ok_or("unable to get user info")?;

    // Make this process a session leader.
    setsid().map_err(|e| format!("unable to become session leader: {}", e))?;

    match tty {
        TerminalMode::Stdin => (),
        TerminalMode::Terminal { path, vt, switch } => {
            // Tell PAM what TTY we're targetting, which is used by logind.
            pam.set_item(PamItemType::TTY, &format!("tty{}", vt))?;
            pam.putenv(&format!("XDG_VTNR={}", vt))?;

            // Opening our target terminal.
            let target_term = terminal::Terminal::open(&path)?;

            // Set the target VT mode to text for compatibility. Other login managers
            // set this to graphics, but that disallows start of textual applications,
            // which greetd aims to support.
            target_term.kd_setmode(terminal::KdMode::Text)?;

            // Clear TTY so that it will be empty when we switch to it.
            target_term.term_clear()?;

            // A bit more work if a VT switch is required.
            if switch && vt != target_term.vt_get_current()? {
                // Perform a switch to the target VT, simultaneously resetting it to
                // VT_AUTO.
                target_term.vt_setactivate(vt)?;
            }

            // Connect std(in|out|err), and make this our controlling TTY.
            target_term.term_connect_pipes()?;
            target_term.term_take_ctty()?;
        }
    }

    // Prepare some values from the user struct we gathered earlier.
    let username = user.name().to_str().unwrap_or("");
    let home = user.home_dir().to_str().unwrap_or("");
    let shell = user.shell().to_str().unwrap_or("");
    let uid = Uid::from_raw(user.uid());
    let gid = Gid::from_raw(user.primary_group_id());

    // Change working directory
    let pwd = match env::set_current_dir(home) {
        Ok(_) => home,
        Err(_) => {
            env::set_current_dir("/")
                .map_err(|e| format!("unable to set working directory: {}", e))?;
            "/"
        }
    };

    // PAM has to be provided a bunch of environment variables before
    // open_session. We pass any environment variables from our greeter
    // through here as well. This allows them to affect PAM (more
    // specifically, pam_systemd.so), as well as make it easier to gather
    // and set all environment variables later.
    let prepared_env = [
        "XDG_SEAT=seat0".to_string(),
        format!("XDG_SESSION_CLASS={}", class),
        format!("USER={}", username),
        format!("LOGNAME={}", username),
        format!("HOME={}", home),
        format!("SHELL={}", shell),
        format!("PWD={}", pwd),
        format!("GREETD_SOCK={}", env::var("GREETD_SOCK").unwrap()),
        format!(
            "TERM={}",
            env::var("TERM").unwrap_or_else(|_| "linux".to_string())
        ),
    ];

    for e in prepared_env.iter() {
        pam.putenv(e)?;
    }

    // Session time!
    pam.open_session(PamFlag::NONE)?;

    // Prepare some strings in C format that we'll need.
    let cusername = CString::new(username)?;
    let command = if source_profile {
        format!(
            "[ -f /etc/profile ] && . /etc/profile; [ -f $HOME/.profile ] && . $HOME/.profile; exec {}",
            cmd.join(" ")
        )
    } else {
        format!("exec {}", cmd.join(" "))
    };

    // Extract PAM environment for use with execve below.
    let pamenvlist = pam.getenvlist()?;
    let envvec = pamenvlist.to_vec();

    // PAM is weird and gets upset if you exec from the process that opened
    // the session, registering it automatically as a log-out. Thus, we must
    // exec in a new child.
    let child = match fork().map_err(|e| format!("unable to fork: {}", e))? {
        ForkResult::Parent { child, .. } => child,
        ForkResult::Child => {
            // It is important that we do *not* return from here by
            // accidentally using '?'. The process *must* exit from within
            // this match arm.

            // Drop privileges to target user
            initgroups(&cusername, gid).expect("unable to init groups");
            setgid(gid).expect("unable to set GID");
            setuid(uid).expect("unable to set UID");

            // Set our parent death signal. setuid/setgid above resets the
            // death signal, which is why we do this here.
            prctl(PrctlOption::SET_PDEATHSIG(libc::SIGTERM)).expect("unable to set death signal");

            // Run
            let cpath = CString::new("/bin/sh").unwrap();
            execve(
                &cpath,
                &[
                    &cpath,
                    &CString::new("-c").unwrap(),
                    &CString::new(command).unwrap(),
                ],
                &envvec,
            )
            .expect("unable to exec");

            unreachable!("after exec");
        }
    };

    // Signal the inner PID to the parent process.
    SessionChildToParent::FinalChildPid(child.as_raw() as u64).send(sock)?;
    sock.shutdown(std::net::Shutdown::Both)?;

    // Set our parent death signal. setsid above resets the signal, hence our
    // late assignment, which is why we do this here.
    prctl(PrctlOption::SET_PDEATHSIG(libc::SIGTERM))?;

    // Wait for process to terminate, handling EINTR as necessary.
    loop {
        match waitpid(child, None) {
            Err(nix::Error::Sys(nix::errno::Errno::EINTR)) => continue,
            Err(e) => {
                eprintln!("session: waitpid on inner child failed: {}", e);
                break;
            }
            Ok(_) => break,
        }
    }

    // Close the session. This step requires root privileges to run, as it
    // will result in various forms of login teardown (including unmounting
    // home folders, telling logind that the session ended, etc.). This is
    // why we cannot drop privileges in this process, but must do it in the
    // inner-most child.
    pam.close_session(PamFlag::NONE)?;
    pam.setcred(PamFlag::DELETE_CRED)?;
    pam.end()?;

    Ok(())
}

pub fn main(sock: &UnixDatagram) -> Result<(), Error> {
    if let Err(e) = worker(sock) {
        SessionChildToParent::Error(e.clone()).send(sock)?;
        Err(e)
    } else {
        Ok(())
    }
}
