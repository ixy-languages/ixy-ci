use std::{
    io,
    net::IpAddr,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use chrono::{SecondsFormat, Utc};
use futures::{
    channel::mpsc::{self, Receiver, Sender, UnboundedReceiver, UnboundedSender},
    SinkExt, StreamExt,
};
use log::*;
use snafu::{ResultExt, Snafu};

use crate::{
    config::{OpenStackConfig, Repository, RepositoryConfig, TestConfig},
    openstack,
    openstack::OpenStack,
    pcap_tester,
    remote::{self, Log, Remote},
    utility,
};

const PCAP_FILE: &str = "capture.pcap";
const PCAP_TIMEOUT: Duration = Duration::from_secs(15);

const SSH_MAX_RETRIES: usize = 10;
const SSH_RETRY_DELAY: Duration = Duration::from_secs(5);

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
    OpenStackError { source: openstack::Error },
    #[snafu(display("Failed to save test output: {}", source))]
    SaveTestOutput { source: io::Error },
    #[snafu(display("An error occured while performing tests: {}", source))]
    PerformTest {
        source: PerformTestError,
        test_output: TestOutput,
    },
}

#[derive(Debug, Snafu)]
pub enum PerformTestError {
    #[snafu(display("Failed to prepare a VM: {}", source))]
    PrepareVm { source: remote::Error },
    #[snafu(display("A thread panicked during VM preparation"))]
    ThreadPanicked,
    #[snafu(display("An error occurred on a VM: {}", source))]
    RemoteError { source: remote::Error },
    #[snafu(display("pcap test error: {}", source))]
    TestPcap { source: pcap_tester::Error },
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
    report_sender: UnboundedSender<Report>,
    openstack: OpenStack,
    test_config: TestConfig,
}

impl Worker {
    pub fn new(
        job_queue_size: usize,
        log_directory: PathBuf,
        openstack: OpenStackConfig,
        test_config: TestConfig,
    ) -> (Worker, Sender<Job>, UnboundedReceiver<Report>) {
        let (job_sender, job_receiver) = mpsc::channel(job_queue_size);
        let (report_sender, future_receiver) = mpsc::unbounded();
        (
            Worker {
                log_directory,
                job_receiver,
                report_sender,
                openstack: OpenStack::new(openstack).expect("failed to connect to OpenStack"),
                test_config,
            },
            job_sender,
            future_receiver,
        )
    }

    pub async fn run(&mut self) {
        while let Some(job) = self.job_receiver.next().await {
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
                        .await
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
                        .await
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
                        .await
                        .expect("failed to send report");
                }
            }
        }
    }

    fn test_repository(
        &self,
        repository: &Repository,
        branch: &str,
    ) -> Result<TestOutput, TestError> {
        let repo_config = fetch_repo_config(repository, branch)?;

        let (ip_pktgen, ip_fwd, ip_pcap) = self.openstack.spawn_vms().context(OpenStackError)?;

        let ret = self.test_repository_inner(
            &repo_config,
            repository,
            branch,
            ip_pktgen,
            ip_fwd,
            ip_pcap,
        );

        self.openstack.clean_environment().context(OpenStackError)?;

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
    ) -> Result<TestOutput, TestError> {
        info!("Using VMs at: {}, {}, {}", ip_pktgen, ip_fwd, ip_pcap);

        trace!("Connecting to pktgen");
        let vm_pktgen = utility::retry(SSH_MAX_RETRIES, SSH_RETRY_DELAY, || {
            Remote::connect(
                (ip_pktgen, 22).into(),
                &self.openstack.config.ssh_login,
                &self.openstack.config.private_key_path,
            )
        })
        .context(ConnectVm { vm: "pktgen" })?;

        trace!("Connecting to fwd");
        let vm_fwd = utility::retry(SSH_MAX_RETRIES, SSH_RETRY_DELAY, || {
            Remote::connect(
                (ip_fwd, 22).into(),
                &self.openstack.config.ssh_login,
                &self.openstack.config.private_key_path,
            )
        })
        .context(ConnectVm { vm: "fwd" })?;

        trace!("Connecting to pcap");
        let vm_pcap = utility::retry(SSH_MAX_RETRIES, SSH_RETRY_DELAY, || {
            Remote::connect(
                (ip_pcap, 22).into(),
                &self.openstack.config.ssh_login,
                &self.openstack.config.private_key_path,
            )
        })
        .context(ConnectVm { vm: "pcap" })?;

        let mut context = TestContext {
            vm_pktgen,
            vm_fwd,
            vm_pcap,
            pcap: None,
        };
        let result = self.perform_test(&repository, branch, &repo_config, &mut context);

        let test_output = self
            .save_test_output(repository, branch, context)
            .context(SaveTestOutput)?;

        match result {
            Ok(_) => Ok(test_output),
            Err(e) => Err(e).context(PerformTest { test_output }),
        }
    }

    fn perform_test(
        &self,
        repository: &Repository,
        branch: &str,
        repo_config: &RepositoryConfig,
        context: &mut TestContext,
    ) -> Result<(), PerformTestError> {
        info!("Preparing VMs");
        prepare_vms(
            &mut [
                &mut context.vm_pktgen,
                &mut context.vm_fwd,
                &mut context.vm_pcap,
            ],
            &repo_config.build,
            &repository,
            &branch,
        )?;

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
        let mut pcap_cmd = context
            .vm_pcap
            .execute_cancellable_command(&format!("sudo {}", repo_config.pcap), &env)
            .context(RemoteError)?;
        let fwd_cmd = context
            .vm_fwd
            .execute_cancellable_command(&format!("sudo {}", repo_config.fwd), &env)
            .context(RemoteError)?;
        let pktgen_cmd = context
            .vm_pktgen
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

        let pcap = context
            .vm_pcap
            .download_file(Path::new(&format!(
                "/home/{}/{}/{}",
                self.openstack.config.ssh_login, repository.name, PCAP_FILE
            )))
            .context(RemoteError)?;
        context.pcap = Some(pcap);

        pcap_tester::test_pcap(&context.pcap.as_ref().unwrap(), self.test_config.packets)
            .context(TestPcap)?;
        info!("pcap test succeeded");

        Ok(())
    }

    fn save_test_output(
        &self,
        repository: &Repository,
        branch: &str,
        context: TestContext,
    ) -> Result<TestOutput, io::Error> {
        let file_name = format!(
            "{}__{}__{}__{}",
            repository.user,
            repository.name,
            branch,
            Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
        );
        let log_file = file_name.clone() + ".log";
        std::fs::write(self.log_directory.join(&log_file), "TODO")?; // TODO
        let pcap_file = context
            .pcap
            .map(|pcap| -> Result<_, io::Error> {
                let pcap_file = file_name + ".pcap";
                std::fs::write(self.log_directory.join(&pcap_file), pcap)?;
                Ok(pcap_file)
            })
            .transpose()?;
        Ok(TestOutput {
            log_pktgen: context.vm_pktgen.into_log(),
            log_fwd: context.vm_fwd.into_log(),
            log_pcap: context.vm_pcap.into_log(),
            log_file,
            pcap_file,
        })
    }
}

