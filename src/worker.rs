use std::io;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::{SecondsFormat, Utc};
use crossbeam_channel::{Receiver, Sender};
use log::*;
use snafu::{ResultExt, Snafu};

use crate::config::{OpenStackConfig, Repository, RepositoryConfig, TestConfig};
use crate::openstack;
use crate::pcap_tester;
use crate::remote::{self, Log, Remote};

const PCAP_FILE: &str = "capture.pcap";
const PCAP_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Snafu)]
pub enum TestError {
    #[snafu(display("Failed to fetch CI config: {}", source))]
    FetchRepositoryConfig { source: reqwest::Error },
    #[snafu(display("Failed to parse CI config: {}", source))]
    ConfigError { source: toml::de::Error },
    #[snafu(display("Failed to connect to VM {} ({})", vm, source))]
    ConnectVm {
        vm: &'static str,
        source: remote::Error,
    },
    #[snafu(display("An OpenStack error occurred: {}", source))]
    OpenStack { source: openstack::Error },
    #[snafu(display("Failed to save logs: {}", source))]
    SaveLogs { source: io::Error },
    #[snafu(display("An error occured while performing tests: {}", source))]
    PerformTest {
        source: PerformTestError,
        logs: (Log, Log, Log),
    },
}

#[derive(Debug, Snafu)]
pub enum PerformTestError {
    #[snafu(display("Failed to prepare a VM: {}", source))]
    PrepareVm { source: remote::Error },
    #[snafu(display("An error occurred on a VM: {}", source))]
    RemoteError { source: remote::Error },
    #[snafu(display("pcap test error: {}", source))]
    TestPcap {
        source: pcap_tester::Error,
        pcap: Vec<u8>,
    },
}

#[derive(Debug)]
pub enum Job {
    TestPullRequest {
        repository: Repository,
        fork_user: String,
        fork_branch: String,
        pull_request_id: u64,
    },
    TestBranch {
        repository: Repository,
        branch: String,
    },
    Ping {
        repository: Repository,
        issue_id: u64,
    },
}

pub struct Worker {
    log_directory: PathBuf,
    job_receiver: Receiver<Job>,
    report_sender: Sender<Report>,
    openstack_config: OpenStackConfig,
    test_config: TestConfig,
}

impl Worker {
    pub fn new(
        job_queue_size: usize,
        log_directory: PathBuf,
        openstack_config: OpenStackConfig,
        test_config: TestConfig,
    ) -> (Worker, Sender<Job>, Receiver<Report>) {
        let (job_sender, job_receiver) = crossbeam_channel::bounded(job_queue_size);
        let (report_sender, future_receiver) = crossbeam_channel::unbounded();
        (
            Worker {
                log_directory,
                job_receiver,
                report_sender,
                openstack_config,
                test_config,
            },
            job_sender,
            future_receiver,
        )
    }

    pub fn run(&self) {
        while let Ok(job) = self.job_receiver.recv() {
            match job {
                Job::Ping {
                    repository,
                    issue_id,
                } => {
                    self.report_sender
                        .send(Report {
                            repository,
                            content: ReportContent::Pong { issue_id },
                        })
                        .expect("failed to send report");
                }
                Job::TestBranch { repository, branch } => {
                    info!("Testing branch: {}:{}", repository, branch);
                    let result = self.test_repository(&repository, &branch);
                    self.report_sender
                        .send(Report {
                            repository,
                            content: ReportContent::TestResult {
                                result,
                                test_target: TestTarget::Branch(branch),
                            },
                        })
                        .expect("failed to send report");
                }
                Job::TestPullRequest {
                    repository,
                    fork_user,
                    fork_branch,
                    pull_request_id,
                } => {
                    info!(
                        "Testing pull request: {}'s fork of {} (branch {})",
                        fork_user, repository, fork_branch
                    );
                    let test_repo = Repository {
                        user: fork_user,
                        name: repository.name.clone(),
                    };
                    self.report_sender
                        .send(Report {
                            repository,
                            content: ReportContent::TestResult {
                                result: self.test_repository(&test_repo, &fork_branch),
                                test_target: TestTarget::PullRequest(pull_request_id),
                            },
                        })
                        .expect("failed to send report");
                }
            }
        }
    }

