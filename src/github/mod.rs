pub mod message;

use actix_web::web::{BytesMut, Data, Payload};
use actix_web::{post, Error, HttpRequest, HttpResponse};
use crossbeam_channel::Sender;
use crossbeam_channel::TrySendError;
use futures::future::{self, Either};
use futures::{Future, Stream};
use hubcaps::Github;
use log::*;
use ring::hmac::VerificationKey;
use ring::{digest, hmac};

use crate::config::{self, GitHubConfig};
use crate::worker::Job;
use message::*;

// TODO: Respond with "Sorry dave can't let you do that if @ixy-ci test outside of PR"
// TODO: Improve error handling (make use of actix's ResponseError)

#[post("/webhook")]
fn webhook_service(
    request: HttpRequest,
    payload: Payload,
    config: Data<GitHubConfig>,
    github: Data<Github>,
    job_sender: Data<Sender<Job>>,
) -> impl Future<Item = HttpResponse, Error = Error> {
    payload
        .map_err(Error::from)
        .fold(BytesMut::new(), move |mut body, chunk| {
            body.extend_from_slice(&chunk);
            Ok::<_, Error>(body)
        })
        .and_then(move |body| {
            if let Ok(message) = serde_json::from_slice::<Message>(&body) {
                let repo = message.repository();
                if let Some(webhook_secret) = config.webhook_secrets.get(&repo) {
                    if !check_request(&request, &body, &message, webhook_secret) {
                        Either::A(future::ok(HttpResponse::Unauthorized().finish()))
                    } else {
                        let delivery_id = request
                            .headers()
                            .get("X-GitHub-Delivery")
                            .and_then(|delivery_id| delivery_id.to_str().ok())
                            .unwrap_or("unknown");
                        info!("Processing delivery id {}", delivery_id);

                        Either::B(
                            process_message(
                                message,
                                &config.bot_name,
                                github.get_ref().clone(),
                                job_sender.get_ref().clone(),
                            )
                            .then(|r| match r {
                                Ok(()) => HttpResponse::Ok(),
                                Err(_) => HttpResponse::InternalServerError(),
                            }),
                        )
                    }
                } else {
                    error!("Failed to find webhook secret for {}", repo);
                    Either::A(future::ok(HttpResponse::BadRequest().finish()))
                }
            } else {
                Either::A(future::ok(HttpResponse::BadRequest().finish()))
            }
        })
}

fn process_message(
    message: Message,
    bot_name: &str,
    github: Github,
    job_sender: Sender<Job>,
) -> impl Future<Item = (), Error = Error> {
    let job_future = match message {
        Message::Ping { .. } => Either::B(future::ok(None)),
        Message::IssueComment {
            action,
            repository,
            issue,
            comment,
            ..
        } => {
            if action == IssueCommentAction::Created {
                if comment.body.contains(&format!("@{} test", bot_name)) {
                    Either::A(
                        github
                            .repo(&repository.owner.login, &repository.name)
                            .pulls()
                            .get(issue.number)
                            .get()
                            .map(move |pull| {
                                Some(Job::TestPullRequest {
                                    repository: config::Repository {
                                        user: repository.owner.login,
                                        name: repository.name,
                                    },
                                    fork_user: pull.head.user.login,
                                    fork_branch: pull.head.commit_ref,
                                    pull_request_id: issue.number,
                                })
                            })
                            .map_err(|_| Error::from(())), // TODO: ...
                    )
                } else if comment.body.contains(&format!("@{} ping", bot_name)) {
                    Either::B(future::ok(Some(Job::Ping {
                        repository: config::Repository {
                            user: repository.owner.login,
                            name: repository.name,
                        },
                        issue_id: issue.number,
                    })))
                } else {
                    Either::B(future::ok(None))
                }
            } else {
                Either::B(future::ok(None))
            }
        }
    };
    job_future.map(move |job| {
        info!(
            "Adding new job to queue {:?} (current queue size: {})",
            job,
            job_sender.len(),
        );
        if let Some(job) = job {
            match job_sender.try_send(job) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => error!("Dropping job because queue is full"),
                Err(TrySendError::Disconnected(_)) => panic!("Job queue disconnected"),
            }
        }
    })
}

// This could be rewritten to a proper middleware but that doesn't really seem worth it atm.
fn check_request(
    request: &HttpRequest,
    payload: &[u8],
    message: &Message,
    github_webhook_secret: &str,
) -> bool {
    let headers = request.headers();

    let event = headers
        .get("X-GitHub-Event")
        .map(|event| event == message.github_event())
        .unwrap_or(false);

    if !event {
        error!("X-GitHub-Event didn't match deserialized message");
    }

    let signature = headers
        .get("X-Hub-Signature")
        .and_then(|signature| hex::decode(&signature.as_bytes()[5..]).ok()) // Skip the "sha1=" prefix
        .map(|signature| {
            // ring verifies the hash in constant time
            let v_key = VerificationKey::new(&digest::SHA1, github_webhook_secret.as_bytes());
            hmac::verify(&v_key, payload, &signature).is_ok()
        })
        .unwrap_or(false);

    if !signature {
        error!("Signature check failed");
    }

    event && signature
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_check_request() {
        // TODO
    }
}
