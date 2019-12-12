use futures::Future;
use hubcaps::comments::CommentOptions;
use hubcaps::Github;
use log::*;

use crate::remote::Log;
use crate::worker::TestError;
use crate::worker::{Report, ReportContent, TestTarget};

pub struct Publisher {
    github: Github,
}

impl Publisher {
    pub fn new(github: Github) -> Publisher {
        Publisher { github }
    }

    pub fn handle_report(&self, report: Report) -> Box<dyn Future<Item = (), Error = ()>> {
        match report.content {
            ReportContent::Pong { issue_id } => Box::new(
                self.github
                    .repo(report.repository.user, report.repository.name)
                    .issues()
                    .get(issue_id)
                    .comments()
                    .create(&CommentOptions {
                        body: "pong".to_string(),
                    })
                    .map_err(|e| error!("Failed to post comment: {:?}", e))
                    .map(|_| {}),
            ),
            ReportContent::TestResult {
                result,
                test_target,
            } => match test_target {
                TestTarget::PullRequest(id) => {
                    info!("Posting result in {}#{}", report.repository, id);
                    Box::new(
                        self.github
                            .repo(report.repository.user, report.repository.name)
                            .issues()
                            .get(id)
                            .comments()
                            .create(&CommentOptions {
                                body: format_pull_request_comment(result),
                            })
                            .map_err(|e| error!("Failed to post comment: {:?}", e))
                            .map(|_| {}),
                    )
                }
                TestTarget::Branch(branch) => {
                    info!(
                        "Test result for branch {} of {}: {}",
                        branch,
                        report.repository,
                        result.is_ok()
                    );
                    if let Err(e) = result {
                        error!("Error: {}", e);
                    }
                    Box::new(futures::future::ok(()))
                }
            },
        }
    }
}

fn format_pull_request_comment(result: Result<(Log, Log, Log), TestError>) -> String {
    match result {
        Ok(logs) => format!("Test __passed__!\n\n{}", format_logs(logs),),
        Err(test_error) => format!(
            "Test __failed__!\n\nCause: {}",
            match test_error {
                TestError::PerformTest { source, logs } => {
                    format!("{}\n\n{}", source, format_logs(logs))
                }
                e => e.to_string(),
            }
        ),
    }
}

fn format_logs(logs: (Log, Log, Log)) -> String {
    format!(
        "{}\n{}\n{}",
        format_log("pktgen", logs.0),
        format_log("fwd", logs.1),
        format_log("pcap", logs.2)
    )
}

fn format_log(name: &str, log: Log) -> String {
    let mut log_content = String::new();
    for (command, output) in log {
        log_content += &format!("$ {}\n{}\n\n", command, output);
    }
    format!(
        "<details><summary>{} logs</summary>\n\n```\n{}\n```\n</details>",
        name,
        log_content.trim()
    )
}