    fn test_repository(
        &self,
        repository: &Repository,
        branch: &str,
    ) -> Result<(Log, Log, Log), TestError> {
        let repo_config = fetch_repo_config(repository, branch)?;

        let cloud = openstack::connect_to_cloud(&self.openstack_config).context(OpenStack)?;
        let (ip_pktgen, ip_fwd, ip_pcap) =
            openstack::spawn_vms(&cloud, &self.openstack_config).context(OpenStack)?;
            // ("138.246.233.100".parse().unwrap(), "138.246.233.95".parse().unwrap(), "138.246.233.105".parse().unwrap());

        let ret = self.test_repository_inner(
            &repo_config,
            repository,
            branch,
            ip_pktgen,
            ip_fwd,
            ip_pcap,
        );

        openstack::clean_environment(&cloud).context(OpenStack)?;

        ret
    }

    fn test_repository_inner(
        &self,
        repo_config: &RepositoryConfig,
        repository: &Repository,
        branch: &str,
        ip_pktgen: IpAddr,
        ip_fwd: IpAddr,
        ip_pcap: IpAddr,
    ) -> Result<(Log, Log, Log), TestError> {
        info!("Using VMs at: {}, {}, {}", ip_pktgen, ip_fwd, ip_pcap);

        trace!("Connecting to pktgen");
        let mut vm_pktgen = Remote::connect(
            (ip_pktgen, 22).into(),
            &self.openstack_config.ssh_login,
            &self.openstack_config.private_key_path,
        )
        .context(ConnectVm { vm: "pktgen" })?;
        trace!("Connecting to fwd");
        let mut vm_fwd = Remote::connect(
            (ip_fwd, 22).into(),
            &self.openstack_config.ssh_login,
            &self.openstack_config.private_key_path,
        )
        .context(ConnectVm { vm: "fwd" })?;
        trace!("Connecting to pcap");
        let mut vm_pcap = Remote::connect(
            (ip_pcap, 22).into(),
            &self.openstack_config.ssh_login,
            &self.openstack_config.private_key_path,
        )
        .context(ConnectVm { vm: "pcap" })?;

        let result = self.perform_test(
            &repository,
            branch,
            &repo_config,
            &mut vm_pktgen,
            &mut vm_fwd,
            &mut vm_pcap,
        );

        // Turn remotes into logs which also closes their connections
        let logs = (vm_pktgen.into_log(), vm_fwd.into_log(), vm_pcap.into_log());

        match result {
            Ok(pcap) => {
                self.save_logs(repository, branch, &logs, &pcap)
                    .context(SaveLogs)?;
                Ok(logs)
            }
            Err(e) => {
                let pcap = if let PerformTestError::TestPcap { pcap, .. } = &e {
                    &pcap[..]
                } else {
                    &[]
                };
                self.save_logs(repository, branch, &logs, &pcap)
                    .context(SaveLogs)?;
                Err(TestError::PerformTest { source: e, logs })
            }
        }
    }

