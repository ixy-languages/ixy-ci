pub mod message;

use actix_web::{
    post,
    web::{BytesMut, Data, Payload},
    Error, HttpRequest, HttpResponse,
};
use futures::{channel::mpsc::Sender, StreamExt};
use hubcaps::Github;
use log::*;
use ring::hmac::{self, Key, HMAC_SHA256};

use crate::{
    config::{self, GitHubConfig},
    worker::Job,
};
use message::*;

// TODO: Respond with "Sorry dave can't let you do that if @ixy-ci test outside of PR"
// TODO: Improve error handling (make use of actix's ResponseError)

#[post("/webhook")]
async fn webhook_service(
    request: HttpRequest,
    mut payload: Payload,
    config: Data<GitHubConfig>,
    github: Data<Github>,
    job_sender: Data<Sender<Job>>,
) -> Result<HttpResponse, Error> {
    let mut body = BytesMut::new();
    while let Some(item) = payload.next().await {
        body.extend_from_slice(&item?);
    }

    let mut response = if let Ok(message) = serde_json::from_slice::<Message>(&body) {
        let repo = message.repository();
        if let Some(webhook_secret) = config.webhook_secrets.get(&repo) {
            if !check_request(&request, &body, &message, webhook_secret) {
                HttpResponse::Unauthorized()
            } else {
                let delivery_id = request
                    .headers()
                    .get("X-GitHub-Delivery")
                    .and_then(|delivery_id| delivery_id.to_str().ok())
                    .unwrap_or("unknown");
                info!("Processing delivery id {}", delivery_id);

                let result = process_message(
                    message,
                    &config.bot_name,
                    github.get_ref().clone(),
                    job_sender.get_ref().clone(),
                )
                .await;
                match result {
                    Ok(()) => HttpResponse::Ok(),
                    Err(_) => HttpResponse::InternalServerError(),
                }
            }
        } else {
            error!("Failed to find webhook secret for {}", repo);
            HttpResponse::BadRequest()
        }
    } else {
        HttpResponse::BadRequest()
    };

    Ok(response.finish())
}

async fn process_message(
    message: Message,
    bot_name: &str,
    github: Github,
    mut job_sender: Sender<Job>,
) -> Result<(), Error> {
    let job = match message {
        Message::Ping { .. } => None,
        Message::IssueComment {
            action,
            repository,
            issue,
            comment,
            ..
        } => {
            if action == IssueCommentAction::Created {
                if comment.body.contains(&format!("@{} test", bot_name)) {
                    github
                        .repo(&repository.owner.login, &repository.name)
                        .pulls()
                        .get(issue.number)
                        .get()
                        .await
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
                        .map_err(|_| Error::from(()))? // TODO: ...
                } else if comment.body.contains(&format!("@{} ping", bot_name)) {
                    Some(Job::Ping {
                        repository: config::Repository {
                            user: repository.owner.login,
                            name: repository.name,
                        },
                        issue_id: issue.number,
                    })
                } else {
                    None
                }
            } else {
                None
            }
        }
    };
    if let Some(job) = job {
        info!("Adding new job to queue {:?}", job,);
        match job_sender.try_send(job) {
            Ok(()) => {}
            Err(e) if e.is_full() => error!("Dropping job because queue is full"),
            Err(e) if e.is_disconnected() => panic!("Job queue disconnected"),
            Err(e) => error!("Unknown try_send error: {:?}", e),
        }
    }
    Ok(())
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
        .get("X-Hub-Signature-256")
        .and_then(|signature| hex::decode(&signature.as_bytes()[7..]).ok()) // Skip the "sha256=" prefix
        .map(|signature| {
            // ring verifies the hash in constant time
            let key = Key::new(HMAC_SHA256, github_webhook_secret.as_bytes());
            hmac::verify(&key, payload, &signature).is_ok()
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

    use actix_web::test::TestRequest;

    #[test]
    fn test_check_request() {
        let payload = br#"
        {
            "zen": "Speak like a human.",
            "hook_id": 1239,
            "repository": {
                "name": "ixy.rs",
                "owner": {
                    "login": "Bobo1239"
                }
            }
        }"#;
        let message = serde_json::from_slice::<Message>(payload).unwrap();
        let request = TestRequest::get()
            .header("X-GitHub-Event", "ping")
            .header(
                "X-Hub-Signature-256",
                "sha256=bbe4db51aa010a37d2ed857abca415f5bea80550a6b4dd04e86194725d3041c8",
            )
            .set_payload(payload.as_ref())
            .to_http_request();

        assert!(check_request(&request, payload, &message, "foobar"))
    }
}
