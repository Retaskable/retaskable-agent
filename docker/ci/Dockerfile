# docs/specs/agent-ci.md section 6: the CI runner (`retaskable-ci`, src/bin/ci.rs) in its own container. Built from
# the repo root at whatever commit is checked out, so approving a new version of the runner is checking that commit
# out and rebuilding (README.md beside this file). Each checked repo's own `.agent-ci/` files are read from its mirror
# at run time and run only in containers with no token.
#   docker compose -f docker/ci/compose.yaml up -d --build
FROM rust:1-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY assets ./assets
COPY prompts ./prompts
RUN cargo build --release --bin retaskable-ci

FROM debian:bookworm-slim
COPY --from=docker:27-cli /usr/local/bin/docker /usr/local/bin/docker
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates git gh && rm -rf /var/lib/apt/lists/*
# The runner's own clones of private repos (outside any check container) authenticate with GH_TOKEN through gh.
RUN git config --system credential.https://github.com.helper '!gh auth git-credential'
COPY --from=build /src/target/release/retaskable-ci /usr/local/bin/retaskable-ci
# The commit it was built from, for `docker inspect` (compose passes `git rev-parse HEAD` when told to).
ARG CI_RUST_COMMIT=unknown
LABEL org.retaskable.ci-rust-commit=$CI_RUST_COMMIT
# The entrypoint finds the host's Docker socket (docker/ci/ci-start.sh, docs/specs/setup-wizard.md section 2.2) and drops from
# root to uid 1000 with the socket's group; run as a non-root user it just runs the command.
COPY docker/ci/ci-start.sh /usr/local/bin/ci-start.sh
RUN chmod +x /usr/local/bin/ci-start.sh
# Runs as uid 1000, the same uid as the agent's `node`, because the two containers share one `retaskable-data` volume
# and what either makes at mode 0600 (secrets.key, credentials) must stay readable to the other. The socket's group is
# added at run time (`group_add: [DOCKER_GID]` in compose.yaml), not here: its gid is the host's.
RUN useradd --uid 1000 --create-home --shell /usr/sbin/nologin ci \
    && mkdir -p /data && chown ci:ci /data
# The shared data folder (this runner's part is /data/ci: mirrors, logs, events), the store link's token file inside
# it, and the data key the agent's page also uses.
ENV RETASKABLE_DATA_DIR=/data \
    RETASKABLE_CI_CREDENTIALS_FILE=/data/ci/credentials.json \
    RETASKABLE_SECRETS_KEY_FILE=/data/secrets.key
# No USER: the entrypoint starts as root only to read the socket's group, then runs the runner as `ci` (uid 1000).
WORKDIR /data
ENTRYPOINT ["/usr/local/bin/ci-start.sh"]
CMD ["retaskable-ci", "run"]