    fn perform_test(
        &self,
        repository: &Repository,
        branch: &str,
        repo_config: &RepositoryConfig,
        vm_pktgen: &mut Remote,
        vm_fwd: &mut Remote,
        vm_pcap: &mut Remote,
    ) -> Result<Vec<u8>, PerformTestError> {
        info!("Preparing VMs");
        prepare_vms(
            &mut [vm_pktgen, vm_fwd, vm_pcap],
            &repo_config.build,
            &repository,
            &branch,
        )
        .context(PrepareVm)?;

        info!("Starting pcap");
        let env = format!(
            "PCI_ADDR_PKTGEN={}; \
             PCI_ADDR_FWD_SRC={}; \
             PCI_ADDR_FWD_DST={}; \
             PCI_ADDR_PCAP={}; \
             PCAP_OUT={}; \
             PCAP_N={}; \
             cd {}",
            self.test_config.pci_addresses.pktgen,
            self.test_config.pci_addresses.fwd_src,
            self.test_config.pci_addresses.fwd_dst,
            self.test_config.pci_addresses.pcap,
            PCAP_FILE,
            self.test_config.packets,
            repository.name
        );

        // Start pcap first, then fwd, and at last pktgen so we dont miss packets
        let mut pcap_cmd = vm_pcap
            .execute_cancellable_command(&format!("sudo {}", repo_config.pcap), &env)
            .context(RemoteError)?;
        let fwd_cmd = vm_fwd
            .execute_cancellable_command(&format!("sudo {}", repo_config.fwd), &env)
            .context(RemoteError)?;
        let pktgen_cmd = vm_pktgen
            .execute_cancellable_command(&format!("sudo {}", repo_config.pktgen), &env)
            .context(RemoteError)?;

        let start_time = Instant::now();
        while pcap_cmd.is_running() {
            if start_time.elapsed() >= PCAP_TIMEOUT {
                error!("pcap timeout");
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        info!("pcap finished in {:?}", start_time.elapsed());

        pcap_cmd.cancel().context(RemoteError)?;
        fwd_cmd.cancel().context(RemoteError)?;
        pktgen_cmd.cancel().context(RemoteError)?;

        let pcap = vm_pcap
            .download_file(Path::new(&format!(
                "/home/{}/{}/{}",
                self.openstack_config.ssh_login, repository.name, PCAP_FILE
            )))
            .context(RemoteError)?;
        pcap_tester::test_pcap(&pcap, self.test_config.packets)
            .context(TestPcap { pcap: pcap.clone() })?;
        info!("pcap test succeeded");

        Ok(pcap)
    }

    fn save_logs(
        &self,
        repository: &Repository,
        branch: &str,
        _logs: &(Log, Log, Log),
        pcap: &[u8],
    ) -> Result<(), io::Error> {
        let prefix = format!(
            "{}__{}__{}__{}",
            repository.user,
            repository.name,
            branch,
            Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
        );
        let log_file = prefix.clone() + ".log";
        let pcap_file = prefix + ".pcap";
        std::fs::write(self.log_directory.join(log_file), "TODO")?; // TODO
        std::fs::write(self.log_directory.join(pcap_file), pcap)
    }
}

fn fetch_repo_config(repository: &Repository, branch: &str) -> Result<RepositoryConfig, TestError> {
    let toml = reqwest::get(&format!(
        "https://raw.githubusercontent.com/{}/{}/ixy-ci.toml",
        repository, branch
    ))
    .and_then(|r| r.error_for_status()?.text())
    .context(FetchRepositoryConfig)?;
    toml::from_str(&toml).context(ConfigError)
}

fn prepare_vms(
    remotes: &mut [&mut Remote],
    setup: &[String],
    repository: &Repository,
    branch: &str,
) -> Result<(), remote::Error> {
    for remote in remotes {
        remote.execute_command("sudo apt update")?;
        remote.execute_command("sudo apt install -y git")?;
        remote.execute_command(&format!(
            "git clone https://github.com/{} --branch {} --single-branch --recurse-submodules",
            repository, branch
        ))?;
        for step in setup {
            remote.execute_command(&format!("cd {} && {}", repository.name, step))?;
        }
        // Required for CancellableCommand atm
        remote.upload_file(
            Path::new("runner/target/release/runner"),
            Path::new("runner"),
            0o777,
        )?;
        remote.execute_command("sudo mv runner /usr/bin/runner")?;
    }
    Ok(())
}

#[derive(Debug)]
pub struct Report {
    pub repository: Repository,
    pub content: ReportContent,
}

#[derive(Debug)]
pub enum ReportContent {
    Pong {
        issue_id: u64,
    },
    TestResult {
        result: Result<(Log, Log, Log), TestError>,
        test_target: TestTarget,
    },
}

#[derive(Debug)]
pub enum TestTarget {
    PullRequest(u64),
    Branch(String),
}
