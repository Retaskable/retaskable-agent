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
# The data folder (mirrors, logs, events) and the store link's token file, both in the `data` volume.
ENV RETASKABLE_DATA_DIR=/data \
    RETASKABLE_CI_CREDENTIALS_FILE=/data/ci-credentials.json \
    RETASKABLE_SECRETS_KEY_FILE=/data/secrets.key
WORKDIR /data
CMD ["retaskable-ci", "run"]
