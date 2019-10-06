use std::fs::File;
use std::io::{self, ErrorKind, Read};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;

use log::*;
use snafu::{ensure, ResultExt, Snafu};
use ssh2::{Channel, ExtendedData, Session};

// TODO: Probably want to do this more `struct`ured
// TODO: Add time to log
pub type Log = Vec<(String, String)>;

#[derive(Debug, Snafu)]
pub enum Error {
    Ssh { source: ssh2::Error },
    Io { source: io::Error },
    NonZeroReturn { command: String },
}

pub struct Remote {
    session: Session,
    log: Log,
}

impl Remote {
    pub fn connect(
        socket_addr: SocketAddr,
        user: &str,
        private_key_file: &Path,
    ) -> Result<Remote, Error> {
        let tcp = TcpStream::connect(socket_addr).context(Io)?;
        let mut session = Session::new().context(Ssh)?;
        session.set_tcp_stream(tcp);
        session.handshake().context(Ssh)?;
        session
            .userauth_pubkey_file(user, None, private_key_file, None)
            .context(Ssh)?;
        Ok(Remote {
            session,
            log: Vec::new(),
        })
    }

    /// Executes a command on the remote. This blocks until the command finishes and the whole
    /// output was read. The command is executed by the default shell on the remote (probably bash)
    /// so commands like `echo 123 && echo abc` are valid.
    pub fn execute_command(&mut self, command: &str) -> Result<(), Error> {
        self.log.push((command.to_string(), String::new()));

        let mut channel = self.session.channel_session().context(Ssh)?;

        // Merge stderr output into default stream
        // We may want to do this more granularly in the future
        channel
            .handle_extended_data(ExtendedData::Merge)
            .context(Ssh)?;

        debug!("executing command: {}", command);
        channel.exec(command).context(Ssh)?;

        let mut output = String::new();
        channel.read_to_string(&mut output).context(Io)?;
        channel.wait_close().context(Ssh)?;

        // We pushed to log at the start so this can't fail
        self.log.last_mut().unwrap().1 = output;
        ensure!(
            channel.exit_status().context(Ssh)? == 0,
            NonZeroReturn { command }
        );
        Ok(())
    }

    // TODO: This currently behaves differently than the normal `execute_command` due to the runner
    //       implementation detail (bash isn't used for execution). That's also the reason for the
    //       separate (unergonomic) `env` parameter.
    pub fn execute_cancellable_command(
        &mut self,
        command: &str,
        env: &str,
    ) -> Result<CancellableCommand, Error> {
        // TODO: Would like to just send signals over ssh which is actually part of the SSH
        //       specification; Unfortunately nobody implemented that part for a long time and
        //       OpenSSH just did so recently:
        //       https://github.com/openssh/openssh-portable/commit/cd98925c6405e972dc9f211afc7e75e838abe81c
        //       The current deployed OpenSSH version doesn't contain that commit yet and neither
        //       does libssh2 support it so we we're stuck with this for now...
        // NOTE: Currently we rely on a helper binary (see runner) to achieve this
        // NOTE: libssh2's setenv doesn't work here as that would try to set the environment of the
        //       remote ssh handler which is disabled by default in sshd.conf:
        //       https://serverfault.com/questions/427522/why-is-acceptenv-considered-insecure

        self.log.push((command.to_string(), String::new()));

        let mut channel = self.session.channel_session().context(Ssh)?;

        channel
            .handle_extended_data(ExtendedData::Merge)
            .context(Ssh)?;

        // Old solution without additional binary
        // let command = format!("{} & read -t {}; kill $!", command, timeout_secs);

        // Have to start runner with sudo to be able to kill sudo'ed children
        let command = format!("{}; sudo runner {}", env, command);
        debug!("Executing cancellable command: {}", command);
        channel.exec(&command).context(Ssh)?;

        Ok(CancellableCommand {
            channel,
            log: &mut self.log,
            session: &mut self.session,
        })
    }

    pub fn upload_file(
        &mut self,
        local_path: &Path,
        remote_path: &Path,
        mode: i32,
    ) -> Result<(), Error> {
        debug!(
            "Uploading file: {} -> {}",
            local_path.display(),
            remote_path.display()
        );
        let mut local_file = File::open(local_path).context(Io)?;
        let size = local_file.metadata().context(Io)?.len();
        let mut remote_file = self
            .session
            .scp_send(remote_path, mode, size, None)
            .context(Ssh)?;
        io::copy(&mut local_file, &mut remote_file).context(Io)?;
        Ok(())
    }

    pub fn download_file(&mut self, remote_path: &Path) -> Result<Vec<u8>, Error> {
        debug!("Downloading file {}", remote_path.display());
        let (mut remote_file, stat) = self.session.scp_recv(remote_path).context(Ssh)?;
        let mut contents = Vec::with_capacity(stat.size() as usize);
        remote_file.read_to_end(&mut contents).context(Io)?;
        Ok(contents)
    }

    pub fn into_log(self) -> Log {
        self.log
    }
}

pub struct CancellableCommand<'a> {
    channel: Channel,
    session: &'a mut Session,
    log: &'a mut Log,
}

impl CancellableCommand<'_> {
    pub fn is_running(&mut self) -> bool {
        // TODO: This feels like a horrible hack but I'm unable to find another API for this...
        self.session.set_blocking(false);
        let mut buf = [];
        let mut is_running = false;
        if let Err(e) = self.channel.read(&mut buf) {
            if e.kind() == ErrorKind::WouldBlock {
                is_running = true;
            }
        }
        self.session.set_blocking(true);
        is_running
    }

    pub fn cancel(mut self) -> Result<(), Error> {
        // Close stdin which causes runner to kill the command
        self.channel.send_eof().context(Ssh)?;

        let mut output = String::new();
        self.channel.read_to_string(&mut output).context(Io)?;
        self.channel.wait_close().context(Ssh)?;

        // We pushed to log at the start so this can't fail
        self.log.last_mut().unwrap().1 = output;
        Ok(())
    }
}
