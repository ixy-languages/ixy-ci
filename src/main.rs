mod config;
mod github;
mod openstack;
mod pcap_tester;
mod publisher;
mod remote;
mod worker;

use std::{fs, io, thread};

use actix_web::middleware::Logger;
use actix_web::{web, App, HttpServer};
use futures::Stream;
use hubcaps::{Credentials, Github};

use crate::config::Config;
use crate::publisher::Publisher;
use crate::worker::Worker;

fn main() -> io::Result<()> {
    env_logger::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let config = fs::read_to_string("config.toml")?;
    let config: Config = toml::from_str(&config).expect("failed to deserialize config");

    let github = Github::new(
        format!("{}/{}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION")),
        Credentials::Token(config.github.api_token.clone()),
    )
    .expect("failed to initialize GitHub");

    fs::create_dir_all(&config.log_directory).expect("failed to create configured log directory");

    let (worker, job_sender, report_receiver) = Worker::new(
        config.job_queue_size,
        config.log_directory,
        config.openstack,
        config.test,
    );

    // job_sender
    //     .send(worker::Job::TestBranch {
    //         repository: config::Repository {
    //             user: "bobo1239".to_string(),
    //             name: "ixy".to_string(),
    //         },
    //         branch: "ci".to_string(),
    //     })
    //     .unwrap();

    thread::spawn(move || {
        // TODO: Restart on panic
        worker.run();
    });

    let sys = actix_rt::System::new("runtime");

    let publisher = Publisher::new(github.clone());
    actix_rt::spawn(
        futures::stream::iter_ok(report_receiver)
            .for_each(move |report| publisher.handle_report(report)),
    );

    let github_config = config.github;
    HttpServer::new(move || {
        App::new()
            .data(github_config.clone())
            .data(job_sender.clone())
            .data(github.clone())
            .wrap(Logger::default())
            .service(web::scope("/github/").service(github::webhook_service))
    })
    .bind(config.bind_address)?
    .start();

    sys.run()
}
