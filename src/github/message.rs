use serde::Deserialize;

use crate::config;

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum Message {
    Ping {
        zen: String,
        hook_id: u64,
        repository: Repository,
    },
    IssueComment {
        action: IssueCommentAction,
        issue: Issue,
        repository: Repository,
        comment: Comment,
    },
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IssueCommentAction {
    Created,
    Edited,
    Deleted,
}

impl Message {
    pub fn github_event(&self) -> &str {
        match self {
            Message::Ping { .. } => "ping",
            Message::IssueComment { .. } => "issue_comment",
        }
    }

    pub fn repository(&self) -> config::Repository {
        match self {
            Message::Ping { repository, .. } => repository,
            Message::IssueComment { repository, .. } => repository,
        }
        .into()
    }
}

#[derive(Debug, Deserialize)]
pub struct Issue {
    pub id: u64,
    pub number: u64,
}

#[derive(Debug, Deserialize)]
pub struct Repository {
    pub name: String,
    pub owner: Owner,
}

#[derive(Debug, Deserialize)]
pub struct Owner {
    pub login: String,
}

#[derive(Debug, Deserialize)]
pub struct Comment {
    pub body: String,
}