fn fetch_repo_config(repository: &Repository, branch: &str) -> Result<RepositoryConfig, TestError> {
    // We're forced to use the blocking Client atm since openstack tries to spawn it's own tokio
    // runtime (it's not async/await compatible yet) which would conflict with anyone we're
    // creating. But without a tokio runtime async reqwest doesn't work...
    let response = reqwest::blocking::get(&format!(
        "https://raw.githubusercontent.com/{}/{}/ixy-ci.toml",
        repository, branch
    ))
    .and_then(|r| Ok(r.error_for_status()?))
    .context(FetchRepositoryConfig)?;
    let toml = response.text().context(FetchRepositoryConfig)?;
    toml::from_str(&toml).context(ConfigError)
}

fn prepare_vms(
    remotes: &mut [&mut Remote],
    setup: &[String],
    repository: &Repository,
    branch: &str,
) -> Result<(), PerformTestError> {
    // Perform VM initialization concurrently
    let results = crossbeam_utils::thread::scope(|s| {
        let mut join_handles = Vec::new();
        for remote in remotes {
            let handle = s.spawn(move |_| {
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
                remote.upload_file(Path::new("runner-bin"), Path::new("runner"), 0o777)?;
                remote.execute_command("sudo mv runner /usr/bin/runner")?;
                Ok(())
            });
            join_handles.push(handle);
        }
        let mut results = Vec::new();
        for handle in join_handles {
            let result = handle
                .join()
                .map_err(|_| PerformTestError::ThreadPanicked)?
                .context(PrepareVm);
            results.push(result);
        }
        Ok(results)
    })
    .map_err(|_| PerformTestError::ThreadPanicked)??;

    // Transform Vec<Result> to Result<Vec>
    results
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .map(|_| ())
}

pub struct TestContext {
    pub vm_pktgen: Remote,
    pub vm_fwd: Remote,
    pub vm_pcap: Remote,
    pub pcap: Option<Vec<u8>>,
}

#[derive(Debug)]
pub struct TestOutput {
    pub log_pktgen: Log,
    pub log_fwd: Log,
    pub log_pcap: Log,

    pub log_file: String,
    pub pcap_file: Option<String>,
}

#[derive(Debug)]
pub struct Report {
    pub repository: Repository,
    pub content: ReportContent,
}

#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum ReportContent {
    Pong {
        issue_id: u64,
    },
    TestResult {
        result: Result<TestOutput, TestError>,
        test_target: TestTarget,
    },
}

#[derive(Debug)]
pub enum TestTarget {
    PullRequest(u64),
    Branch(String),
}
