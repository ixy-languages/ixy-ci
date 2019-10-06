# ixy-ci

TODO: README

test idea:
pktgen -> fwd -> pcap
maybe use in conjunction with traditional CI to check build/formatting etc.

## ixy-ci setup instructions
ixy-ci requires some 
### OpenStack
### `config.toml`
See config.toml.example
How to query project domain
### `clouds.yaml`
Needed for now because we use openstack cli
TODO: also mention openstack cli requirement
### GitHub
#### Webhook
    url: <base>/github/webhook
    Content type: application/json
    Secret: -> .env (e.g. `openssl rand -base64 48`)
    Events: Issue comments (+ branch push for future?)
#### Bot account
    scope: public_repo
    how to get api token
### Runner
compile runner --release first! (also strip to minimize size)

## How to add a new repository to test
### Required command line interface of applications

## TODO list
- Only allow configured users to start test (prevent abuse)
- Do more stuff concurrently once async/await is ready (also trussh instead of libssh2)
- Make logs available
- ctrl+c graceful shutdown
- ctrl+c lock up after a comment has been posted
- Doccomments

## Future feature plans
- Test on master branch push (also cronjob?) + Badges (https://img.shields.io/badge/ixy--ci-success-success)
- Abstract VM Provider so also locally runnable
- Integration with github checks api
- Ability to reload parts of config during runtime
- Dashboard with queue + past results
