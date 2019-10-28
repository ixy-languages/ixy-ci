FROM debian:buster
EXPOSE 8080
VOLUME /config
RUN apt-get update && apt-get --yes install python-openstackclient libssl1.1 ca-certificates && apt-get clean
COPY target/release/ixy-ci /ixy-ci
COPY runner/target/release/runner /runner-bin
ENV RUST_BACKTRACE 1
ENV RUST_LOG info,ixy_ci=trace
CMD ["/ixy-ci", "--config", "/config/config.toml"]
